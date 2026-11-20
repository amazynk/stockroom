//! Stockroom's own place on the computer.
//!
//! Stockroom is handed out as a download that people run from wherever it landed, often only
//! once. What it registers for later (the daily update check, the uninstall entries of the
//! apps it installs) has to name a program that is still there after the download is tidied
//! away. So when it is started from the user's folders it keeps a copy of itself in its data
//! folder, `<data>/bin/stockroom`, registers that one, and gives it a way to be found again:
//! the `stockroom` command and an application-menu entry with Stockroom's icon on Linux, a
//! Start-menu shortcut on Windows. A newer download replaces the copy; an older one never
//! does.
//!
//! [`unsettle`] takes all of it away again.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use suite_catalog::Os;
use suite_identity::{APP_ID, NAME, SLUG};
use suite_platform::Env;

use crate::{InstallError, integrate, state::write_atomic};

/// Where the kept copy lives.
pub fn home_exe(env: &Env) -> PathBuf {
    let name = if env.os == Os::Windows { format!("{SLUG}.exe") } else { SLUG.to_owned() };
    bin_dir(env).join(name)
}

fn bin_dir(env: &Env) -> PathBuf {
    env.paths.data.join("bin")
}

fn version_file(env: &Env) -> PathBuf {
    bin_dir(env).join("version")
}

fn command_link(env: &Env) -> PathBuf {
    env.paths.bin.join(SLUG)
}

fn desktop_file(env: &Env) -> PathBuf {
    env.paths.xdg_data.join("applications").join(format!("{APP_ID}.desktop"))
}

fn start_menu_shortcut(env: &Env) -> PathBuf {
    env.paths.start_menu.join(format!("{NAME}.lnk"))
}

/// Stockroom's icon for its menu entry (Linux), at the sizes menus and docks ask for.
const ICONS: [(&str, &[u8]); 2] = [
    ("48x48", include_bytes!("../../../packaging/icons/stockroom-48.png")),
    ("256x256", include_bytes!("../../../packaging/icons/stockroom-256.png")),
];

fn icon_file(env: &Env, size: &str) -> PathBuf {
    env.paths.xdg_data.join("icons/hicolor").join(size).join("apps").join(format!("{APP_ID}.png"))
}

/// `stockroom.com`, the command line on Windows (`crates/suite-console`), beside a `stockroom.exe`.
fn console_file_name() -> String {
    format!("{SLUG}.com")
}

/// Programs Stockroom carries inside itself, by the name they are kept under.
static CARRIED: OnceLock<Vec<(String, &'static [u8])>> = OnceLock::new();

/// Hands over the programs a Windows release of Stockroom carries inside itself (the
/// launcher shim and `stockroom.com`), so that a single downloaded `.exe` leaves a complete
/// Stockroom behind. The `stockroom` binary calls it once at start; files found beside the running
/// Stockroom are preferred.
pub fn carry(files: Vec<(String, &'static [u8])>) {
    let _ = CARRIED.set(files);
}

/// The programs that travel with Stockroom on Windows: the name each is kept under, and
/// where a download that came with it has it.
fn companions(env: &Env, running: &Path) -> Vec<(String, PathBuf)> {
    let mut files = vec![(integrate::shim_file_name(), running.with_file_name(integrate::shim_file_name()))];
    if env.os == Os::Windows {
        // Named after the program it starts, so a download's `Specular-Installer.com` is
        // kept as `stockroom.com`.
        files.push((console_file_name(), running.with_extension("com")));
    }
    files
}

/// The program to name in anything that outlives this run: the kept copy when there is one,
/// else `running`.
pub fn registered_exe(env: &Env, running: &Path) -> PathBuf {
    let home = home_exe(env);
    if home.is_file() { home } else { running.to_owned() }
}

/// Keeps a copy of the running Stockroom, if it needs one, and returns the program to register.
///
/// A copy of Stockroom outside the user's folders (`/usr/bin`, `/Applications`, `Program Files`) was put
/// there by a package or an administrator and stays put: it is returned as it is. One that
/// macOS runs from a place that goes away is a download like any other ([`transient`]).
pub fn settle(env: &Env, running: &Path) -> Result<PathBuf, InstallError> {
    let version = env!("CARGO_PKG_VERSION")
        .parse()
        .map_err(|e| InstallError::State(format!("Stockroom's own version: {e}")))?;
    settle_as(env, running, &version)
}

/// [`settle`] for Stockroom at the given version.
pub fn settle_as(env: &Env, running: &Path, version: &semver::Version) -> Result<PathBuf, InstallError> {
    let home = home_exe(env);
    if running == home {
        return Ok(home);
    }
    if !running.starts_with(&env.paths.home) && !transient(env, running) {
        return Ok(running.to_owned());
    }
    let kept: Option<semver::Version> =
        std::fs::read_to_string(version_file(env)).ok().and_then(|v| v.trim().parse().ok());
    let len = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
    let stale = match (&kept, home.is_file()) {
        (Some(kept), true) => kept < version || (kept == version && len(&home) != len(running)),
        _ => true,
    };
    if stale {
        match copy_program(running, &home) {
            Ok(()) => {
                write_atomic(&version_file(env), version.to_string().as_bytes())?;
                // The launcher the apps' shortcuts start and the command line (Windows)
                // travel with Stockroom: the files beside it, or the ones it carries.
                for (name, beside) in companions(env, running) {
                    let dst = bin_dir(env).join(&name);
                    let carried = CARRIED.get().and_then(|c| c.iter().find(|(n, _)| *n == name));
                    if beside.is_file() {
                        copy_program(&beside, &dst)?;
                    } else if let Some((_, bytes)) = carried {
                        write_atomic(&dst, bytes)?;
                    }
                }
                tracing::info!(from = %running.display(), to = %home.display(), %version, "kept a copy of Stockroom");
            }
            // The kept copy is running (the scheduled check) and can't be replaced right
            // now: it stays, and the next start tries again.
            Err(e) if home.is_file() => tracing::info!("kept copy not replaced: {e}"),
            Err(e) => return Err(e),
        }
    }
    if let Err(e) = menu_entry(env, &home) {
        tracing::warn!("Stockroom's menu entry: {e}");
    }
    Ok(home)
}

/// Whether macOS runs this program from somewhere that goes away: a mounted disk image
/// (`/Volumes`), or the read-only copy that Gatekeeper's App Translocation makes of a
/// downloaded app that hasn't been moved (`.../AppTranslocation/<id>/d/Install Specular.app`).
fn transient(env: &Env, running: &Path) -> bool {
    env.os == Os::Macos
        && (running.starts_with("/Volumes")
            || running.components().any(|c| c.as_os_str() == "AppTranslocation"))
}

/// Copies a program next to its destination, marks it executable and renames it into place.
fn copy_program(from: &Path, to: &Path) -> Result<(), InstallError> {
    let dir = to.parent().ok_or_else(|| InstallError::State(format!("{} has no folder", to.display())))?;
    std::fs::create_dir_all(dir).map_err(|e| InstallError::io(dir, e))?;
    let mut tmp_name = to.file_name().unwrap_or_default().to_owned();
    tmp_name.push(format!(".{}.suite-new", std::process::id()));
    let tmp = dir.join(tmp_name);
    let copied = std::fs::copy(from, &tmp).map_err(|e| InstallError::io(from, e)).and_then(|_| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| InstallError::io(&tmp, e))?;
        }
        std::fs::rename(&tmp, to).map_err(|e| InstallError::io(to, e))
    });
    if copied.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    copied
}

/// A way to start the kept copy again: never over something that isn't Stockroom's.
fn menu_entry(env: &Env, home: &Path) -> Result<(), InstallError> {
    match env.os {
        Os::Linux => {
            let link = command_link(env);
            let free = match std::fs::read_link(&link) {
                Ok(target) => target.starts_with(&env.paths.data),
                Err(_) => std::fs::symlink_metadata(&link).is_err(),
            };
            if free {
                if let Some(dir) = link.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| InstallError::io(dir, e))?;
                }
                crate::pointer::symlink_atomic(home, &link)?;
            }
            let desktop = desktop_file(env);
            if integrate::ours_or_absent(&desktop) {
                for (size, png) in ICONS {
                    let icon = icon_file(env, size);
                    if std::fs::read(&icon).ok().as_deref() != Some(png) {
                        write_atomic(&icon, png)?;
                    }
                }
                // The window's app ID is APP_ID (suite-ui), so docks and task switchers match
                // the window to this entry and show its icon.
                let text = format!(
                    "[Desktop Entry]\nType=Application\nName={NAME}\nComment=Install and update the family's apps\n\
                     Exec={}\nIcon={APP_ID}\nStartupWMClass={APP_ID}\nTerminal=false\n\
                     Categories=System;PackageManager;\n{}={SLUG}\n",
                    suite_platform::scheduler::desktop_exec_quote(&home.display().to_string()),
                    integrate::DESKTOP_MARKER,
                );
                write_atomic(&desktop, text.as_bytes())?;
            }
            Ok(())
        }
        Os::Windows => {
            let shortcut = start_menu_shortcut(env);
            if shortcut.exists() { Ok(()) } else { integrate::write_shortcut(home, home, &shortcut) }
        }
        // A bare program has no place in Launchpad; Stockroom comes as an app bundle there.
        Os::Macos => Ok(()),
    }
}

/// Removes the kept copy and what [`settle`] made to find it. Returns what was removed.
pub fn unsettle(env: &Env) -> Vec<PathBuf> {
    let command = command_link(env);
    let desktop = desktop_file(env);
    let shortcut = start_menu_shortcut(env);
    let bin = bin_dir(env);
    let entry_is_ours = desktop.is_file() && integrate::ours_or_absent(&desktop);
    let icons = ICONS.map(|(size, _)| icon_file(env, size));
    let mut ours = vec![
        (&command, std::fs::read_link(&command).is_ok_and(|t| t.starts_with(&env.paths.data))),
        (&desktop, entry_is_ours),
        (&shortcut, env.os == Os::Windows && shortcut.is_file()),
        (&bin, bin.is_dir()),
    ];
    // The icons are named with Stockroom's app ID; they go with an entry Stockroom wrote.
    ours.extend(icons.iter().map(|icon| (icon, entry_is_ours && icon.is_file())));
    let mut removed = Vec::new();
    for (path, _) in ours.into_iter().filter(|(_, ours)| *ours) {
        let gone = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        if gone.is_ok() {
            removed.push(path.clone());
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> semver::Version {
        s.parse().unwrap()
    }

    fn download(env: &Env, name: &str, contents: &str) -> PathBuf {
        let dir = env.paths.home.join("Downloads");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn a_download_keeps_a_copy_that_only_newer_ones_replace() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::for_tests(dir.path(), Os::Linux);
        let first = download(&env, "specular-setup", "stockroom 0.2.0");
        assert_eq!(registered_exe(&env, &first), first, "nothing kept yet");
        let home = settle_as(&env, &first, &v("0.2.0")).unwrap();
        assert_eq!(home, env.paths.data.join("bin/stockroom"));
        assert_eq!(std::fs::read_to_string(&home).unwrap(), "stockroom 0.2.0");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&home).unwrap().permissions().mode() & 0o111, 0o111);
        }
        // The download can go: what is registered is the copy.
        std::fs::remove_file(&first).unwrap();
        assert_eq!(registered_exe(&env, &first), home);
        assert_eq!(settle_as(&env, &home, &v("0.2.0")).unwrap(), home);

        // It is on the PATH and in the application menu, with its icon, matched to the
        // window by its app ID.
        assert_eq!(std::fs::read_link(env.paths.bin.join("stockroom")).unwrap(), home);
        let entry = std::fs::read_to_string(desktop_file(&env)).unwrap();
        assert!(entry.contains(&format!("Exec=\"{}\"", home.display())), "{entry}");
        assert!(entry.contains(&format!("\nIcon={APP_ID}\nStartupWMClass={APP_ID}\n")), "{entry}");
        for (size, png) in ICONS {
            assert_eq!(std::fs::read(icon_file(&env, size)).unwrap(), png, "{size}");
        }

        // An older download runs but doesn't replace the copy; a newer one does, and so does
        // a rebuild of the same version.
        let old = download(&env, "dozed-setup", "stockroom 0.1.0");
        assert_eq!(settle_as(&env, &old, &v("0.1.0")).unwrap(), home);
        assert_eq!(std::fs::read_to_string(&home).unwrap(), "stockroom 0.2.0");
        let new = download(&env, "suite-new", "stockroom 0.3.0!");
        settle_as(&env, &new, &v("0.3.0")).unwrap();
        assert_eq!(std::fs::read_to_string(&home).unwrap(), "stockroom 0.3.0!");
        let rebuilt = download(&env, "suite-rebuilt", "stockroom 0.3.0 again");
        settle_as(&env, &rebuilt, &v("0.3.0")).unwrap();
        assert_eq!(std::fs::read_to_string(&home).unwrap(), "stockroom 0.3.0 again");

        // And all of it goes again.
        let removed = unsettle(&env);
        assert_eq!(removed.len(), 5, "{removed:?}");
        assert!(!home.exists() && !desktop_file(&env).exists());
        assert!(ICONS.iter().all(|(size, _)| !icon_file(&env, size).exists()));
        assert!(std::fs::symlink_metadata(env.paths.bin.join("stockroom")).is_err());
        assert_eq!(unsettle(&env), [] as [PathBuf; 0]);
    }

    #[test]
    fn a_system_wide_suite_and_other_peoples_files_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::for_tests(dir.path(), Os::Linux);
        // Installed by a package: outside the home folder.
        let system = env.paths.sys("usr/bin/stockroom");
        std::fs::create_dir_all(system.parent().unwrap()).unwrap();
        std::fs::write(&system, "packaged").unwrap();
        assert_eq!(settle_as(&env, &system, &v("1.0.0")).unwrap(), system);
        assert!(!home_exe(&env).exists());

        // Someone's own `stockroom` command and menu entry stay as they are.
        std::fs::create_dir_all(&env.paths.bin).unwrap();
        std::fs::write(env.paths.bin.join("stockroom"), "mine").unwrap();
        let desktop = desktop_file(&env);
        std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
        std::fs::write(&desktop, "[Desktop Entry]\nName=Mine\n").unwrap();
        let dl = download(&env, "stockroom", "stockroom 1.0.0");
        settle_as(&env, &dl, &v("1.0.0")).unwrap();
        assert_eq!(std::fs::read_to_string(env.paths.bin.join("stockroom")).unwrap(), "mine");
        assert!(std::fs::read_to_string(&desktop).unwrap().contains("Name=Mine"));
        unsettle(&env);
        assert!(env.paths.bin.join("stockroom").exists() && desktop.exists());
    }

    #[test]
    fn a_translocated_mac_download_keeps_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::for_tests(dir.path(), Os::Macos);
        let macos = dir.path().join("AppTranslocation/0A1B/d/Install Specular.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let running = macos.join("stockroom");
        std::fs::write(&running, "stockroom").unwrap();
        assert!(!running.starts_with(&env.paths.home));
        assert_eq!(settle_as(&env, &running, &v("1.0.0")).unwrap(), home_exe(&env));
        assert!(transient(&env, Path::new("/Volumes/Stockroom/Stockroom.app/Contents/MacOS/stockroom")));
        // An app put in /Applications stays where it is.
        let applications = Path::new("/Applications/Stockroom.app/Contents/MacOS/stockroom");
        assert!(!transient(&env, applications));
        assert_eq!(settle_as(&env, applications, &v("1.0.0")).unwrap(), applications);
    }

    #[test]
    fn windows_gets_a_start_menu_shortcut_and_the_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::for_tests(dir.path(), Os::Windows);
        let dl = download(&env, "Specular-Setup.exe", "stockroom");
        std::fs::write(dl.with_file_name(integrate::shim_file_name()), "shim").unwrap();
        std::fs::write(dl.with_extension("com"), "console").unwrap();
        let home = settle_as(&env, &dl, &v("1.0.0")).unwrap();
        assert!(home.ends_with("bin/stockroom.exe"), "{}", home.display());
        assert!(home.with_file_name(integrate::shim_file_name()).is_file());
        // The command line is kept under the name that runs the kept copy.
        assert_eq!(std::fs::read_to_string(home.with_file_name("stockroom.com")).unwrap(), "console");

        // A download on its own writes out what it carries, for a newer version too.
        carry(vec![
            (integrate::shim_file_name(), b"carried shim"),
            ("stockroom.com".to_owned(), b"carried com"),
        ]);
        let desktop = env.paths.home.join("Desktop");
        std::fs::create_dir_all(&desktop).unwrap();
        let alone = desktop.join("Install Specular.exe");
        std::fs::write(&alone, "stockroom 2").unwrap();
        settle_as(&env, &alone, &v("2.0.0")).unwrap();
        assert_eq!(std::fs::read(home.with_file_name(integrate::shim_file_name())).unwrap(), b"carried shim");
        assert_eq!(std::fs::read(home.with_file_name("stockroom.com")).unwrap(), b"carried com");
        let shortcut = start_menu_shortcut(&env);
        assert!(std::fs::read_to_string(&shortcut).unwrap().contains(&home.display().to_string()));
        assert!(unsettle(&env).contains(&shortcut));
    }
}
