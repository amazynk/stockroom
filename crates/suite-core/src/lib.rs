//! Orchestration for the CLI and the UI: loading the catalog, planning
//! installs and updates, a job queue that reports progress over a channel, per-app update
//! policies, the scheduled background run, and `doctor`.

mod queue;

use std::{fmt::Write as _, path::PathBuf, sync::OnceLock, time::Duration as StdDuration};

pub use queue::{CoreEvent, Job, JobOutcome, Queue};
use suite_catalog::{App, Asset, Catalog, CatalogError, Channel, HostFacts, NotInstallable, Prefs, Release};
use suite_detect::Found;
use suite_fetch::Fetcher;
pub use suite_install::{Change, ChangeKind, Scope};
use suite_install::{Installer, Outcome, Policy, State};
use suite_platform::{Env, Metered, Scheduler};
use time::{Duration, OffsetDateTime};

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(transparent)]
    Catalog(#[from] suite_catalog::CatalogError),
    #[error(transparent)]
    Fetch(#[from] suite_fetch::FetchError),
    #[error(transparent)]
    Install(#[from] suite_install::InstallError),
    #[error(transparent)]
    Platform(#[from] suite_platform::PlatformError),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("no app called {0:?} in the catalog")]
    UnknownApp(String),
    #[error("{app}: {why}")]
    NotInstallable { app: String, why: NotInstallable },
    #[error("{0} is not installed by Stockroom")]
    NotInstalled(String),
    #[error("{0}")]
    Open(String),
}

/// What kind of trouble an error is, so a front end can say it plainly (and translated)
/// while keeping the details for those who want them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Another Stockroom (the scheduled run, a second window) is changing apps.
    Busy,
    /// The app is open and has to be closed first.
    Running,
    /// A pinned download changed upstream: the catalog has to be published again.
    NeedsRefresh,
    /// The network or the server failed.
    Network,
    /// The download didn't match the catalog (size or checksum).
    Integrity,
    /// The catalog couldn't be used: not valid, or older than one already seen.
    Catalog,
    /// This computer can't run the app, or it has no release yet.
    NotInstallable,
    /// There is no previous version to go back to.
    NoPrevious,
    /// Reading or writing files failed (disk full, permissions).
    Disk,
    Other,
}

impl CoreError {
    pub fn kind(&self) -> ErrorKind {
        use suite_install::InstallError as I;
        let fetch = |e: &suite_fetch::FetchError| match e {
            suite_fetch::FetchError::Checksum { .. } | suite_fetch::FetchError::TooLarge { .. } => {
                ErrorKind::Integrity
            }
            suite_fetch::FetchError::Io { .. } => ErrorKind::Disk,
            _ => ErrorKind::Network,
        };
        match self {
            Self::Catalog(_) => ErrorKind::Catalog,
            Self::Fetch(e) => fetch(e),
            Self::Install(e) => match e {
                I::Busy => ErrorKind::Busy,
                I::Running { .. } => ErrorKind::Running,
                I::PinnedChanged { .. } => ErrorKind::NeedsRefresh,
                I::NoPrevious => ErrorKind::NoPrevious,
                I::Fetch(f) => fetch(f),
                I::Unpack(suite_unpack::UnpackError::Io { .. }) | I::Io { .. } => ErrorKind::Disk,
                I::Unpack(_) | I::LayoutMissing(_) => ErrorKind::Integrity,
                _ => ErrorKind::Other,
            },
            Self::Io { .. } => ErrorKind::Disk,
            Self::NotInstallable { .. } => ErrorKind::NotInstallable,
            Self::Platform(_) | Self::UnknownApp(_) | Self::NotInstalled(_) | Self::Open(_) => {
                ErrorKind::Other
            }
        }
    }
}

/// Where the catalog comes from: a URL, or a local file (development).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogSource {
    Url(String),
    File(PathBuf),
}

impl CatalogSource {
    pub fn parse(s: &str) -> Self {
        if s.starts_with("https://") || s.starts_with("http://") {
            Self::Url(s.to_owned())
        } else {
            Self::File(PathBuf::from(s))
        }
    }

    pub fn default_url() -> Self {
        Self::Url(suite_identity::CATALOG_URL.to_owned())
    }
}

impl std::fmt::Display for CatalogSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(u) => f.write_str(u),
            Self::File(p) => write!(f, "{}", p.display()),
        }
    }
}

/// The app list this build shipped with (`catalog/builtin.json`, a copy of the catalog as it
/// was published when the build was made): what [`Core::catalog`] falls back to when the
/// server can't be reached and nothing newer was saved. Its downloads are checked against
/// the checksums in it, like the server's.
pub const BUILTIN_CATALOG: &[u8] = include_bytes!("../../../catalog/builtin.json");

/// The icons of the apps in the built-in list (`catalog/apps/<id>.png`), by app ID, so that
/// a first start without the network shows them too. [`Core::icon_file`] uses one when it is
/// the icon the list names, or when that one can't be downloaded.
pub const BUILTIN_ICONS: &[(&str, &[u8])] = &[
    ("dozed", include_bytes!("../../../catalog/apps/dozed.png")),
    ("pagebara", include_bytes!("../../../catalog/apps/pagebara.png")),
    ("specular", include_bytes!("../../../catalog/apps/specular.png")),
];

/// Where an app list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListOrigin {
    /// Fetched from the server just now.
    Server,
    /// The copy saved after the last successful check.
    Saved,
    /// The list this build shipped with.
    BuiltIn,
}

/// The app list to work with, and where it came from.
#[derive(Debug, Clone)]
pub struct AppList {
    pub catalog: Catalog,
    pub origin: ListOrigin,
    /// Why the server's list isn't the one in use: what kind of trouble, and the details.
    pub problem: Option<(ErrorKind, String)>,
}

const MAX_CATALOG: u64 = 8 << 20;
/// The scheduled run checks at most this often.
const MIN_CHECK_INTERVAL: Duration = Duration::hours(20);

#[derive(Debug)]
pub struct Core {
    env: Env,
    installer: Installer,
    source: CatalogSource,
    /// The list to fall back to when the server's can't be had; see [`BUILTIN_CATALOG`].
    builtin: Option<Vec<u8>>,
    facts: OnceLock<HostFacts>,
}

/// A row of `stockroom list` / `stockroom status`.
#[derive(Debug, Clone)]
pub struct AppRow {
    pub app: App,
    pub installed: Option<suite_install::AppState>,
    /// The best release for this computer on the app's channel.
    pub available: Result<semver::Version, NotInstallable>,
    /// The best asset is a store listing: the store installs and updates the app.
    pub via_store: Option<suite_catalog::Store>,
    pub others: Vec<Found>,
    /// The release and asset an install or update would get.
    pub best: Option<(Release, Asset)>,
}

impl AppRow {
    pub fn update_available(&self) -> Option<&semver::Version> {
        match (&self.installed, &self.available) {
            (Some(i), Ok(v)) if *v > i.current => Some(v),
            _ => None,
        }
    }
}

/// What a background run did.
#[derive(Debug, Default)]
pub struct BackgroundReport {
    pub skipped: Option<&'static str>,
    pub applied: Vec<Outcome>,
    /// Updates waiting for the user (policy "ask").
    pub waiting: Vec<(String, semver::Version)>,
    pub failed: Vec<(String, String)>,
}

/// A `doctor` finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub level: Level,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Core {
    pub fn new(env: Env, fetcher: Fetcher, source: CatalogSource) -> Self {
        let installer = Installer::new(env.clone(), fetcher);
        Self { env, installer, source, builtin: None, facts: OnceLock::new() }
    }

    /// Falls back to this list (catalog JSON) when the server's can't be fetched or used:
    /// [`BUILTIN_CATALOG`] for the real catalog. Without one, only the server's list and the
    /// copy saved from it are used, which is what a catalog given with `--catalog` wants.
    #[must_use]
    pub fn with_builtin(mut self, json: impl Into<Vec<u8>>) -> Self {
        self.builtin = Some(json.into());
        self
    }

    #[must_use]
    pub fn with_installer(mut self, installer: Installer) -> Self {
        self.installer = installer;
        self
    }

    #[must_use]
    pub fn with_facts(self, facts: HostFacts) -> Self {
        let _ = self.facts.set(facts);
        self
    }

    pub fn env(&self) -> &Env {
        &self.env
    }

    pub fn installer(&self) -> &Installer {
        &self.installer
    }

    pub fn source(&self) -> &CatalogSource {
        &self.source
    }

    pub fn facts(&self) -> &HostFacts {
        self.facts.get_or_init(|| suite_platform::facts::detect(&self.env))
    }

    pub fn state(&self) -> Result<State, CoreError> {
        Ok(self.installer.state()?)
    }

    fn fetch_catalog_bytes(&self) -> Result<Vec<u8>, CoreError> {
        match &self.source {
            CatalogSource::Url(u) => Ok(self.installer.fetcher().get_bytes(u, MAX_CATALOG)?),
            CatalogSource::File(p) => {
                std::fs::read(p).map_err(|e| CoreError::Io { path: p.clone(), source: e })
            }
        }
    }

    /// Parses a catalog and refuses one older than the newest seen here, so a stale copy on
    /// the way can't undo a newer list.
    fn parse_catalog(json: &[u8], state: &State) -> Result<Catalog, CoreError> {
        let catalog = Catalog::from_json(json)?;
        match state.catalog.last_version {
            Some(seen) if catalog.version < seen => {
                Err(CatalogError::Older { got: catalog.version, seen }.into())
            }
            _ => Ok(catalog),
        }
    }

    /// Fetches and checks the catalog, then records its serial.
    pub fn load_catalog(&self) -> Result<Catalog, CoreError> {
        let json = self.fetch_catalog_bytes()?;
        let state = self.installer.state()?;
        let catalog = Self::parse_catalog(&json, &state)?;
        // Kept for the next start, which shows it at once and refreshes behind it.
        if let Err(e) = suite_install::state::write_atomic(&self.catalog_cache_path(), &json) {
            tracing::debug!("catalog cache not written: {e}");
        }
        let version = catalog.version;
        if state.catalog.last_version != Some(version) {
            self.installer.update_state(|s| {
                s.catalog.last_version = Some(s.catalog.last_version.map_or(version, |v| v.max(version)));
            })?;
        }
        Ok(catalog)
    }

    /// The release and asset `app_id` would get on `channel`.
    pub fn plan<'c>(
        &self,
        catalog: &'c Catalog,
        app_id: &str,
        channel: Channel,
    ) -> Result<(&'c App, &'c Release, &'c Asset), CoreError> {
        let app = catalog.app(app_id).ok_or_else(|| CoreError::UnknownApp(app_id.to_owned()))?;
        let state = self.installer.state()?;
        let (release, asset) = suite_catalog::best_installable(app, channel, self.facts(), prefs(&state))
            .map_err(|why| CoreError::NotInstallable { app: app_id.to_owned(), why })?;
        Ok((app, release, asset))
    }

    /// Installs (or updates to) the best release on `channel`; `None` keeps the app's
    /// current channel, or stable for a new install.
    pub fn install(
        &self,
        catalog: &Catalog,
        app_id: &str,
        channel: Option<Channel>,
        on: &mut dyn FnMut(suite_install::Event),
    ) -> Result<Outcome, CoreError> {
        let state = self.installer.state()?;
        let channel = channel.or_else(|| state.apps.get(app_id).map(|a| a.channel)).unwrap_or_default();
        let (app, release, asset) = self.plan(catalog, app_id, channel)?;
        let outcome = self.installer.install(app, release, asset, channel, on)?;
        self.record(&app.name, &outcome);
        Ok(outcome)
    }

    /// Updates an installed app if a newer release is available; `Ok(None)` if current.
    pub fn update(
        &self,
        catalog: &Catalog,
        app_id: &str,
        on: &mut dyn FnMut(suite_install::Event),
    ) -> Result<Option<Outcome>, CoreError> {
        let state = self.installer.state()?;
        let installed = state.apps.get(app_id).ok_or_else(|| CoreError::NotInstalled(app_id.to_owned()))?;
        let (app, release, asset) = self.plan(catalog, app_id, installed.channel)?;
        if release.version <= installed.current {
            return Ok(None);
        }
        let outcome = self.installer.install(app, release, asset, installed.channel, on)?;
        self.record(&app.name, &outcome);
        Ok(Some(outcome))
    }

    pub fn remove(
        &self,
        catalog: Option<&Catalog>,
        app_id: &str,
        purge: bool,
    ) -> Result<Vec<PathBuf>, CoreError> {
        let paths: Vec<String> = if purge {
            catalog.and_then(|c| c.app(app_id)).map_or_default(|a| a.data_paths.for_os(self.env.os).to_vec())
        } else {
            Vec::new()
        };
        let before = self.installer.state()?.apps.get(app_id).cloned();
        let purged = self.installer.remove(app_id, purge.then_some(paths.as_slice()))?;
        if let Some(st) = before {
            self.log(app_id, &st.name, ChangeKind::Removed { version: st.current, purged: purge });
        }
        Ok(purged)
    }

    pub fn rollback(&self, app_id: &str) -> Result<Outcome, CoreError> {
        let outcome = self.installer.rollback(app_id)?;
        let name =
            self.installer.state()?.apps.get(app_id).map_or_else(|| app_id.to_owned(), |a| a.name.clone());
        if let Some(from) = &outcome.from {
            self.log(app_id, &name, ChangeKind::RolledBack { from: from.clone(), to: outcome.to.clone() });
        }
        Ok(outcome)
    }

    /// Records a finished install or update in the history.
    fn record(&self, name: &str, o: &Outcome) {
        let what = match (&o.opened_store, &o.from) {
            (Some(store), _) => ChangeKind::OpenedStore { store: *store },
            _ if o.unchanged => return,
            (None, Some(from)) => ChangeKind::Updated { from: from.clone(), to: o.to.clone() },
            (None, None) => ChangeKind::Installed { version: o.to.clone() },
        };
        self.log(&o.app, name, what);
    }

    fn log(&self, app: &str, name: &str, what: ChangeKind) {
        let change =
            Change { at: OffsetDateTime::now_utc(), app: app.to_owned(), name: name.to_owned(), what };
        if let Err(e) = self.installer.update_state(|s| s.record(change)) {
            tracing::warn!("history not saved: {e}");
        }
    }

    fn catalog_cache_path(&self) -> PathBuf {
        self.env.paths.data.join("catalog.json")
    }

    /// The catalog saved by the last successful [`Core::load_catalog`], checked again,
    /// without touching the network: for showing something at once while a fresh copy
    /// loads, and for working while the server is out of reach. `Ok(None)` when nothing is
    /// cached.
    pub fn cached_catalog(&self) -> Result<Option<Catalog>, CoreError> {
        let Ok(json) = std::fs::read(self.catalog_cache_path()) else { return Ok(None) };
        Ok(Some(Self::parse_catalog(&json, &self.installer.state()?)?))
    }

    /// The list this build shipped with, if it was given one ([`Core::with_builtin`]).
    pub fn builtin_catalog(&self) -> Option<Catalog> {
        let json = self.builtin.as_deref()?;
        match Catalog::from_json(json) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!("the built-in app list is unusable: {e}");
                None
            }
        }
    }

    /// The best list to be had without the network: the newer of the saved copy and the
    /// built-in one.
    pub fn offline_catalog(&self) -> Option<(Catalog, ListOrigin)> {
        let saved = match self.cached_catalog() {
            Ok(c) => c,
            Err(e) => {
                tracing::info!("saved app list not used: {e}");
                None
            }
        };
        match (saved, self.builtin_catalog()) {
            (Some(s), Some(b)) if b.version > s.version => Some((b, ListOrigin::BuiltIn)),
            (Some(s), _) => Some((s, ListOrigin::Saved)),
            (None, Some(b)) => Some((b, ListOrigin::BuiltIn)),
            (None, None) => None,
        }
    }

    /// The app list to work with: the server's, or, when that can't be fetched or used and
    /// this core has a built-in list to fall back to, the best one at hand
    /// ([`Core::offline_catalog`]), with the reason in [`AppList::problem`].
    pub fn catalog(&self) -> Result<AppList, CoreError> {
        let error = match self.load_catalog() {
            Ok(catalog) => return Ok(AppList { catalog, origin: ListOrigin::Server, problem: None }),
            Err(e) => e,
        };
        if self.builtin.is_none() {
            return Err(error);
        }
        match self.offline_catalog() {
            Some((catalog, origin)) => {
                tracing::info!(?origin, serial = catalog.version, "server's app list not used: {error}");
                Ok(AppList { catalog, origin, problem: Some((error.kind(), error.to_string())) })
            }
            None => Err(error),
        }
    }

    /// The app's icon as a local PNG file: downloaded once and checked against the catalog's
    /// SHA-256, or the one this build carries for the app ([`BUILTIN_ICONS`]) when that is
    /// the very icon the catalog names, or when the download fails. `Ok(None)` when the
    /// catalog has none.
    pub fn icon_file(&self, app: &App) -> Result<Option<PathBuf>, CoreError> {
        const MAX_ICON: u64 = 1 << 20;
        let Some(icon) = &app.icon else { return Ok(None) };
        let dir = self.env.paths.data.join("icons");
        let path = dir.join(format!("{}.png", icon.sha256));
        if path.is_file() {
            return Ok(Some(path));
        }
        let digest = |bytes: &[u8]| {
            suite_catalog::Sha256Hex::from_digest(&<sha2::Sha256 as sha2::Digest>::digest(bytes))
        };
        let builtin = BUILTIN_ICONS.iter().find(|(id, _)| *id == app.id).map(|(_, png)| *png);
        if let Some(png) = builtin
            && digest(png) == icon.sha256
        {
            suite_install::state::write_atomic(&path, png)?;
            return Ok(Some(path));
        }
        let fetched =
            self.installer.fetcher().get_bytes(&icon.url, MAX_ICON).map_err(CoreError::from).and_then(
                |bytes| {
                    let got = digest(&bytes);
                    if got == icon.sha256 {
                        Ok(bytes)
                    } else {
                        Err(CoreError::Fetch(suite_fetch::FetchError::Checksum {
                            url: icon.url.clone(),
                            expected: icon.sha256.clone(),
                            got,
                        }))
                    }
                },
            );
        match (fetched, builtin) {
            (Ok(bytes), _) => {
                suite_install::state::write_atomic(&path, &bytes)?;
                Ok(Some(path))
            }
            // An older drawing of the same app beats none; the newer one is tried again
            // next time, as nothing is saved under its name.
            (Err(e), Some(png)) => {
                tracing::debug!(app = %app.id, "icon not downloaded ({e}); using the built-in one");
                let stand_in = dir.join(format!("builtin-{}.png", app.id));
                suite_install::state::write_atomic(&stand_in, png)?;
                Ok(Some(stand_in))
            }
            (Err(e), None) => Err(e),
        }
    }

    /// Starts an app Stockroom installed.
    pub fn open_app(&self, app_id: &str) -> Result<(), CoreError> {
        Ok(self.installer.launch(app_id)?)
    }

    /// Starts a copy installed by other means, as found by detection.
    pub fn open_found(&self, app: &App, found: &Found) -> Result<(), CoreError> {
        let (program, args) = found_command(app, found).ok_or_else(|| {
            CoreError::Open(format!("{} was installed by {}; open it from there", app.name, found.source))
        })?;
        self.env.runner.spawn(&program, &args).map_err(|e| CoreError::Open(format!("{}: {e}", app.name)))
    }

    /// Opens a web page (release notes, a store page, the homepage) in the browser.
    pub fn open_url(&self, url: &str) -> Result<(), CoreError> {
        suite_catalog::model::validate_url(url)?;
        let opener = match self.env.os {
            suite_catalog::Os::Windows => "explorer.exe",
            suite_catalog::Os::Macos => "open",
            suite_catalog::Os::Linux => "xdg-open",
        };
        self.env
            .runner
            .spawn(opener, &suite_platform::runner::args([url]))
            .map_err(|e| CoreError::Open(format!("{url}: {e}")))
    }

    /// Stockroom program to register for later runs. A copy of Stockroom started from a download keeps
    /// a copy of itself first ([`suite_install::settle`]) and that copy is the answer, so
    /// the daily check still finds its program once the download is gone. Development
    /// builds stay where cargo put them.
    pub fn settled_exe(&self) -> PathBuf {
        let running = std::env::current_exe().unwrap_or_default();
        if cfg!(debug_assertions) && !self.env.sandboxed {
            return running;
        }
        suite_install::settle::settle(&self.env, &running).unwrap_or_else(|e| {
            tracing::warn!("no copy of Stockroom kept: {e}");
            running
        })
    }

    /// Takes Stockroom itself off the computer: the daily check, its kept copy and menu
    /// entry, its downloads and the saved app list. The apps it installed stay and keep
    /// working, and so does the record of them, so a later Stockroom can look after them again.
    /// Returns what was removed.
    pub fn uninstall_suite(&self) -> Result<Vec<PathBuf>, CoreError> {
        Scheduler::new(&self.env).disable()?;
        self.installer.update_state(|s| s.settings.auto_update = false)?;
        let mut removed = suite_install::settle::unsettle(&self.env);
        let icons = self.env.paths.data.join("icons");
        for path in [self.catalog_cache_path(), icons, self.env.paths.downloads.clone()] {
            let gone =
                if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            if gone.is_ok() {
                removed.push(path);
            }
        }
        Ok(removed)
    }

    /// Turns the daily background update on (registering `exe` with the OS scheduler) or
    /// off, and remembers the choice.
    pub fn set_auto_update(
        &self,
        on: bool,
        exe: &std::path::Path,
    ) -> Result<Option<suite_platform::SchedulerKind>, CoreError> {
        let sched = Scheduler::new(&self.env);
        let kind = if on {
            Some(sched.enable(exe)?)
        } else {
            sched.disable()?;
            None
        };
        self.installer.update_state(|s| s.settings.auto_update = on)?;
        Ok(kind)
    }

    /// Whether background updates wait for an unmetered connection.
    pub fn set_skip_metered(&self, skip: bool) -> Result<(), CoreError> {
        Ok(self.installer.update_state(|s| s.settings.skip_metered = skip)?)
    }

    /// Who new installs are for (see [`Core::all_users_supported`]).
    pub fn set_scope(&self, scope: Scope) -> Result<(), CoreError> {
        if scope == Scope::AllUsers && !Self::all_users_supported() {
            return Err(CoreError::Open("installing for all users is not available yet".into()));
        }
        Ok(self.installer.update_state(|s| s.settings.scope = scope)?)
    }

    /// Installs for every user of the computer: on macOS the `.app` copy goes to
    /// `/Applications` (writable by admin accounts without elevating). Elsewhere it needs an
    /// elevated helper, which is planned, not built.
    pub const fn all_users_supported() -> bool {
        cfg!(target_os = "macos")
    }

    /// Moves an installed app to another channel; the next update follows it.
    pub fn set_channel(&self, app_id: &str, channel: Channel) -> Result<(), CoreError> {
        let found =
            self.installer.update_state(|s| s.apps.get_mut(app_id).map(|a| a.channel = channel).is_some())?;
        if found { Ok(()) } else { Err(CoreError::NotInstalled(app_id.to_owned())) }
    }

    pub fn set_policy(&self, app_id: &str, policy: Policy) -> Result<(), CoreError> {
        let found =
            self.installer.update_state(|s| s.apps.get_mut(app_id).map(|a| a.policy = policy).is_some())?;
        if found { Ok(()) } else { Err(CoreError::NotInstalled(app_id.to_owned())) }
    }

    /// Every catalog app with its install state, best available version and other copies.
    pub fn rows(&self, catalog: &Catalog) -> Result<Vec<AppRow>, CoreError> {
        let state = self.installer.state()?;
        let prefs = prefs(&state);
        Ok(catalog
            .apps
            .iter()
            .map(|app| {
                let installed = state.apps.get(&app.id).cloned();
                let channel = installed.as_ref().map_or_default(|i| i.channel);
                let best = suite_catalog::best_installable(app, channel, self.facts(), prefs);
                let via_store = best.as_ref().ok().and_then(|(_, a)| a.store.as_ref()).map(|s| s.store);
                let chosen = best.as_ref().ok().map(|(r, a)| ((*r).clone(), (*a).clone()));
                let available = best.map(|(r, _)| r.version.clone());
                let mut others = suite_detect::detect(&self.env, app);
                // A Flatpak Stockroom installed itself is not "another" install.
                if installed.as_ref().is_some_and(|i| i.kind == suite_install::InstallKind::Flatpak) {
                    others.retain(|f| !matches!(f.source, suite_detect::Source::Flatpak { user: true }));
                }
                // Nor is the `.app` copy it made in ~/Applications or /Applications.
                if let Some(i) = &installed {
                    others.retain(|f| f.location.as_ref().is_none_or(|l| !i.created_path(l)));
                }
                AppRow { app: app.clone(), installed, available, via_store, others, best: chosen }
            })
            .collect())
    }

    /// The scheduled run: at most daily, not on metered connections, applying updates by
    /// each app's policy.
    pub fn background_update(&self, force: bool) -> Result<BackgroundReport, CoreError> {
        let mut report = BackgroundReport::default();
        let state = self.installer.state()?;
        if !force {
            if !state.settings.auto_update {
                report.skipped = Some("automatic updates are off");
                return Ok(report);
            }
            if state.catalog.last_check.is_some_and(|t| OffsetDateTime::now_utc() - t < MIN_CHECK_INTERVAL) {
                report.skipped = Some("checked recently");
                return Ok(report);
            }
            if state.settings.skip_metered && self.env.network.metered() == Metered::Yes {
                report.skipped = Some("metered connection");
                return Ok(report);
            }
        }
        let catalog = self.load_catalog()?;
        self.installer.update_state(|s| s.catalog.last_check = Some(OffsetDateTime::now_utc()))?;
        for (id, app) in &state.apps {
            let Ok((_, release, _)) = self.plan(&catalog, id, app.channel) else { continue };
            if release.version <= app.current {
                continue;
            }
            match app.policy {
                Policy::Never => {}
                Policy::Ask => report.waiting.push((id.clone(), release.version.clone())),
                Policy::Auto => match self.update(&catalog, id, &mut |_| {}) {
                    Ok(Some(o)) => report.applied.push(o),
                    Ok(None) => {}
                    Err(e) => report.failed.push((id.clone(), e.to_string())),
                },
            }
        }
        Ok(report)
    }

    /// Checks the platform, scheduler, state and catalog.
    pub fn doctor(&self) -> Vec<Check> {
        let mut out = Vec::new();
        let f = self.facts();
        let mut platform = format!("{} {}", f.os, f.arch);
        if let Some(g) = &f.glibc {
            let _ = write!(platform, ", glibc {g}");
        }
        if let Some(m) = &f.macos {
            let _ = write!(platform, ", macOS {m}");
        }
        if let Some(w) = f.windows_build {
            let _ = write!(platform, ", Windows build {w}");
        }
        if f.arch == suite_catalog::Arch::X86_64 {
            platform.push_str(if f.x86_64_v3 { ", x86-64-v3" } else { ", no AVX2" });
        }
        if f.flatpak {
            platform.push_str(", flatpak");
        }
        out.push(Check { name: "platform", level: Level::Ok, detail: platform });

        out.push(Check {
            name: "network",
            level: Level::Ok,
            detail: match self.env.network.metered() {
                Metered::Yes => "metered (background updates wait)".into(),
                Metered::No => "not metered".into(),
                Metered::Unknown => "metered state unknown".into(),
            },
        });

        let sched = Scheduler::new(&self.env).status();
        out.push(Check {
            name: "scheduler",
            level: if sched.enabled { Level::Ok } else { Level::Warn },
            detail: format!(
                "{} {}",
                sched.kind.describe(),
                if sched.enabled { "registered" } else { "not registered (`stockroom schedule on`)" }
            ),
        });

        match self.installer.state() {
            Ok(s) => out.push(Check {
                name: "state",
                level: Level::Ok,
                detail: format!("{} ({} apps)", self.env.paths.state_file.display(), s.apps.len()),
            }),
            Err(e) => out.push(Check { name: "state", level: Level::Fail, detail: e.to_string() }),
        }

        let started = std::time::Instant::now();
        match self.load_catalog() {
            Ok(c) => out.push(Check {
                name: "catalog",
                level: Level::Ok,
                detail: format!(
                    "{}: serial {}, {} apps, published {}, {} ms",
                    self.source,
                    c.version,
                    c.apps.len(),
                    c.published.date(),
                    started.elapsed().as_millis()
                ),
            }),
            Err(e) => {
                // Fetch errors name the address themselves.
                let (source, e) = (self.source.to_string(), e.to_string());
                let detail = if e.starts_with(&source) { e } else { format!("{source}: {e}") };
                out.push(Check { name: "catalog", level: Level::Fail, detail });
            }
        }
        if self.builtin.is_some() {
            out.push(match self.offline_catalog() {
                Some((c, origin)) => Check {
                    name: "fallback",
                    level: Level::Ok,
                    detail: format!(
                        "{} list of {}: serial {}, {} apps",
                        match origin {
                            ListOrigin::BuiltIn => "built-in",
                            ListOrigin::Saved | ListOrigin::Server => "saved",
                        },
                        c.published.date(),
                        c.version,
                        c.apps.len()
                    ),
                },
                None => Check {
                    name: "fallback",
                    level: Level::Warn,
                    detail: "no usable built-in or saved app list".into(),
                },
            });
        }
        out
    }
}

/// Whether [`Core::open_found`] knows how to start this copy.
pub fn can_open_found(app: &App, found: &Found) -> bool {
    found_command(app, found).is_some()
}

/// How to start a copy made by other means: `flatpak run`, the Store's app folder, `open`
/// on a bundle, or the executable that was found.
fn found_command(app: &App, found: &Found) -> Option<(String, Vec<std::ffi::OsString>)> {
    use suite_detect::Source;
    use suite_platform::runner::args;
    match (&found.source, &found.location) {
        (Source::Flatpak { .. }, _) => {
            let id = app.ids.flatpak.as_ref().or(app.ids.desktop.as_ref())?;
            Some(("flatpak".into(), args(["run", id.as_str()])))
        }
        (Source::MicrosoftStore, _) => {
            let aumid = app.ids.windows_aumid.as_deref()?;
            Some(("explorer.exe".into(), args([format!(r"shell:AppsFolder\{aumid}")])))
        }
        (_, Some(path)) if path.extension().is_some_and(|e| e == "app") => {
            Some(("open".into(), args([path])))
        }
        (_, Some(path)) if path.extension().is_none_or(|e| e == "exe") => {
            Some((path.to_string_lossy().into_owned(), Vec::new()))
        }
        _ => None,
    }
}

fn prefs(state: &State) -> Prefs {
    Prefs { prefer_flatpak: state.settings.prefer_flatpak }
}

/// How long the CLI waits between progress lines; exported for front ends.
pub const PROGRESS_INTERVAL: StdDuration = StdDuration::from_millis(200);
