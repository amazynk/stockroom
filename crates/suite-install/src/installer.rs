//! Install, update, rollback and remove (PLAN.md §4).
//!
//! Each version lives in its own folder (`apps/<id>/<version>`) and a pointer names the live
//! one, so an update never writes into files a running app uses: it stages the new version
//! next to the old, commits the state file, then switches the pointer. A crash at any point
//! is repaired by [`Installer::recover`], which every operation runs first.

use std::{
    collections::BTreeSet,
    fs::File,
    path::{Path, PathBuf},
};

use suite_catalog::{App, Asset, AssetKind, Channel, Layout, Os, Release};
use suite_fetch::{Expected, Fetcher, Progress};
use suite_platform::Env;
use suite_unpack::Limits;
use time::OffsetDateTime;

use crate::{
    InstallError,
    integrate::{self, Target},
    pointer,
    state::{AppState, InstallKind, Scope, State},
};

/// Points where a test can make an install stop as if the machine crashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failpoint {
    AfterDownload,
    AfterUnpack,
    AfterCommit,
    AfterSwitch,
}

impl std::str::FromStr for Failpoint {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "after-download" => Ok(Self::AfterDownload),
            "after-unpack" => Ok(Self::AfterUnpack),
            "after-commit" => Ok(Self::AfterCommit),
            "after-switch" => Ok(Self::AfterSwitch),
            other => Err(format!("unknown failpoint {other:?}")),
        }
    }
}

/// Steps of an install, for progress display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Download,
    Unpack,
    Switch,
    Integrate,
    Cleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Stage(Stage),
    Download(Progress),
}

/// The result of an install, update or rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub app: String,
    pub from: Option<semver::Version>,
    pub to: semver::Version,
    /// The app is running; the new version starts with its next launch.
    pub needs_restart: bool,
    /// Already at this version; nothing was done.
    pub unchanged: bool,
    /// Not installed by Stockroom: this store's page was opened instead, and the store
    /// installs and updates the app.
    pub opened_store: Option<suite_catalog::Store>,
}

#[derive(Debug)]
pub struct Installer {
    env: Env,
    fetcher: Fetcher,
    limits: Limits,
    /// The running Stockroom. The program the apps' uninstall entries name, and the launcher
    /// shim, are found from it when an app is integrated ([`Installer::suite_files`]), so a
    /// copy of Stockroom kept since this one started is the one they name.
    running: PathBuf,
    /// Where the Windows launcher shim comes from.
    shim: Shim,
    failpoint: Option<Failpoint>,
    abort_at_failpoint: bool,
    lock_wait: std::time::Duration,
}

/// Where the Windows launcher shim comes from.
#[derive(Debug, Clone)]
enum Shim {
    /// Beside the running Stockroom, or beside its kept copy.
    Beside,
    /// Set with [`Installer::with_shim`] (tests, and installs from a build folder).
    Given(Option<PathBuf>),
}

/// Held while an operation runs; a second Stockroom process gets [`InstallError::Busy`].
#[derive(Debug)]
struct Lock {
    _file: File,
}

impl Installer {
    pub fn new(env: Env, fetcher: Fetcher) -> Self {
        let running = std::env::current_exe().unwrap_or_else(|_| PathBuf::from(suite_identity::SLUG));
        Self {
            env,
            fetcher,
            limits: Limits::default(),
            running,
            shim: Shim::Beside,
            failpoint: None,
            abort_at_failpoint: false,
            lock_wait: std::time::Duration::from_secs(3),
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Stops at `fp` with [`InstallError::Failpoint`], or aborts the process when `abort`
    /// (a real crash, for tests that run the binary).
    #[must_use]
    pub fn with_failpoint(mut self, fp: Option<Failpoint>, abort: bool) -> Self {
        self.failpoint = fp;
        self.abort_at_failpoint = abort;
        self
    }

    /// Uses `shim` as the Windows launcher shim (tests, and installs from a build folder).
    #[must_use]
    pub fn with_shim(mut self, shim: Option<PathBuf>) -> Self {
        self.shim = Shim::Given(shim);
        self
    }

    /// What the apps' uninstall entries name, and the Windows launcher shim to copy next to
    /// each app: Stockroom's kept copy once there is one, which outlives the download this
    /// may be running from, and the shim beside Stockroom (`None` when there is none, e.g.
    /// in a development build).
    fn suite_files(&self) -> (PathBuf, Option<PathBuf>) {
        let suite_exe = crate::settle::registered_exe(&self.env, &self.running);
        let shim = match &self.shim {
            Shim::Given(shim) => shim.clone(),
            Shim::Beside => {
                let beside = self.running.with_file_name(integrate::shim_file_name());
                let shim = if beside.is_file() {
                    beside
                } else {
                    suite_exe.with_file_name(integrate::shim_file_name())
                };
                shim.is_file().then_some(shim)
            }
        };
        (suite_exe, shim)
    }

    /// How long to wait for another operation to finish before giving up with
    /// [`InstallError::Busy`].
    #[must_use]
    pub fn with_lock_wait(mut self, wait: std::time::Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    pub fn env(&self) -> &Env {
        &self.env
    }

    pub fn fetcher(&self) -> &Fetcher {
        &self.fetcher
    }

    fn checkpoint(&self, fp: Failpoint) -> Result<(), InstallError> {
        if self.failpoint == Some(fp) {
            if self.abort_at_failpoint {
                tracing::error!(?fp, "failpoint: aborting");
                std::process::abort();
            }
            return Err(InstallError::Failpoint(fp));
        }
        Ok(())
    }

    fn lock(&self) -> Result<Lock, InstallError> {
        let data = &self.env.paths.data;
        std::fs::create_dir_all(data).map_err(|e| InstallError::io(data, e))?;
        let path = data.join("lock");
        let f = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| InstallError::io(&path, e))?;
        // Short waits cover a background run finishing, and a forked child briefly sharing
        // the lock's file description before it execs.
        let deadline = std::time::Instant::now() + self.lock_wait;
        loop {
            match f.try_lock() {
                Ok(()) => return Ok(Lock { _file: f }),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(std::fs::TryLockError::WouldBlock) => return Err(InstallError::Busy),
                Err(std::fs::TryLockError::Error(e)) => return Err(InstallError::io(&path, e)),
            }
        }
    }

    /// The current state (read without the lock; for display).
    pub fn state(&self) -> Result<State, InstallError> {
        State::load(&self.env.paths.state_file)
    }

    /// Changes the state under the lock (settings, policies, catalog bookkeeping).
    pub fn update_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> Result<T, InstallError> {
        let _lock = self.lock()?;
        let mut state = State::load(&self.env.paths.state_file)?;
        let out = f(&mut state);
        state.save(&self.env.paths.state_file)?;
        Ok(out)
    }

    fn app_dir(&self, id: &str) -> PathBuf {
        self.env.paths.apps.join(id)
    }

    fn running_pids(&self, dir: &Path) -> Vec<u32> {
        if !dir.exists() {
            return Vec::new();
        }
        self.env.processes.running_under(dir).into_iter().map(|p| p.pid).collect()
    }

    /// Installs `release` of `app` using `asset`, or updates to it.
    // One transaction, kept in one place so its order (stage, commit, switch) reads top
    // to bottom.
    #[allow(clippy::too_many_lines)]
    pub fn install(
        &self,
        app: &App,
        release: &Release,
        asset: &Asset,
        channel: Channel,
        on: &mut dyn FnMut(Event),
    ) -> Result<Outcome, InstallError> {
        let _lock = self.lock()?;
        let sf = &self.env.paths.state_file;
        let mut state = State::load(sf)?;
        self.recover_locked(&mut state)?;

        if asset.kind == AssetKind::Store {
            return self.open_store(app, release, asset);
        }
        let kind = self.kind_for(asset)?;
        let version = release.version.clone();
        let existing = state.apps.get(&app.id).cloned();
        if let Some(e) = &existing {
            if e.kind != kind {
                return Err(InstallError::Unsupported(format!(
                    "{} is installed as {:?}; remove it before switching to {:?}",
                    app.id, e.kind, kind
                )));
            }
            if e.current == version {
                return Ok(Outcome {
                    app: app.id.clone(),
                    from: Some(version.clone()),
                    to: version,
                    needs_restart: false,
                    unchanged: true,
                    opened_store: None,
                });
            }
        }

        // Download (resumable) and verify.
        on(Event::Stage(Stage::Download));
        let dl = self.env.paths.downloads.join(format!("{}-{}.{}", app.id, version, asset.kind));
        let sha256 = asset.sha256.clone().ok_or_else(|| {
            InstallError::Unsupported(format!("{}: the catalog has no checksum for this download", app.name))
        })?;
        let expected = Expected { size: asset.size, sha256 };
        let reuse = dl.is_file() && suite_fetch::sha256_file(&dl).is_ok_and(|h| h == expected.sha256);
        if !reuse {
            let fetched = self.fetcher.download(&asset.url, &dl, &expected, &mut |p| on(Event::Download(p)));
            match fetched {
                // A pinned "latest" URL that no longer serves the pinned bytes: the vendor
                // shipped a new build. Nothing unverified is installed.
                Err(e) if asset.pinned && pinned_moved(&e) => {
                    tracing::warn!(app = %app.id, error = %e, "pinned download changed");
                    return Err(InstallError::PinnedChanged { app: app.name.clone() });
                }
                other => other?,
            }
        }
        self.checkpoint(Failpoint::AfterDownload)?;

        let now = OffsetDateTime::now_utc();
        let from = existing.as_ref().map(|e| e.current.clone());
        let desktop_id =
            app.ids.desktop.clone().unwrap_or_else(|| format!("{}.{}", suite_identity::APP_ID, app.id));
        let bin_name = app.ids.executables.first().cloned().unwrap_or_else(|| app.id.clone());

        if kind == InstallKind::Flatpak {
            on(Event::Stage(Stage::Unpack));
            suite_unpack::flatpak::install_bundle(self.env.runner.as_ref(), &dl)?;
            let st = AppState {
                name: app.name.clone(),
                kind,
                channel,
                policy: existing.as_ref().map_or_default(|e| e.policy),
                current: version.clone(),
                previous: None,
                versions: Vec::new(),
                executable: None,
                flatpak_id: app.ids.flatpak.clone().or_else(|| app.ids.desktop.clone()),
                desktop_id: Some(desktop_id),
                bin_name: None,
                pitch: app.pitch.clone(),
                layouts: std::collections::BTreeMap::new(),
                installed: existing.as_ref().map_or(now, |e| e.installed),
                updated: now,
                created: Vec::new(),
                associate: false,
            };
            state.apps.insert(app.id.clone(), st);
            state.save(sf)?;
            let _ = std::fs::remove_file(&dl);
            return Ok(Outcome {
                app: app.id.clone(),
                from,
                to: version,
                needs_restart: false,
                unchanged: false,
                opened_store: None,
            });
        }

        // Stage the new version next to the live one.
        let app_dir = self.app_dir(&app.id);
        let version_dir = app_dir.join(version.to_string());
        let mut exe_rel = asset.layout.executable.clone();
        let on_disk =
            existing.as_ref().is_some_and(|e| e.versions.contains(&version)) && version_dir.is_dir();
        if !on_disk {
            on(Event::Stage(Stage::Unpack));
            let staged = self.unpack(&app.id, &version, asset, &dl, &app_dir, &mut exe_rel)?;
            if version_dir.exists() {
                std::fs::remove_dir_all(&version_dir).map_err(|e| InstallError::io(&version_dir, e))?;
            }
            std::fs::rename(&staged, &version_dir).map_err(|e| InstallError::io(&version_dir, e))?;
            let staging = app_dir.join(format!(".staging-{version}"));
            if staging.exists() {
                let _ = std::fs::remove_dir_all(&staging);
            }
        }
        if let Some(exe) = &exe_rel
            && !integrate::join_rel(&version_dir, exe).exists()
        {
            let _ = std::fs::remove_dir_all(&version_dir);
            return Err(InstallError::LayoutMissing(exe.clone()));
        }
        if kind == InstallKind::MacBundle
            && let Some(bundle) = &exe_rel
            && let Err(e) = Self::check_bundle_id(app, &integrate::join_rel(&version_dir, bundle))
        {
            if !existing.as_ref().is_some_and(|e| e.versions.contains(&version)) {
                let _ = std::fs::remove_dir_all(&version_dir);
            }
            return Err(e);
        }
        self.checkpoint(Failpoint::AfterUnpack)?;

        // Commit: the state names the new version. Recovery makes the disk match it.
        let mut layouts = existing.as_ref().map_or_default(|e| e.layouts.clone());
        let mut layout = asset.layout.clone();
        layout.executable.clone_from(&exe_rel);
        layouts.insert(version.to_string(), layout);
        let mut versions: Vec<semver::Version> = existing.as_ref().map_or_default(|e| e.versions.clone());
        if !versions.contains(&version) {
            versions.push(version.clone());
        }
        let mut st = AppState {
            name: app.name.clone(),
            kind,
            channel,
            policy: existing.as_ref().map_or_default(|e| e.policy),
            current: version.clone(),
            previous: from.clone().filter(|f| *f != version),
            versions,
            executable: exe_rel,
            flatpak_id: None,
            desktop_id: Some(desktop_id),
            bin_name: Some(bin_name),
            pitch: app.pitch.clone(),
            layouts,
            installed: existing.as_ref().map_or(now, |e| e.installed),
            updated: now,
            created: existing.as_ref().map_or_default(|e| e.created.clone()),
            associate: existing.as_ref().is_some_and(|e| e.associate),
        };
        state.apps.insert(app.id.clone(), st.clone());
        state.save(sf)?;
        self.checkpoint(Failpoint::AfterCommit)?;

        on(Event::Stage(Stage::Switch));
        pointer::set(self.env.os, &app_dir, &version, st.executable.as_deref())?;
        self.checkpoint(Failpoint::AfterSwitch)?;

        on(Event::Stage(Stage::Integrate));
        self.reintegrate(&app.id, &mut st, state.settings.scope)?;
        let needs_restart = from.is_some() && !self.running_pids(&app_dir).is_empty();

        on(Event::Stage(Stage::Cleanup));
        self.prune(&app_dir, &mut st);
        state.apps.insert(app.id.clone(), st);
        state.save(sf)?;
        let _ = std::fs::remove_file(&dl);
        tracing::info!(app = %app.id, %version, ?from, needs_restart, "installed");
        Ok(Outcome {
            app: app.id.clone(),
            from,
            to: version,
            needs_restart,
            unchanged: false,
            opened_store: None,
        })
    }

    /// Opens the store page of an app the store installs and updates.
    fn open_store(&self, app: &App, release: &Release, asset: &Asset) -> Result<Outcome, InstallError> {
        let store = asset.store.as_ref().ok_or_else(|| {
            InstallError::Unsupported(format!("{}: store asset without a store link", app.name))
        })?;
        let opener = match self.env.os {
            Os::Windows => "explorer.exe",
            Os::Macos => "open",
            Os::Linux => "xdg-open",
        };
        let out = self
            .env
            .runner
            .run(opener, &suite_platform::runner::args([store.link.as_str()]))
            .map_err(|e| InstallError::Unsupported(format!("opening the {}: {e}", store.store.name())))?;
        // explorer.exe exits with 1 even when it opened the link.
        if !out.success && self.env.os != Os::Windows {
            return Err(InstallError::Unsupported(format!(
                "opening the {} failed: {}",
                store.store.name(),
                out.stderr.trim()
            )));
        }
        tracing::info!(app = %app.id, store = store.store.name(), "opened the store page");
        Ok(Outcome {
            app: app.id.clone(),
            from: None,
            to: release.version.clone(),
            needs_restart: false,
            unchanged: false,
            opened_store: Some(store.store),
        })
    }

    fn kind_for(&self, asset: &Asset) -> Result<InstallKind, InstallError> {
        let bundle = asset
            .layout
            .executable
            .as_deref()
            .is_some_and(|e| Path::new(e).extension().is_some_and(|x| x.eq_ignore_ascii_case("app")));
        Ok(match asset.kind {
            AssetKind::TarGz | AssetKind::Zip if self.env.os == Os::Macos && bundle => InstallKind::MacBundle,
            AssetKind::TarGz | AssetKind::Zip => InstallKind::Archive,
            AssetKind::AppImage => InstallKind::AppImage,
            AssetKind::FlatpakBundle => InstallKind::Flatpak,
            AssetKind::Dmg => InstallKind::MacBundle,
            AssetKind::Msi => {
                return Err(InstallError::Unsupported("MSI packages need an admin install".into()));
            }
            AssetKind::Store => {
                return Err(InstallError::Unsupported("store apps are installed by their store".into()));
            }
        })
    }

    /// Unpacks into `apps/<id>/.staging-<version>` and returns the folder to rename into
    /// place (the single top folder of an archive, if it has one).
    fn unpack(
        &self,
        id: &str,
        version: &semver::Version,
        asset: &Asset,
        dl: &Path,
        app_dir: &Path,
        exe_rel: &mut Option<String>,
    ) -> Result<PathBuf, InstallError> {
        let staging = app_dir.join(format!(".staging-{version}"));
        if staging.exists() {
            std::fs::remove_dir_all(&staging).map_err(|e| InstallError::io(&staging, e))?;
        }
        std::fs::create_dir_all(app_dir).map_err(|e| InstallError::io(app_dir, e))?;
        match asset.kind {
            AssetKind::TarGz => {
                suite_unpack::extract_tar_gz(dl, &staging, &self.limits)?;
            }
            AssetKind::Zip => {
                suite_unpack::extract_zip(dl, &staging, &self.limits)?;
            }
            AssetKind::AppImage => {
                let name = format!("{id}.AppImage");
                suite_unpack::appimage::place(dl, &staging, &name)?;
                *exe_rel = Some(name);
                return Ok(staging);
            }
            AssetKind::Dmg => return self.unpack_dmg(id, dl, &staging, asset, exe_rel),
            AssetKind::FlatpakBundle | AssetKind::Msi | AssetKind::Store => {
                return Err(InstallError::Unsupported(format!("{} is not unpacked", asset.kind)));
            }
        }
        let root = suite_unpack::flatten_single_dir(&staging).map_err(|e| InstallError::io(&staging, e))?;
        // A lone `Demo.app` is the bundle itself, not a wrapper folder.
        if root.extension().is_some_and(|e| e == "app") { Ok(staging) } else { Ok(root) }
    }

    /// Mounts the disk image (`hdiutil`), copies the bundle out (`ditto`) and detaches; all
    /// through the runner, so tests drive it with a fake on any OS.
    fn unpack_dmg(
        &self,
        id: &str,
        dl: &Path,
        staging: &Path,
        asset: &Asset,
        exe_rel: &mut Option<String>,
    ) -> Result<PathBuf, InstallError> {
        if self.env.os != Os::Macos {
            return Err(InstallError::Unsupported("disk images can only be installed on macOS".into()));
        }
        let mount = self.env.paths.data.join(format!("mount-{id}"));
        let bundle = asset.layout.executable.as_deref();
        let app = suite_unpack::dmg::copy_app(self.env.runner.as_ref(), dl, &mount, bundle, staging)?;
        let _ = std::fs::remove_dir(&mount);
        *exe_rel = app.file_name().and_then(|n| n.to_str()).map(str::to_owned);
        Ok(staging.to_owned())
    }

    /// A bundle whose `CFBundleIdentifier` isn't the catalog's is not the app it claims to
    /// be (or the catalog is wrong); either way it isn't installed.
    fn check_bundle_id(app: &App, bundle: &Path) -> Result<(), InstallError> {
        let Some(expected) = &app.ids.macos_bundle else { return Ok(()) };
        match suite_platform::macos::bundle_info(bundle) {
            Some(info) if info.id == *expected => Ok(()),
            Some(info) => Err(InstallError::Unsupported(format!(
                "{} identifies as {:?}, not {expected:?}; not installed",
                bundle.display(),
                info.id
            ))),
            None => Err(InstallError::Unsupported(format!(
                "{} has no readable Info.plist; not installed",
                bundle.display()
            ))),
        }
    }

    /// Refreshes shortcuts, desktop entries and registry entries for `st.current`.
    fn reintegrate(&self, id: &str, st: &mut AppState, scope: Scope) -> Result<(), InstallError> {
        let app_dir = self.app_dir(id);
        let layout = st
            .layouts
            .get(&st.current.to_string())
            .cloned()
            .unwrap_or_else(|| Layout { executable: st.executable.clone(), ..Layout::default() });
        let (suite_exe, shim) = self.suite_files();
        let target = Target {
            env: &self.env,
            app_id: id,
            name: &st.name,
            pitch: &st.pitch,
            desktop_id: st.desktop_id.clone().unwrap_or_else(|| id.to_owned()),
            version_dir: app_dir.join(st.current.to_string()),
            app_dir,
            exe_rel: layout.executable.clone(),
            layout: &layout,
            bin_name: st.bin_name.clone().unwrap_or_else(|| id.to_owned()),
            version: &st.current,
            suite_exe: &suite_exe,
            shim: shim.as_deref(),
            prior: &st.created,
            mac_dest: self.mac_destination(scope),
            file_types: if st.associate { &layout.file_types } else { &[] },
        };
        let created = integrate::integrate(&target, st.kind == InstallKind::MacBundle)?;
        integrate::remove_stale(&self.env, &st.created, &created);
        st.created = created;
        Ok(())
    }

    /// Where `.app` bundles are copied: `~/Applications`, or `/Applications` for everyone.
    fn mac_destination(&self, scope: Scope) -> PathBuf {
        match scope {
            Scope::User => self.env.paths.mac_applications.clone(),
            Scope::AllUsers => self.env.paths.sys("Applications"),
        }
    }

    /// Turns the app's file associations (HKCU "Open with" entries, Windows) on or off.
    pub fn set_file_associations(&self, id: &str, on: bool) -> Result<Vec<String>, InstallError> {
        if self.env.os != Os::Windows {
            return Err(InstallError::Unsupported(
                "file types are registered by the app itself on this system (its desktop file or Info.plist)"
                    .into(),
            ));
        }
        let _lock = self.lock()?;
        let sf = &self.env.paths.state_file;
        let mut state = State::load(sf)?;
        self.recover_locked(&mut state)?;
        let mut st = state.apps.get(id).cloned().ok_or_else(|| InstallError::NotInstalled(id.to_owned()))?;
        if st.kind != InstallKind::Archive {
            return Err(InstallError::Unsupported(format!("{} has no file types to register", st.name)));
        }
        let types = st.layouts.get(&st.current.to_string()).map_or_default(|l| l.file_types.clone());
        if on && types.is_empty() {
            return Err(InstallError::Unsupported(format!("{} opens no file types", st.name)));
        }
        st.associate = on;
        self.reintegrate(id, &mut st, state.settings.scope)?;
        state.apps.insert(id.to_owned(), st);
        state.save(sf)?;
        Ok(if on { types } else { Vec::new() })
    }

    /// Keeps the current and previous versions; deletes older ones unless still running.
    fn prune(&self, app_dir: &Path, st: &mut AppState) {
        let keep: BTreeSet<semver::Version> =
            [Some(st.current.clone()), st.previous.clone()].into_iter().flatten().collect();
        let mut kept = Vec::new();
        for v in std::mem::take(&mut st.versions) {
            let dir = app_dir.join(v.to_string());
            if keep.contains(&v) {
                kept.push(v);
            } else if !self.running_pids(&dir).is_empty() {
                tracing::info!(version = %v, "old version still running; removing it later");
                kept.push(v);
            } else {
                if let Err(e) = std::fs::remove_dir_all(&dir)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(dir = %dir.display(), error = %e, "could not remove old version");
                    kept.push(v.clone());
                }
                st.layouts.remove(&v.to_string());
            }
        }
        st.versions = kept;
    }

    /// The command that starts an installed app: its executable in the current version
    /// folder, `flatpak run`, or `open` on the copied `.app` bundle.
    pub fn launch_command(&self, id: &str) -> Result<(String, Vec<std::ffi::OsString>), InstallError> {
        let state = self.state()?;
        let st = state.apps.get(id).ok_or_else(|| InstallError::NotInstalled(id.to_owned()))?;
        let missing = || InstallError::Unsupported(format!("{} has no program to open", st.name));
        match st.kind {
            InstallKind::Flatpak => {
                let fid = st.flatpak_id.clone().unwrap_or_else(|| id.to_owned());
                Ok(("flatpak".into(), suite_platform::runner::args(["run", fid.as_str()])))
            }
            InstallKind::MacBundle => {
                let bundle = st.executable.as_ref().ok_or_else(missing)?;
                // The copy in (~)/Applications, or the version folder's when a copy that
                // isn't Stockroom's is in the way.
                let path = st
                    .created
                    .iter()
                    .filter_map(crate::state::Created::path)
                    .find(|p| p.extension().is_some_and(|e| e == "app"))
                    .map_or_else(
                        || integrate::join_rel(&self.app_dir(id).join(st.current.to_string()), bundle),
                        Path::to_owned,
                    );
                Ok(("open".into(), suite_platform::runner::args([path])))
            }
            InstallKind::Archive | InstallKind::AppImage => {
                let exe = st.executable.as_ref().ok_or_else(missing)?;
                let path = integrate::join_rel(&self.app_dir(id).join(st.current.to_string()), exe);
                if !path.is_file() {
                    return Err(missing());
                }
                Ok((path.to_string_lossy().into_owned(), Vec::new()))
            }
        }
    }

    /// Starts an installed app without waiting for it.
    pub fn launch(&self, id: &str) -> Result<(), InstallError> {
        let (program, args) = self.launch_command(id)?;
        self.env
            .runner
            .spawn(&program, &args)
            .map_err(|e| InstallError::Unsupported(format!("{id} could not be opened: {e}")))
    }

    /// Switches back to the previous version.
    pub fn rollback(&self, id: &str) -> Result<Outcome, InstallError> {
        let _lock = self.lock()?;
        let sf = &self.env.paths.state_file;
        let mut state = State::load(sf)?;
        self.recover_locked(&mut state)?;
        let mut st = state.apps.get(id).cloned().ok_or_else(|| InstallError::NotInstalled(id.to_owned()))?;
        if st.kind == InstallKind::Flatpak {
            return Err(InstallError::Unsupported("rollback is not available for Flatpak installs".into()));
        }
        let app_dir = self.app_dir(id);
        let prev = st
            .previous
            .clone()
            .filter(|p| app_dir.join(p.to_string()).is_dir())
            .ok_or(InstallError::NoPrevious)?;
        let from = st.current.clone();
        st.current = prev.clone();
        st.previous = Some(from.clone());
        st.updated = OffsetDateTime::now_utc();
        state.apps.insert(id.to_owned(), st.clone());
        state.save(sf)?;
        pointer::set(self.env.os, &app_dir, &prev, st.exe_for(&prev))?;
        self.reintegrate(id, &mut st, state.settings.scope)?;
        state.apps.insert(id.to_owned(), st);
        state.save(sf)?;
        let needs_restart = !self.running_pids(&app_dir).is_empty();
        Ok(Outcome {
            app: id.to_owned(),
            from: Some(from),
            to: prev,
            needs_restart,
            unchanged: false,
            opened_store: None,
        })
    }

    /// Removes an app: its version folders and exactly what Stockroom created for it. With
    /// `purge`, also its data folders (`~/...` paths from the catalog).
    pub fn remove(&self, id: &str, purge: Option<&[String]>) -> Result<Vec<PathBuf>, InstallError> {
        let _lock = self.lock()?;
        let sf = &self.env.paths.state_file;
        let mut state = State::load(sf)?;
        self.recover_locked(&mut state)?;
        let st = state.apps.get(id).cloned().ok_or_else(|| InstallError::NotInstalled(id.to_owned()))?;
        let app_dir = self.app_dir(id);
        if st.kind == InstallKind::Flatpak {
            let fid = st.flatpak_id.clone().unwrap_or_else(|| id.to_owned());
            suite_unpack::flatpak::uninstall(self.env.runner.as_ref(), &fid)?;
        } else {
            let pids = self.running_pids(&app_dir);
            if !pids.is_empty() {
                return Err(InstallError::Running { app: id.to_owned(), pids });
            }
        }
        integrate::remove_created(&self.env, &st.created);
        if self.env.os == Os::Linux && st.kind != InstallKind::Flatpak {
            integrate::refresh_linux_caches(&self.env, true);
        }
        pointer::clear(&app_dir);
        if app_dir.exists() {
            std::fs::remove_dir_all(&app_dir).map_err(|e| InstallError::io(&app_dir, e))?;
        }
        let mut purged = Vec::new();
        for p in purge.unwrap_or_default() {
            match self.env.paths.expand_home(p) {
                Some(path) if path.exists() => {
                    std::fs::remove_dir_all(&path)
                        .or_else(|_| std::fs::remove_file(&path))
                        .map_err(|e| InstallError::io(&path, e))?;
                    purged.push(path);
                }
                Some(_) => {}
                None => tracing::warn!(path = p, "not a home-relative data path; skipped"),
            }
        }
        state.apps.remove(id);
        state.save(sf)?;
        Ok(purged)
    }

    /// Repairs what an interrupted operation left behind. Returns what was fixed.
    pub fn recover(&self) -> Result<Vec<String>, InstallError> {
        let _lock = self.lock()?;
        let mut state = State::load(&self.env.paths.state_file)?;
        self.recover_locked(&mut state)
    }

    fn recover_locked(&self, state: &mut State) -> Result<Vec<String>, InstallError> {
        let mut fixed = Vec::new();
        let os = self.env.os;
        let mut lost = Vec::new();
        for (id, st) in &mut state.apps {
            if st.kind == InstallKind::Flatpak {
                continue;
            }
            let app_dir = self.env.paths.apps.join(id);
            st.versions.retain(|v| app_dir.join(v.to_string()).is_dir());
            if !st.versions.contains(&st.current) {
                if let Some(p) = st.previous.take().filter(|p| st.versions.contains(p)) {
                    fixed.push(format!("{id}: {} was missing; back to {p}", st.current));
                    st.current = p;
                } else {
                    fixed.push(format!("{id}: its files are gone; forgetting it"));
                    lost.push(id.clone());
                    continue;
                }
            }
            if st.previous.as_ref().is_some_and(|p| !st.versions.contains(p)) {
                st.previous = None;
            }
            if pointer::read(os, &app_dir).as_ref() != Some(&st.current) {
                let exe = st.exe_for(&st.current).map(str::to_owned);
                pointer::set(os, &app_dir, &st.current, exe.as_deref())?;
                fixed.push(format!("{id}: pointer set to {}", st.current));
            }
            // Leftovers of interrupted installs: staging folders, uncommitted versions.
            if let Ok(entries) = std::fs::read_dir(&app_dir) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let path = e.path();
                    let orphan_version =
                        name.parse::<semver::Version>().is_ok_and(|v| !st.versions.contains(&v));
                    let leftover =
                        name.starts_with(".staging-") || name.ends_with(".suite-new") || orphan_version;
                    if leftover && self.running_pids(&path).is_empty() {
                        let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
                        fixed.push(format!("{id}: removed leftover {name}"));
                    }
                }
            }
        }
        for id in lost {
            if let Some(st) = state.apps.remove(&id) {
                integrate::remove_created(&self.env, &st.created);
            }
        }
        // App folders the state doesn't know: a first install interrupted before its commit.
        if let Ok(entries) = std::fs::read_dir(&self.env.paths.apps) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !state.apps.contains_key(&name)
                    && e.path().is_dir()
                    && self.running_pids(&e.path()).is_empty()
                {
                    let _ = std::fs::remove_dir_all(e.path());
                    fixed.push(format!("removed unfinished install {name}"));
                }
            }
        }
        if !fixed.is_empty() {
            for f in &fixed {
                tracing::info!("recovery: {f}");
            }
            state.save(&self.env.paths.state_file)?;
        }
        Ok(fixed)
    }
}

/// Whether a download error means a pinned URL now serves different bytes.
fn pinned_moved(e: &suite_fetch::FetchError) -> bool {
    use suite_fetch::FetchError as F;
    matches!(
        e,
        F::Checksum { .. }
            | F::TooLarge { .. }
            | F::Truncated { .. }
            | F::Status { status: 404 | 410 | 416, .. }
    )
}
