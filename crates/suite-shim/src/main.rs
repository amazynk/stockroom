//! The launcher shim (PLAN.md §4, Windows). Stockroom copies it to
//! `%LOCALAPPDATA%\Programs\<stockroom>\<app>\<app>.exe`, next to the version folders and
//! `current.txt`, and points the Start-menu shortcut and file associations at it. It reads
//! `current.txt` (version, then the executable relative to the version folder), starts that
//! program with the same arguments and exits, so shortcuts never change when an update
//! switches versions, and a running old version keeps its files.
//!
//! Standard library only and no `unsafe`; it builds and is tested on every OS, but only
//! Windows installs use it.

#![windows_subsystem = "windows"]

use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
};

/// The pointer file beside the shim; the format is `suite-install`'s `pointer` module.
const POINTER: &str = "current.txt";
/// Bigger than any real pointer file; anything larger is not one.
const MAX_POINTER: u64 = 4096;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match std::env::current_exe().map_err(|e| e.to_string()).and_then(|shim| launch(&shim, &args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // No console in a GUI-subsystem program; this reaches a terminal when there is one.
            eprintln!("suite-shim: {e}");
            ExitCode::FAILURE
        }
    }
}

fn launch(shim: &Path, args: &[OsString]) -> Result<(), String> {
    let program = target(shim)?;
    Command::new(&program).args(args).spawn().map_err(|e| format!("{}: {e}", program.display()))?;
    Ok(())
}

/// The program `current.txt` beside `shim` names, checked to stay inside the app folder.
fn target(shim: &Path) -> Result<PathBuf, String> {
    let dir = shim.parent().ok_or("the shim has no folder")?;
    let pointer = dir.join(POINTER);
    let len = std::fs::metadata(&pointer).map_err(|e| format!("{}: {e}", pointer.display()))?.len();
    if len > MAX_POINTER {
        return Err(format!("{} is not a version pointer", pointer.display()));
    }
    let text = std::fs::read_to_string(&pointer).map_err(|e| format!("{}: {e}", pointer.display()))?;
    let mut lines = text.lines().map(str::trim);
    let version = lines.next().filter(|v| is_version(v)).ok_or("current.txt names no version")?;
    let exe = lines.next().filter(|e| !e.is_empty()).ok_or("current.txt names no program")?;
    let mut path = dir.join(version);
    for part in exe.split('/') {
        let ok =
            matches!(Path::new(part).components().collect::<Vec<_>>().as_slice(), [Component::Normal(_)]);
        if !ok || part.contains('\\') || part.contains(':') {
            return Err(format!("current.txt names an unsafe path {exe:?}"));
        }
        path.push(part);
    }
    if !path.is_file() {
        return Err(format!("{} is missing", path.display()));
    }
    Ok(path)
}

/// A version folder name: semver characters only, so it can't climb out of the app folder.
fn is_version(v: &str) -> bool {
    !v.is_empty()
        && v.chars().next().is_some_and(|c| c.is_ascii_digit())
        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        && !v.contains("..")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use super::*;

    fn app_dir(pointer: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("1.2.0/bin")).unwrap();
        std::fs::write(dir.path().join("1.2.0/bin/demo.exe"), "MZ").unwrap();
        std::fs::write(dir.path().join(POINTER), pointer).unwrap();
        let shim = dir.path().join("demo.exe");
        (dir, shim)
    }

    #[test]
    fn follows_the_pointer() {
        let (dir, shim) = app_dir("1.2.0\nbin/demo.exe\n");
        assert_eq!(target(&shim).unwrap(), dir.path().join("1.2.0").join("bin").join("demo.exe"));
        let (_d, shim) = app_dir("1.2.0\r\nbin/demo.exe\r\n");
        assert!(target(&shim).is_ok(), "Windows line ends");
    }

    #[test]
    fn refuses_anything_else() {
        for bad in [
            "",
            "1.2.0\n",
            "1.2.0\n\n",
            "../x\nbin/demo.exe\n",
            "1.2.0/..\nbin/demo.exe\n",
            "1.2.0\n../1.2.0/bin/demo.exe\n",
            "1.2.0\n/etc/passwd\n",
            "1.2.0\nbin\\..\\..\\x.exe\n",
            "1.2.0\nC:/Windows/notepad.exe\n",
            "1.2.0\nbin/missing.exe\n",
            "1.2.0\nbin\n",
        ] {
            let (_d, shim) = app_dir(bad);
            assert!(target(&shim).is_err(), "{bad:?}");
        }
        let (_d, shim) = app_dir(&"9".repeat(5000));
        assert!(target(&shim).unwrap_err().contains("not a version pointer"));
    }

    #[cfg(unix)]
    #[test]
    fn starts_the_program_with_its_arguments() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, shim) = app_dir("1.2.0\nbin/demo.exe\n");
        let out = dir.path().join("out.txt");
        let script = format!("#!/bin/sh\necho \"$@\" > '{}'\n", out.display());
        let exe = dir.path().join("1.2.0/bin/demo.exe");
        std::fs::write(&exe, script).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        launch(&shim, &["a b".into(), "c".into()]).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !out.exists() || std::fs::read_to_string(&out).unwrap().is_empty() {
            assert!(std::time::Instant::now() < end, "the program did not run");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "a b c\n");
    }
}
