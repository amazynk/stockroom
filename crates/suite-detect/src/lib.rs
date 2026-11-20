//! Finding copies of the family's apps installed some other way (PLAN.md §4): a distro
//! package or a binary on PATH, a Flatpak, an `.app` bundle, an MSI or other Windows
//! installer, a Homebrew cask, winget. They are shown as "installed by <source>" and never
//! touched. Everything goes through the [`Env`]: paths under its root, commands through
//! its runner, the registry through its registry.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use suite_catalog::{App, Os};
use suite_platform::{Env, Hive, registry::UNINSTALL_KEY, runner::args};

/// Where a copy came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// An executable on PATH (a package, a manual install).
    Path,
    /// `/usr/bin`: the distribution's package manager.
    SystemPackage,
    Flatpak {
        user: bool,
    },
    /// An `.app` bundle in `/Applications` or `~/Applications`.
    MacApp,
    /// An `.app` bundle with an App Store receipt.
    MacAppStore,
    /// A Microsoft Store (AppX/MSIX) package.
    MicrosoftStore,
    Homebrew,
    /// An entry in the Windows uninstall list (MSI or another installer).
    WindowsInstaller {
        machine: bool,
    },
    Winget,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Path => "a program on PATH",
            Self::SystemPackage => "the system package manager",
            Self::Flatpak { user: true } => "Flatpak (user)",
            Self::Flatpak { user: false } => "Flatpak (system)",
            Self::MacApp => "an app bundle",
            Self::MacAppStore => "the Mac App Store",
            Self::MicrosoftStore => "the Microsoft Store",
            Self::Homebrew => "Homebrew",
            Self::WindowsInstaller { machine: true } => "an installer (all users)",
            Self::WindowsInstaller { machine: false } => "an installer (this user)",
            Self::Winget => "winget",
        })
    }
}

/// One copy found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub source: Source,
    pub version: Option<String>,
    /// The executable, bundle or install folder, when known.
    pub location: Option<PathBuf>,
}

/// All copies of `app` not made by Stockroom.
pub fn detect(env: &Env, app: &App) -> Vec<Found> {
    let mut out = match env.os {
        Os::Linux => linux(env, app),
        Os::Macos => macos(env, app),
        Os::Windows => windows(env, app),
    };
    out.dedup();
    out
}

/// Whether `p` belongs to Stockroom (its apps folder or a link into it).
fn is_ours(env: &Env, p: &Path) -> bool {
    let apps = &env.paths.apps;
    p.starts_with(apps)
        || std::fs::canonicalize(p).is_ok_and(|real| {
            real.starts_with(apps)
                || real.starts_with(std::fs::canonicalize(apps).unwrap_or_else(|_| apps.clone()))
        })
}

fn linux(env: &Env, app: &App) -> Vec<Found> {
    let mut out = Vec::new();
    let usr_bin = env.paths.sys("usr/bin");
    for exe in &app.ids.executables {
        let mut dirs: Vec<PathBuf> = env.paths.path_dirs.clone();
        if !dirs.contains(&usr_bin) {
            dirs.push(usr_bin.clone());
        }
        for dir in dirs {
            let p = dir.join(exe);
            if p.is_file() && !is_ours(env, &p) {
                let source = if dir == usr_bin { Source::SystemPackage } else { Source::Path };
                if !out.iter().any(|f: &Found| f.location.as_ref() == Some(&p)) {
                    out.push(Found { source, version: None, location: Some(p) });
                }
            }
        }
    }
    if let Some(id) = app.ids.flatpak.as_ref().or(app.ids.desktop.as_ref())
        && env.paths.which("flatpak").is_some()
    {
        for (flag, user) in [("--user", true), ("--system", false)] {
            if let Ok(o) = env.runner.run("flatpak", &args(["info", flag, id.as_str()]))
                && o.success
            {
                let version = suite_unpack::flatpak::parse_info_version(&o.stdout);
                out.push(Found { source: Source::Flatpak { user }, version, location: None });
            }
        }
    }
    out
}

pub use suite_platform::macos::{BundleInfo, bundle_info};

fn macos(env: &Env, app: &App) -> Vec<Found> {
    let mut out = Vec::new();
    if let Some(bundle_id) = &app.ids.macos_bundle {
        for dir in [env.paths.sys("Applications"), env.paths.mac_applications.clone()] {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "app") {
                    continue;
                }
                if let Some(BundleInfo { id, version, .. }) = bundle_info(&p)
                    && id == *bundle_id
                {
                    let source = if p.join("Contents").join("_MASReceipt").exists() {
                        Source::MacAppStore
                    } else {
                        Source::MacApp
                    };
                    out.push(Found { source, version, location: Some(p) });
                }
            }
        }
    }
    if let Some(cask) = &app.ids.homebrew_cask {
        for prefix in ["opt/homebrew/Caskroom", "usr/local/Caskroom"] {
            let dir = env.paths.sys(prefix).join(cask);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let version = entries
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .find(|n| !n.starts_with('.'));
                out.push(Found { source: Source::Homebrew, version, location: Some(dir) });
            }
        }
        // A Homebrew elsewhere (a custom prefix): ask it.
        if !out.iter().any(|f| f.source == Source::Homebrew)
            && env.paths.which("brew").is_some()
            && let Ok(o) = env.runner.run("brew", &args(["list", "--cask", "--versions", cask.as_str()]))
            && o.success
            && let Some(version) = parse_brew_version(&o.stdout, cask)
        {
            out.push(Found { source: Source::Homebrew, version: Some(version), location: None });
        }
    }
    out
}

/// The newest version `brew list --cask --versions <cask>` prints (`demo 1.9 2.0`).
pub fn parse_brew_version(out: &str, cask: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let mut toks = l.split_whitespace();
        (toks.next()? == cask).then_some(())?;
        toks.last().map(str::to_owned)
    })
}

fn windows(env: &Env, app: &App) -> Vec<Found> {
    let mut out = Vec::new();
    let ours = format!("{}.{}", suite_identity::APP_ID, app.id);
    let wanted = app.ids.windows_uninstall_key.as_deref();
    let roots = [
        (Hive::LocalMachine, UNINSTALL_KEY.to_owned(), true),
        (
            Hive::LocalMachine,
            r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall".to_owned(),
            true,
        ),
        (Hive::CurrentUser, UNINSTALL_KEY.to_owned(), false),
    ];
    for (hive, root, machine) in roots {
        let Ok(keys) = env.registry.subkeys(hive, &root) else { continue };
        for k in keys {
            if k.eq_ignore_ascii_case(&ours) {
                continue;
            }
            let key = format!(r"{root}\{k}");
            let name = env.registry.get_string(hive, &key, "DisplayName");
            let matches = wanted.is_some_and(|w| {
                k.eq_ignore_ascii_case(w) || name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(w))
            }) || name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&app.name));
            if matches {
                let version = env.registry.get_string(hive, &key, "DisplayVersion");
                let location = env
                    .registry
                    .get_string(hive, &key, "InstallLocation")
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from);
                out.push(Found { source: Source::WindowsInstaller { machine }, version, location });
            }
        }
    }
    if let Some(family) = &app.ids.appx_family
        && let Some(found) = appx(env, family)
    {
        out.push(found);
    }
    if let Some(id) = &app.ids.winget
        && let Ok(o) = env.runner.run(
            "winget",
            &args([
                "list",
                "--id",
                id.as_str(),
                "--exact",
                "--accept-source-agreements",
                "--disable-interactivity",
            ]),
        )
        && o.success
        && let Some(version) = parse_winget_version(&o.stdout, id)
    {
        out.push(Found { source: Source::Winget, version: Some(version), location: None });
    }
    out
}

/// Per-user AppX/MSIX packages, keyed by package full name.
pub const APPX_PACKAGES: &str =
    r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";

/// A package of `family` (`Name_PublisherId`) among the user's `AppX` packages. Full names
/// are `Name_Version_Arch_ResourceId_PublisherId`.
fn appx(env: &Env, family: &str) -> Option<Found> {
    let (name, publisher) = family.rsplit_once('_')?;
    let keys = env.registry.subkeys(Hive::CurrentUser, APPX_PACKAGES).ok()?;
    let full = keys.into_iter().find(|k| {
        k.strip_prefix(name)
            .and_then(|r| r.strip_prefix('_'))
            .is_some_and(|r| r.ends_with(&format!("_{publisher}")))
    })?;
    let version = full.split('_').nth(1).map(str::to_owned);
    Some(Found { source: Source::MicrosoftStore, version, location: None })
}

/// The version column of `winget list` for `id`: the token after the ID on its row.
pub fn parse_winget_version(out: &str, id: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let mut toks = l.split_whitespace();
        toks.by_ref().find(|t| t.eq_ignore_ascii_case(id))?;
        toks.next().map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use suite_platform::{FakeRunner, Output, registry::Value};
    use suite_testkit::app_entry;

    use super::*;

    fn app() -> App {
        let mut a = app_entry("demo");
        a.name = "Demo".into();
        a.ids.macos_bundle = Some("com.example.demo".into());
        a.ids.homebrew_cask = Some("demo".into());
        a.ids.windows_uninstall_key = Some("Demo".into());
        a.ids.winget = Some("Example.Demo".into());
        a.ids.appx_family = Some("Example.Demo_b5p39ft652y30".into());
        a
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn linux_path_usr_bin_and_flatpak() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = Env::for_tests(dir.path(), Os::Linux);
        let runner = Arc::new(FakeRunner::new());
        runner.on(
            &["flatpak", "info", "--system", "com.example.demo"],
            Output::ok("ID: com.example.demo\nVersion: 3.1\n"),
        );
        runner.on(&["flatpak", "info", "--user"], Output::failed(1, "not installed"));
        env.runner = runner;
        assert_eq!(detect(&env, &app()), [] as [Found; 0]);
        touch(&env.paths.sys("usr/bin/demo"));
        touch(&env.paths.sys("usr/bin/flatpak"));
        touch(&env.paths.sys("usr/local/bin/demo"));
        // Stockroom's own link is not "another install".
        let ours = env.paths.apps.join("demo/1.0.0/demo");
        touch(&ours);
        env.paths.path_dirs.push(env.paths.bin.clone());
        std::fs::create_dir_all(&env.paths.bin).unwrap();
        std::os::unix::fs::symlink(&ours, env.paths.bin.join("demo")).unwrap();
        let found = detect(&env, &app());
        let sources: Vec<_> = found.iter().map(|f| f.source.clone()).collect();
        assert_eq!(
            sources,
            [Source::Path, Source::SystemPackage, Source::Flatpak { user: false }],
            "{found:?}"
        );
        assert_eq!(found[2].version.as_deref(), Some("3.1"));
        assert_eq!(found[1].source.to_string(), "the system package manager");
    }

    #[test]
    fn macos_bundles_and_homebrew() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::for_tests(dir.path(), Os::Macos);
        let plist = |id: &str| {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
                   <key>CFBundleIdentifier</key><string>{id}</string>
                   <key>CFBundleShortVersionString</key><string>2.0</string></dict></plist>"#
            )
        };
        let a = env.paths.sys("Applications/Demo.app/Contents/Info.plist");
        touch(&a);
        std::fs::write(&a, plist("com.example.demo")).unwrap();
        let b = env.paths.mac_applications.join("Other.app/Contents/Info.plist");
        touch(&b);
        std::fs::write(&b, plist("com.example.other")).unwrap();
        touch(&env.paths.sys("opt/homebrew/Caskroom/demo/1.9/Demo.app"));
        // An App Store copy has a receipt.
        let c = env.paths.sys("Applications/Demo Store.app/Contents/Info.plist");
        touch(&c);
        std::fs::write(&c, plist("com.example.demo")).unwrap();
        touch(&env.paths.sys("Applications/Demo Store.app/Contents/_MASReceipt/receipt"));
        let found = detect(&env, &app());
        assert_eq!(found.len(), 3, "{found:?}");
        let sources: Vec<Source> = found.iter().map(|f| f.source.clone()).collect();
        assert!(sources.contains(&Source::MacApp) && sources.contains(&Source::MacAppStore), "{found:?}");
        assert_eq!((found[2].source.clone(), found[2].version.as_deref()), (Source::Homebrew, Some("1.9")));
    }

    #[test]
    fn homebrew_in_a_custom_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = Env::for_tests(dir.path(), Os::Macos);
        let runner = Arc::new(FakeRunner::new());
        runner.on(&["brew", "list", "--cask", "--versions", "demo"], Output::ok("demo 1.9 2.0\n"));
        env.runner = runner.clone();
        assert_eq!(detect(&env, &app()), [] as [Found; 0], "no brew on PATH: not asked");
        assert!(!runner.ran(&["brew"]));
        touch(&env.paths.sys("usr/local/bin/brew"));
        let found = detect(&env, &app());
        assert_eq!(found, [Found { source: Source::Homebrew, version: Some("2.0".into()), location: None }]);
        assert_eq!(parse_brew_version("Error: Cask 'x' is not installed.", "x"), None);
    }

    #[test]
    fn windows_uninstall_keys_and_winget() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = Env::for_tests(dir.path(), Os::Windows);
        let runner = Arc::new(FakeRunner::new());
        runner.on(
            &["winget", "list"],
            Output::ok("Name   Id             Version  Source\n-----------------------------\nDemo   Example.Demo   2.4.1    winget\n"),
        );
        env.runner = runner;
        let r = &env.registry;
        let msi = format!(r"{UNINSTALL_KEY}\{{1234-ABCD}}");
        r.set(Hive::LocalMachine, &msi, "DisplayName", Value::String("Demo".into())).unwrap();
        r.set(Hive::LocalMachine, &msi, "DisplayVersion", Value::String("2.4.1".into())).unwrap();
        let ours = format!(r"{UNINSTALL_KEY}\{}.demo", suite_identity::APP_ID);
        r.set(Hive::CurrentUser, &ours, "DisplayName", Value::String("Demo".into())).unwrap();
        let other = format!(r"{UNINSTALL_KEY}\Unrelated");
        r.set(Hive::CurrentUser, &other, "DisplayName", Value::String("Unrelated".into())).unwrap();
        let pkg = format!(r"{APPX_PACKAGES}\Example.Demo_10.0.36.0_x64__b5p39ft652y30");
        r.set(Hive::CurrentUser, &pkg, "DisplayName", Value::String("Demo".into())).unwrap();
        let other_pkg = format!(r"{APPX_PACKAGES}\Example.Demo.Beta_1.0.0.0_x64__b5p39ft652y30");
        r.set(Hive::CurrentUser, &other_pkg, "DisplayName", Value::String("Beta".into())).unwrap();
        let found = detect(&env, &app());
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(found[2].source, Source::Winget);
        let store = found.iter().find(|f| f.source == Source::MicrosoftStore).unwrap();
        assert_eq!(store.version.as_deref(), Some("10.0.36.0"));
        assert_eq!(found[0].source, Source::WindowsInstaller { machine: true });
        assert_eq!(found[0].version.as_deref(), Some("2.4.1"));
        assert_eq!(parse_winget_version("No installed package found", "X"), None);
    }
}
