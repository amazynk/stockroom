//! Install, update, rollback and remove against the fake release server, in a temporary
//! root. Nothing here touches the real home, registry or scheduler.

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{path::Path, process::Command, sync::Arc, time::Duration};

use suite_catalog::{AssetKind, Channel, Os};
use suite_fetch::{FetchError, Fetcher, Options};
use suite_install::{Failpoint, InstallError, InstallKind, Installer, Outcome, State, pointer};
use suite_platform::{Env, FakeRunner, Hive, Output, registry::Value};
#[cfg(target_os = "linux")]
use suite_platform::{ProcessProbe as _, process::SystemProcesses};
use suite_testkit::{Fault, Fixture, archive, fake_app, fake_app_with_exe};
use tempfile::TempDir;

fn fetcher() -> Fetcher {
    Fetcher::new(Options { backoff: Duration::from_millis(5), ..Options::default() })
}

struct Setup {
    root: TempDir,
    fx: Fixture,
    env: Env,
}

impl Setup {
    fn new(os: Os) -> Self {
        let root = tempfile::tempdir().unwrap();
        let env = Env::for_tests(root.path(), os);
        Self { root, fx: Fixture::new(), env }
    }

    fn installer(&self) -> Installer {
        Installer::new(self.env.clone(), fetcher())
    }

    fn install(&self, inst: &Installer, version: &str) -> Result<Outcome, InstallError> {
        let app = self.fx.catalog.app("demo").unwrap();
        let release = app.releases.iter().find(|r| r.version.to_string() == version).unwrap();
        inst.install(app, release, &release.assets[0], Channel::Stable, &mut |_| {})
    }

    fn app_dir(&self) -> std::path::PathBuf {
        self.env.paths.apps.join("demo")
    }

    fn state(&self) -> State {
        State::load(&self.env.paths.state_file).unwrap()
    }
}

fn run(exe: &Path) -> String {
    let out = Command::new(exe).output().unwrap();
    assert!(out.status.success(), "{}", exe.display());
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn files_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                stack.push(e.path());
            } else {
                out.push(e.path().strip_prefix(dir).unwrap().display().to_string());
            }
        }
    }
    out.sort();
    out
}

#[test]
fn install_update_rollback_remove_on_linux() {
    let mut s = Setup::new(Os::Linux);
    for v in ["1.0.0", "1.1.0", "1.2.0"] {
        s.fx.add_release("demo", v, AssetKind::TarGz, fake_app("demo", v));
    }
    let inst = s.installer();
    let paths = s.env.paths.clone();

    // Install.
    let out = s.install(&inst, "1.0.0").unwrap();
    assert_eq!((out.from, out.to.to_string(), out.needs_restart), (None, "1.0.0".into(), false));
    let app_dir = s.app_dir();
    assert_eq!(pointer::read(Os::Linux, &app_dir), Some("1.0.0".parse().unwrap()));
    // The single top folder was stripped.
    assert!(app_dir.join("1.0.0/demo").is_file());
    let bin = paths.bin.join("demo");
    assert_eq!(run(&bin), "demo 1.0.0");
    let desktop =
        std::fs::read_to_string(paths.xdg_data.join("applications/com.example.demo.desktop")).unwrap();
    let exe = app_dir.join("current/demo");
    assert!(desktop.contains(&format!("Exec=\"{}\" %F", exe.display())), "{desktop}");
    assert!(desktop.contains("X-Stockroom-App=demo"));
    assert!(paths.xdg_data.join("icons/hicolor/scalable/apps/com.example.demo.svg").is_file());
    // The download was cleaned up.
    assert_eq!(std::fs::read_dir(&paths.downloads).unwrap().count(), 0);

    // Same version again: nothing to do.
    assert!(s.install(&inst, "1.0.0").unwrap().unchanged);

    // Update twice: the previous version is kept, older ones go.
    let out = s.install(&inst, "1.1.0").unwrap();
    assert_eq!(out.from.unwrap().to_string(), "1.0.0");
    assert_eq!(run(&bin), "demo 1.1.0");
    s.install(&inst, "1.2.0").unwrap();
    assert_eq!(run(&bin), "demo 1.2.0");
    assert!(!app_dir.join("1.0.0").exists());
    assert!(app_dir.join("1.1.0").is_dir());
    let st = s.state();
    let demo = &st.apps["demo"];
    assert_eq!(
        (demo.current.to_string(), demo.previous.as_ref().unwrap().to_string()),
        ("1.2.0".into(), "1.1.0".into())
    );
    assert_eq!(demo.versions.len(), 2);

    // Rollback, and rollback again (back to 1.2.0).
    let out = inst.rollback("demo").unwrap();
    assert_eq!(out.to.to_string(), "1.1.0");
    assert_eq!(run(&bin), "demo 1.1.0");
    assert_eq!(inst.rollback("demo").unwrap().to.to_string(), "1.2.0");

    // Remove with purge: only Stockroom's files and the app's data folder go.
    let data = paths.home.join(".config/demo");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("settings.toml"), "x").unwrap();
    let mine = paths.home.join(".config/other-app");
    std::fs::create_dir_all(&mine).unwrap();
    let purge = s.fx.catalog.app("demo").unwrap().data_paths.linux.clone();
    let purged = inst.remove("demo", Some(&purge)).unwrap();
    assert_eq!(purged, std::slice::from_ref(&data));
    assert!(!data.exists() && mine.exists());
    assert!(!app_dir.exists());
    assert!(std::fs::symlink_metadata(&bin).is_err());
    assert!(s.state().apps.is_empty());
    let left: Vec<String> = files_under(&paths.home)
        .into_iter()
        .filter(|f| !f.ends_with("state.json") && !f.ends_with("/lock"))
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
    assert!(matches!(inst.remove("demo", None), Err(InstallError::NotInstalled(_))));
    assert!(matches!(inst.rollback("demo"), Err(InstallError::NotInstalled(_))));
}

#[test]
fn rollback_needs_a_previous_version() {
    let mut s = Setup::new(Os::Linux);
    s.fx.add_release("demo", "1.0.0", AssetKind::TarGz, fake_app("demo", "1.0.0"));
    let inst = s.installer();
    s.install(&inst, "1.0.0").unwrap();
    assert!(matches!(inst.rollback("demo"), Err(InstallError::NoPrevious)));
}

#[test]
fn checksum_mismatch_installs_nothing() {
    let mut s = Setup::new(Os::Linux);
    s.fx.add_release("demo", "1.0.0", AssetKind::TarGz, fake_app("demo", "1.0.0"));
    // The server now serves different bytes than the signed catalog describes.
    let tampered = fake_app("demo", "6.6.6");
    let path = "/assets/demo-1.0.0.tar.gz";
    let mut padded = tampered;
    padded.resize(usize::try_from(s.fx.catalog.apps[0].releases[0].assets[0].size).unwrap(), 0);
    s.fx.server.put(path, padded);
    let inst = s.installer();
    let err = s.install(&inst, "1.0.0").unwrap_err();
    assert!(matches!(err, InstallError::Fetch(FetchError::Checksum { .. })), "{err}");
    assert!(s.state().apps.is_empty());
    assert!(!s.app_dir().exists());
    assert!(std::fs::symlink_metadata(s.env.paths.bin.join("demo")).is_err());
}

#[test]
fn truncated_download_resumes() {
    let mut s = Setup::new(Os::Linux);
    // Incompressible padding makes the archive big enough to cut in the middle.
    let mut noise = Vec::with_capacity(400_000);
    let mut x: u32 = 1;
    for _ in 0..400_000 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        noise.push(x.to_le_bytes()[0]);
    }
    let tgz = archive::TarGz::new()
        .dir("demo-1.0.0/")
        .file("demo-1.0.0/demo", "#!/bin/sh\necho \"demo 1.0.0\"\n", 0o755)
        .file("demo-1.0.0/data.bin", noise, 0o644)
        .build();
    let size = tgz.len();
    s.fx.add_release("demo", "1.0.0", AssetKind::TarGz, tgz);
    let path = "/assets/demo-1.0.0.tar.gz";
    s.fx.server.fault(path, Fault::TruncateAfter(size / 2), 1);
    let inst = s.installer();
    let mut progress = Vec::new();
    let app = s.fx.catalog.app("demo").unwrap();
    let r = &app.releases[0];
    inst.install(app, r, &r.assets[0], Channel::Stable, &mut |e| progress.push(e)).unwrap();
    assert_eq!(s.fx.server.ranges_requested(path), [None, Some((size / 2) as u64)]);
    assert_eq!(run(&s.env.paths.bin.join("demo")), "demo 1.0.0");
    assert!(progress.iter().any(|e| matches!(e, suite_install::Event::Download(p) if p.done == p.total)));
}

/// A tiny app whose program `exec`s a copy of `sleep` from its own folder, so the running
/// process's executable path is inside the version folder.
#[cfg(target_os = "linux")]
fn sleeper_app(version: &str) -> Vec<u8> {
    let sleep = std::fs::read("/bin/sleep").unwrap();
    let script = "#!/bin/sh\nexec \"$(dirname \"$0\")/sleep\" \"$@\"\n";
    let top = format!("demo-{version}");
    archive::TarGz::new()
        .dir(&format!("{top}/"))
        .file(&format!("{top}/demo"), script, 0o755)
        // Coreutils may be a multi-call binary dispatching on its name: keep "sleep".
        .file(&format!("{top}/sleep"), sleep, 0o755)
        .build()
}

#[cfg(target_os = "linux")]
#[test]
fn update_while_the_app_runs() {
    let mut s = Setup::new(Os::Linux);
    s.env.processes = Arc::new(SystemProcesses);
    for v in ["1.0.0", "1.1.0", "1.2.0", "1.3.0"] {
        s.fx.add_release("demo", v, AssetKind::TarGz, sleeper_app(v));
    }
    let inst = s.installer();
    s.install(&inst, "1.0.0").unwrap();
    let app_dir = s.app_dir();
    let mut child = Command::new(app_dir.join("current/demo")).arg("60").spawn().unwrap();
    // Wait until the script has exec'd the copied sleep.
    let v1 = app_dir.join("1.0.0");
    for _ in 0..300 {
        if !SystemProcesses.running_under(&v1).is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!SystemProcesses.running_under(&v1).is_empty(), "the fake app did not start");

    // Update: staged next to it, pointer switched, the running copy untouched.
    let out = s.install(&inst, "1.1.0").unwrap();
    assert!(out.needs_restart);
    assert_eq!(pointer::read(Os::Linux, &app_dir), Some("1.1.0".parse().unwrap()));
    assert!(child.try_wait().unwrap().is_none(), "the running app was disturbed");
    // Another update would normally delete 1.0.0, but it is still running.
    s.install(&inst, "1.2.0").unwrap();
    assert!(v1.join("sleep").is_file());
    assert!(s.state().apps["demo"].versions.iter().any(|v| v.to_string() == "1.0.0"));
    // Removing a running app is refused.
    assert!(matches!(inst.remove("demo", None), Err(InstallError::Running { .. })));

    child.kill().unwrap();
    child.wait().unwrap();
    s.install(&inst, "1.3.0").unwrap();
    assert!(!v1.exists(), "the old version goes once it has stopped");
    let versions: Vec<String> = s.state().apps["demo"].versions.iter().map(ToString::to_string).collect();
    assert_eq!(versions, ["1.2.0", "1.3.0"]);
}

#[test]
fn crash_mid_install_leaves_a_consistent_state() {
    for fp in
        [Failpoint::AfterDownload, Failpoint::AfterUnpack, Failpoint::AfterCommit, Failpoint::AfterSwitch]
    {
        let mut s = Setup::new(Os::Linux);
        s.fx.add_release("demo", "1.0.0", AssetKind::TarGz, fake_app("demo", "1.0.0"));
        s.fx.add_release("demo", "1.1.0", AssetKind::TarGz, fake_app("demo", "1.1.0"));
        s.install(&s.installer(), "1.0.0").unwrap();

        let crashing = s.installer().with_failpoint(Some(fp), false);
        assert!(matches!(s.install(&crashing, "1.1.0"), Err(InstallError::Failpoint(_))), "{fp:?}");

        // A fresh start repairs whatever was left.
        let inst = s.installer();
        inst.recover().unwrap();
        let st = s.state();
        let demo = &st.apps["demo"];
        let committed = matches!(fp, Failpoint::AfterCommit | Failpoint::AfterSwitch);
        assert_eq!(demo.current.to_string(), if committed { "1.1.0" } else { "1.0.0" }, "{fp:?}");
        assert_eq!(pointer::read(Os::Linux, &s.app_dir()), Some(demo.current.clone()), "{fp:?}");
        let expect = format!("demo {}", demo.current);
        assert_eq!(run(&s.app_dir().join("current/demo")), expect, "{fp:?}");
        let names: Vec<String> = std::fs::read_dir(s.app_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n.starts_with(".staging")), "{fp:?}: {names:?}");
        for n in &names {
            if let Ok(v) = n.parse::<semver::Version>() {
                assert!(demo.versions.contains(&v), "{fp:?}: orphan {n}");
            }
        }
        // And the update goes through afterwards.
        s.install(&inst, "1.1.0").unwrap();
        assert_eq!(run(&s.env.paths.bin.join("demo")), "demo 1.1.0");
    }
}

#[test]
fn crash_during_first_install_is_cleaned_up() {
    let mut s = Setup::new(Os::Linux);
    s.fx.add_release("demo", "1.0.0", AssetKind::TarGz, fake_app("demo", "1.0.0"));
    let crashing = s.installer().with_failpoint(Some(Failpoint::AfterUnpack), false);
    assert!(s.install(&crashing, "1.0.0").is_err());
    assert!(s.app_dir().exists());
    let fixed = s.installer().recover().unwrap();
    assert_ne!(fixed, [] as [std::string::String; 0]);
    assert!(!s.app_dir().exists());
    assert!(s.state().apps.is_empty());
}

#[test]
fn missing_executable_is_refused() {
    let mut s = Setup::new(Os::Linux);
    let tgz = archive::TarGz::new().dir("demo-1.0.0/").file("demo-1.0.0/other", "x", 0o755).build();
    s.fx.add_release("demo", "1.0.0", AssetKind::TarGz, tgz);
    let err = s.install(&s.installer(), "1.0.0").unwrap_err();
    assert!(matches!(err, InstallError::LayoutMissing(_)), "{err}");
    assert!(s.state().apps.is_empty());
}

#[test]
fn malicious_archive_is_refused_and_leaves_nothing() {
    let mut s = Setup::new(Os::Linux);
    let (_, evil) = archive::malicious_tars().into_iter().next().unwrap();
    s.fx.add_release("demo", "1.0.0", AssetKind::TarGz, evil);
    let err = s.install(&s.installer(), "1.0.0").unwrap_err();
    assert!(matches!(err, InstallError::Unpack(_)), "{err}");
    s.installer().recover().unwrap();
    assert!(!s.app_dir().exists());
    assert!(!s.env.paths.apps.join("evil").exists() && !s.root.path().join("evil").exists());
}

#[test]
fn leaves_files_it_did_not_create_alone() {
    let mut s = Setup::new(Os::Linux);
    s.fx.add_release("demo", "1.0.0", AssetKind::TarGz, fake_app("demo", "1.0.0"));
    let bin = s.env.paths.bin.join("demo");
    std::fs::create_dir_all(&s.env.paths.bin).unwrap();
    std::fs::write(&bin, "#!/bin/sh\necho mine\n").unwrap();
    let desktop = s.env.paths.xdg_data.join("applications/com.example.demo.desktop");
    std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
    std::fs::write(&desktop, "[Desktop Entry]\nName=Mine\n").unwrap();
    let inst = s.installer();
    s.install(&inst, "1.0.0").unwrap();
    assert_eq!(std::fs::read_to_string(&bin).unwrap(), "#!/bin/sh\necho mine\n");
    assert_eq!(std::fs::read_to_string(&desktop).unwrap(), "[Desktop Entry]\nName=Mine\n");
    inst.remove("demo", None).unwrap();
    assert!(bin.exists() && desktop.exists());
}

#[test]
fn a_second_operation_waits_its_turn() {
    let mut s = Setup::new(Os::Linux);
    s.fx.add_release("demo", "1.0.0", AssetKind::TarGz, fake_app("demo", "1.0.0"));
    let inst = s.installer();
    let other = s.installer().with_lock_wait(Duration::from_millis(50));
    let r = inst.update_state(|_| other.recover()).unwrap();
    assert!(matches!(r, Err(InstallError::Busy)));
}

fn windows_setup() -> (Setup, std::path::PathBuf) {
    let mut s = Setup::new(Os::Windows);
    let zip = |v: &str| {
        archive::Zip::new()
            .dir(&format!("Demo-{v}-windows-x64/"))
            .file(&format!("Demo-{v}-windows-x64/bin/demo.exe"), format!("MZ {v}"), 0o644)
            .file(&format!("Demo-{v}-windows-x64/README.txt"), "x".repeat(3000), 0o644)
            .build()
    };
    for v in ["1.0.0", "1.1.0"] {
        s.fx.add_release("demo", v, AssetKind::Zip, zip(v));
        let app = s.fx.catalog.apps.iter_mut().find(|a| a.id == "demo").unwrap();
        app.name = "Demo: the app".into();
        let r = app.releases.iter_mut().find(|r| r.version.to_string() == v).unwrap();
        r.assets[0].os = Os::Windows;
        r.assets[0].layout = suite_catalog::Layout {
            executable: Some("bin/demo.exe".into()),
            file_types: vec!["demo".into(), ".DMO".into(), "../bad".into()],
            ..Default::default()
        };
    }
    // The shim that would ship next to stockroom.exe.
    let shim = s.root.path().join("build/stockroom-shim.exe");
    std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
    std::fs::write(&shim, "SHIM").unwrap();
    (s, shim)
}

/// Every key and value in the fake registry, for "removal left exactly what was there".
fn registry_dump(env: &Env) -> Vec<String> {
    let mut out = Vec::new();
    for hive in [Hive::CurrentUser, Hive::LocalMachine] {
        let mut stack = vec![String::new()];
        while let Some(key) = stack.pop() {
            for name in env.registry.value_names(hive, &key).unwrap() {
                out.push(format!("{hive:?}\\{key} [{name}]"));
            }
            for sub in env.registry.subkeys(hive, &key).unwrap() {
                let k = if key.is_empty() { sub } else { format!(r"{key}\{sub}") };
                out.push(format!("{hive:?}\\{k}"));
                stack.push(k);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn windows_integration_simulated_on_this_machine() {
    let (s, shim) = windows_setup();
    let reg = &s.env.registry;
    // Someone else's "Open with" entry and an unrelated app's uninstall key, before us.
    let others = r"Software\Classes\.demo\OpenWithProgids";
    reg.set(Hive::CurrentUser, others, "Other.Viewer", Value::String(String::new())).unwrap();
    let foreign = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Other";
    reg.set(Hive::CurrentUser, foreign, "DisplayName", Value::String("Other".into())).unwrap();
    let before = registry_dump(&s.env);

    let inst = s.installer().with_shim(Some(shim));
    s.install(&inst, "1.0.0").unwrap();
    let paths = &s.env.paths;
    assert!(paths.apps.ends_with("AppData/Local/Programs/Stockroom"));
    let app_dir = s.app_dir();
    assert_eq!(std::fs::read_to_string(app_dir.join("current.txt")).unwrap(), "1.0.0\nbin/demo.exe\n");
    // The launcher shim sits beside the pointer; the shortcut starts it, with the app's icon.
    let launcher = app_dir.join("demo.exe");
    assert_eq!(std::fs::read_to_string(&launcher).unwrap(), "SHIM");
    let lnk = paths.start_menu.join("Demo_ the app.lnk");
    let shortcut = std::fs::read_to_string(&lnk).unwrap();
    assert!(shortcut.contains(&format!("shortcut -> {}\n", launcher.display())), "{shortcut}");
    assert!(shortcut.contains("1.0.0/bin/demo.exe"), "{shortcut}");
    let key = suite_install::integrate::uninstall_key("demo");
    assert_eq!(reg.get_string(Hive::CurrentUser, &key, "DisplayVersion").as_deref(), Some("1.0.0"));
    assert!(reg.get_string(Hive::CurrentUser, &key, "UninstallString").unwrap().ends_with(" remove demo"));
    assert_eq!(reg.get(Hive::CurrentUser, &key, "NoModify").unwrap(), Some(Value::Dword(1)));
    assert_eq!(reg.get(Hive::CurrentUser, &key, "EstimatedSize").unwrap(), Some(Value::Dword(3)));
    // No file associations unless asked for.
    let pid = suite_install::integrate::prog_id("demo");
    assert!(!reg.exists(Hive::CurrentUser, &format!(r"Software\Classes\{pid}")).unwrap());

    // Asked for: a ProgID through the shim, and "Open with" entries beside the other app's.
    let types = inst.set_file_associations("demo", true).unwrap();
    assert_eq!(types.len(), 3);
    let command = format!(r"Software\Classes\{pid}\shell\open\command");
    assert_eq!(
        reg.get_string(Hive::CurrentUser, &command, "").unwrap(),
        format!("\"{}\" \"%1\"", launcher.display())
    );
    assert!(reg.get(Hive::CurrentUser, others, &pid).unwrap().is_some());
    assert!(reg.get(Hive::CurrentUser, others, "Other.Viewer").unwrap().is_some());
    assert!(reg.get(Hive::CurrentUser, r"Software\Classes\.dmo\OpenWithProgids", &pid).unwrap().is_some());
    assert!(!reg.exists(Hive::CurrentUser, r"Software\Classes\.bad").unwrap());
    assert!(s.state().apps["demo"].associate);

    // An update keeps the shim and shortcut target, switches the pointer and the icon.
    s.install(&inst, "1.1.0").unwrap();
    assert_eq!(std::fs::read_to_string(app_dir.join("current.txt")).unwrap(), "1.1.0\nbin/demo.exe\n");
    let shortcut = std::fs::read_to_string(&lnk).unwrap();
    assert!(shortcut.contains(&format!("shortcut -> {}\n", launcher.display())));
    assert!(shortcut.contains("1.1.0/bin/demo.exe"));
    assert_eq!(reg.get_string(Hive::CurrentUser, &key, "DisplayVersion").as_deref(), Some("1.1.0"));
    assert!(reg.get(Hive::CurrentUser, others, &pid).unwrap().is_some(), "associations survive updates");
    inst.rollback("demo").unwrap();
    assert_eq!(reg.get_string(Hive::CurrentUser, &key, "DisplayVersion").as_deref(), Some("1.0.0"));
    assert_eq!(std::fs::read_to_string(app_dir.join("current.txt")).unwrap(), "1.0.0\nbin/demo.exe\n");

    // Turning associations off removes only ours.
    inst.set_file_associations("demo", false).unwrap();
    assert!(!reg.exists(Hive::CurrentUser, &format!(r"Software\Classes\{pid}")).unwrap());
    assert!(!reg.exists(Hive::CurrentUser, r"Software\Classes\.dmo").unwrap());
    assert!(reg.get(Hive::CurrentUser, others, "Other.Viewer").unwrap().is_some());
    inst.set_file_associations("demo", true).unwrap();

    // Removal leaves the registry and Start menu exactly as they were.
    inst.remove("demo", None).unwrap();
    assert!(!lnk.exists());
    assert!(!app_dir.exists());
    assert_eq!(registry_dump(&s.env), before);
    assert_eq!(files_under(&paths.start_menu), [] as [String; 0]);
}

#[test]
fn windows_without_a_shim_and_with_a_foreign_shortcut() {
    let (s, _shim) = windows_setup();
    let lnk = s.env.paths.start_menu.join("Demo_ the app.lnk");
    std::fs::create_dir_all(lnk.parent().unwrap()).unwrap();
    std::fs::write(&lnk, "someone else's").unwrap();
    let inst = s.installer().with_shim(None);
    s.install(&inst, "1.0.0").unwrap();
    // Not ours: left alone, and not recorded.
    assert_eq!(std::fs::read_to_string(&lnk).unwrap(), "someone else's");
    assert!(!s.state().apps["demo"].created_path(&lnk));
    std::fs::remove_file(&lnk).unwrap();
    // Without a shim the shortcut targets the version folder and follows each switch.
    s.install(&inst, "1.1.0").unwrap();
    let shortcut = std::fs::read_to_string(&lnk).unwrap();
    assert!(shortcut.starts_with("shortcut -> ") && shortcut.contains("1.1.0/bin/demo.exe\n"), "{shortcut}");
    assert!(!s.app_dir().join("demo.exe").exists());
    inst.remove("demo", None).unwrap();
    assert!(!lnk.exists());
    // File types are Windows' business only.
    let linux = Setup::new(Os::Linux);
    assert!(matches!(
        linux.installer().set_file_associations("demo", true),
        Err(InstallError::Unsupported(_))
    ));
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

/// A fake macOS: `hdiutil attach` mounts `image` (fills the mount point with its contents),
/// `detach` empties it, `ditto` copies.
fn fake_mac(image: &Path) -> Arc<FakeRunner> {
    let runner = Arc::new(FakeRunner::new());
    let image = image.to_owned();
    runner.on_call(&["hdiutil", "attach"], move |line| {
        let at = line.iter().position(|a| a == "-mountpoint").unwrap();
        copy_dir(&image, Path::new(&line[at + 1]));
        Output::ok("/dev/disk4\tGUID_partition_scheme\n")
    });
    runner.on_call(&["hdiutil", "detach"], |line| {
        let mount = Path::new(line.last().unwrap());
        for e in std::fs::read_dir(mount).unwrap().flatten() {
            std::fs::remove_dir_all(e.path()).unwrap();
        }
        Output::ok("\"disk4\" ejected.\n")
    });
    runner.on_call(&["ditto", "--noqtn"], |line| {
        copy_dir(Path::new(&line[2]), Path::new(&line[3]));
        Output::ok("")
    });
    runner.on(&["open"], Output::ok(""));
    runner
}

fn mac_bundle(dir: &Path, id: &str, version: &str) {
    let app = dir.join("Demo.app/Contents");
    std::fs::create_dir_all(app.join("MacOS")).unwrap();
    std::fs::write(app.join("MacOS/demo"), format!("bin {version}")).unwrap();
    std::fs::write(app.join("Info.plist"), suite_platform::macos::info_plist(id, version, "demo")).unwrap();
}

fn mac_setup() -> (Setup, Arc<FakeRunner>, std::path::PathBuf) {
    let mut s = Setup::new(Os::Macos);
    let image = s.root.path().join("image");
    mac_bundle(&image, "com.example.demo", "1.0.0");
    let runner = fake_mac(&image);
    s.env.runner = runner.clone();
    for v in ["1.0.0", "1.1.0"] {
        s.fx.add_release("demo", v, AssetKind::Dmg, format!("dmg {v}").into_bytes());
        let app = s.fx.catalog.apps.iter_mut().find(|a| a.id == "demo").unwrap();
        app.ids.macos_bundle = Some("com.example.demo".into());
        let r = app.releases.iter_mut().find(|r| r.version.to_string() == v).unwrap();
        r.assets[0].os = Os::Macos;
    }
    (s, runner, image)
}

#[test]
fn macos_dmg_is_mounted_copied_and_detached() {
    let (s, runner, image) = mac_setup();
    let inst = s.installer();
    s.install(&inst, "1.0.0").unwrap();
    let st = s.state();
    assert_eq!(st.apps["demo"].kind, InstallKind::MacBundle);
    assert_eq!(st.apps["demo"].executable.as_deref(), Some("Demo.app"));
    let calls = runner.calls();
    let programs: Vec<&str> = calls.iter().map(|c| c[0].as_str()).collect();
    assert_eq!(programs, ["hdiutil", "ditto", "hdiutil", "ditto"], "{calls:?}");
    assert_eq!(calls[0][1..4], ["attach", "-nobrowse", "-readonly"]);
    assert_eq!(calls[2][1], "detach");
    let copy = s.env.paths.mac_applications.join("Demo.app");
    assert!(copy.join("Contents/MacOS/demo").is_file());
    assert!(s.app_dir().join("1.0.0/Demo.app/Contents/Info.plist").is_file());
    let (program, args) = inst.launch_command("demo").unwrap();
    assert_eq!((program.as_str(), args[0].to_str().unwrap()), ("open", copy.to_str().unwrap()));

    // An update replaces the copy; the old version folder stays for rollback.
    mac_bundle(&image, "com.example.demo", "1.1.0");
    s.install(&inst, "1.1.0").unwrap();
    assert_eq!(std::fs::read_to_string(copy.join("Contents/MacOS/demo")).unwrap(), "bin 1.1.0");
    inst.rollback("demo").unwrap();
    assert_eq!(std::fs::read_to_string(copy.join("Contents/MacOS/demo")).unwrap(), "bin 1.0.0");
    assert_eq!(files_under(&s.env.paths.mac_applications).len(), 2, "no staging leftovers");

    inst.remove("demo", None).unwrap();
    assert!(!copy.exists());
    assert!(!s.app_dir().exists());
    assert!(!s.env.paths.data.join("mount-demo").exists());
}

#[test]
fn macos_refuses_a_bundle_with_another_identity() {
    let (s, runner, image) = mac_setup();
    mac_bundle(&image, "com.evil.demo", "1.0.0");
    let err = s.install(&s.installer(), "1.0.0").unwrap_err();
    assert!(err.to_string().contains("com.evil.demo"), "{err}");
    assert!(runner.ran(&["hdiutil", "detach"]));
    assert!(!s.env.paths.mac_applications.join("Demo.app").exists());
    assert!(s.state().apps.is_empty());
    assert!(!s.app_dir().join("1.0.0").exists());
}

#[test]
fn macos_leaves_a_users_own_copy_and_can_install_for_everyone() {
    let (s, _runner, _image) = mac_setup();
    let own = s.env.paths.mac_applications.join("Demo.app");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("mine"), "dragged in by the user").unwrap();
    let inst = s.installer();
    s.install(&inst, "1.0.0").unwrap();
    assert_eq!(files_under(&own), ["mine"]);
    // It opens from its version folder instead.
    let (_, args) = inst.launch_command("demo").unwrap();
    assert!(args[0].to_str().unwrap().ends_with("demo/1.0.0/Demo.app"));
    inst.remove("demo", None).unwrap();
    assert_eq!(files_under(&own), ["mine"]);

    // /Applications, when chosen.
    inst.update_state(|st| st.settings.scope = suite_install::Scope::AllUsers).unwrap();
    std::fs::create_dir_all(s.env.paths.sys("Applications")).unwrap();
    s.install(&inst, "1.0.0").unwrap();
    let shared = s.env.paths.sys("Applications/Demo.app");
    assert!(shared.join("Contents/Info.plist").is_file());
    inst.remove("demo", None).unwrap();
    assert!(!shared.exists());
    assert!(s.env.paths.sys("Applications").is_dir());
}

#[test]
fn macos_bundle_from_a_zip_is_copied_to_applications() {
    let mut s = Setup::new(Os::Macos);
    let runner = fake_mac(&s.root.path().join("no-image"));
    s.env.runner = runner.clone();
    let plist = suite_platform::macos::info_plist("com.example.demo", "1.0.0", "demo");
    let zip = archive::Zip::new()
        .file("Demo.app/Contents/MacOS/demo", "bin", 0o755)
        .file("Demo.app/Contents/Info.plist", plist, 0o644)
        .build();
    s.fx.add_release("demo", "1.0.0", AssetKind::Zip, zip);
    {
        let app = &mut s.fx.catalog.apps[0];
        app.ids.macos_bundle = Some("com.example.demo".into());
        let r = &mut app.releases[0];
        r.assets[0].os = Os::Macos;
        r.assets[0].layout =
            suite_catalog::Layout { executable: Some("Demo.app".into()), ..Default::default() };
    }
    let inst = s.installer();
    s.install(&inst, "1.0.0").unwrap();
    let st = s.state();
    assert_eq!(st.apps["demo"].kind, InstallKind::MacBundle);
    let ditto = runner.calls().into_iter().find(|c| c[0] == "ditto").unwrap();
    assert!(ditto[2].ends_with("demo/1.0.0/Demo.app"));
    assert!(ditto[3].starts_with(&s.env.paths.mac_applications.display().to_string()));
    assert!(!runner.ran(&["hdiutil"]));
    let copy = s.env.paths.mac_applications.join("Demo.app");
    assert!(copy.join("Contents/MacOS/demo").is_file());
    inst.remove("demo", None).unwrap();
    assert!(!copy.exists());
}

#[test]
fn appimage_and_flatpak_installs() {
    let mut s = Setup::new(Os::Linux);
    let runner = Arc::new(FakeRunner::new());
    runner.on(&["flatpak"], Output::ok(""));
    s.env.runner = runner.clone();
    let mut appimage = b"\x7fELF\x02\x01\x01\x00AI\x02".to_vec();
    appimage.extend_from_slice(&[0u8; 100]);
    s.fx.add_release("demo", "1.0.0", AssetKind::AppImage, appimage);
    let inst = s.installer();
    s.install(&inst, "1.0.0").unwrap();
    let st = s.state();
    assert_eq!(st.apps["demo"].kind, InstallKind::AppImage);
    let link = std::fs::read_link(s.env.paths.bin.join("demo")).unwrap();
    assert!(link.ends_with("demo/current/demo.AppImage"));
    inst.remove("demo", None).unwrap();

    s.fx.add_release("demo", "2.0.0", AssetKind::FlatpakBundle, b"flatpak bundle".to_vec());
    s.install(&inst, "2.0.0").unwrap();
    assert!(runner.ran(&["flatpak", "install", "--user", "--noninteractive", "-y", "--bundle"]));
    assert_eq!(s.state().apps["demo"].kind, InstallKind::Flatpak);
    assert!(matches!(inst.rollback("demo"), Err(InstallError::Unsupported(_))));
    inst.remove("demo", None).unwrap();
    assert!(runner.ran(&["flatpak", "uninstall", "--user"]));
    assert!(s.state().apps.is_empty());
}

#[test]
fn custom_exe_content_runs() {
    let mut s = Setup::new(Os::Linux);
    s.fx.add_release(
        "demo",
        "1.0.0",
        AssetKind::TarGz,
        fake_app_with_exe("demo", "1.0.0", b"#!/bin/sh\necho custom\n".to_vec()),
    );
    s.install(&s.installer(), "1.0.0").unwrap();
    assert_eq!(run(&s.env.paths.bin.join("demo")), "custom");
}

#[test]
fn a_pinned_download_that_changed_upstream_installs_nothing() {
    let mut s = Setup::new(Os::Linux);
    let mut appimage = b"\x7fELF\x02\x01\x01\x00AI\x02".to_vec();
    appimage.extend_from_slice(&[1u8; 200]);
    s.fx.add_release("demo", "2026.11.15+build.091ee20dff0c", AssetKind::AppImage, appimage);
    {
        let a = &mut s.fx.catalog.apps[0].releases[0].assets[0];
        a.pinned = true;
        a.note = Some("no checksum published; pinned at publish time".into());
    }
    // The vendor's "latest" URL now serves a new build (same and different sizes).
    let path = "/assets/demo-2026.11.15+build.091ee20dff0c.appimage";
    for new_build in [vec![2u8; 211], vec![3u8; 999]] {
        s.fx.server.put(path, new_build);
        let err = s.install(&s.installer(), "2026.11.15+build.091ee20dff0c").unwrap_err();
        assert!(matches!(err, InstallError::PinnedChanged { .. }), "{err}");
        assert!(err.to_string().contains("demo released a new build; the catalog needs refreshing"), "{err}");
        assert!(s.state().apps.is_empty());
        assert!(!s.app_dir().exists());
        assert_eq!(std::fs::read_dir(&s.env.paths.downloads).map_or(0, Iterator::count), 0);
    }
    s.fx.server.remove(path);
    let err = s.install(&s.installer(), "2026.11.15+build.091ee20dff0c").unwrap_err();
    assert!(matches!(err, InstallError::PinnedChanged { .. }), "{err}");
}

#[test]
fn store_apps_open_the_store_and_are_not_tracked() {
    let mut s = Setup::new(Os::Windows);
    let runner = Arc::new(FakeRunner::new());
    runner.on(&["explorer.exe"], Output::failed(1, ""));
    s.env.runner = runner.clone();
    s.fx.add_release("demo", "1.0.0", AssetKind::Zip, b"unused".to_vec());
    {
        let a = &mut s.fx.catalog.apps[0].releases[0].assets[0];
        a.kind = AssetKind::Store;
        a.os = Os::Windows;
        a.sha256 = None;
        a.size = 0;
        a.store = Some(suite_catalog::StoreLink {
            store: suite_catalog::Store::Microsoft,
            product_id: "9PP9CNT87P3F".into(),
            url: "https://apps.microsoft.com/store/detail/zynk/9PP9CNT87P3F".into(),
            link: "ms-windows-store://pdp/?productid=9PP9CNT87P3F".into(),
        });
    }
    let out = s.install(&s.installer(), "1.0.0").unwrap();
    assert_eq!(out.opened_store, Some(suite_catalog::Store::Microsoft));
    assert_eq!(runner.calls(), [["explorer.exe", "ms-windows-store://pdp/?productid=9PP9CNT87P3F"]]);
    assert!(s.state().apps.is_empty());
    assert_eq!(s.fx.server.hits("/assets/demo-1.0.0.zip"), 0);
}
