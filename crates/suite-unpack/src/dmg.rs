//! Disk images on macOS: `hdiutil attach -nobrowse -readonly`, `ditto` the `.app` out,
//! detach. Every command goes through a [`Runner`], so tests on any OS drive it with a fake.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use suite_platform::runner::Runner;

use crate::UnpackError;

fn run(runner: &dyn Runner, program: &str, a: &[OsString]) -> Result<(), UnpackError> {
    let out = runner.run(program, a).map_err(|e| UnpackError::Command(format!("{program}: {e}")))?;
    if out.success {
        Ok(())
    } else {
        Err(UnpackError::Command(format!("{program} failed: {}", out.stderr.trim())))
    }
}

/// Mounts `dmg` at `mount` (an empty folder), copies the `.app` bundle named `bundle` (or
/// the only `.app` on the image) to `dest_dir`, and always detaches. Returns the new path.
pub fn copy_app(
    runner: &dyn Runner,
    dmg: &Path,
    mount: &Path,
    bundle: Option<&str>,
    dest_dir: &Path,
) -> Result<PathBuf, UnpackError> {
    std::fs::create_dir_all(mount).map_err(|e| UnpackError::io(mount, e))?;
    run(
        runner,
        "hdiutil",
        &[
            "attach".into(),
            "-nobrowse".into(),
            "-readonly".into(),
            "-noautoopen".into(),
            "-mountpoint".into(),
            mount.into(),
            dmg.into(),
        ],
    )?;
    let result = copy_from_mount(runner, mount, bundle, dest_dir);
    let detach = run(runner, "hdiutil", &["detach".into(), mount.into()])
        .or_else(|_| run(runner, "hdiutil", &["detach".into(), "-force".into(), mount.into()]));
    let path = result?;
    detach?;
    Ok(path)
}

fn copy_from_mount(
    runner: &dyn Runner,
    mount: &Path,
    bundle: Option<&str>,
    dest_dir: &Path,
) -> Result<PathBuf, UnpackError> {
    let name = if let Some(b) = bundle {
        b.to_owned()
    } else {
        let apps: Vec<String> = std::fs::read_dir(mount)
            .map_err(|e| UnpackError::io(mount, e))?
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| is_app_name(n))
            .collect();
        match apps.as_slice() {
            [one] => one.clone(),
            _ => return Err(UnpackError::WrongFormat("disk image with exactly one .app")),
        }
    };
    if name.contains('/') || name.starts_with('.') || !is_app_name(&name) {
        return Err(UnpackError::unsafe_entry(&name, "bad bundle name"));
    }
    let src = mount.join(&name);
    std::fs::create_dir_all(dest_dir).map_err(|e| UnpackError::io(dest_dir, e))?;
    let dest = dest_dir.join(&name);
    // `--noqtn`: no quarantine attribute (Stockroom verified the download itself).
    run(runner, "ditto", &["--noqtn".into(), src.into(), dest.clone().into()])?;
    Ok(dest)
}

fn is_app_name(n: &str) -> bool {
    std::path::Path::new(n).extension().is_some_and(|e| e.eq_ignore_ascii_case("app"))
}

#[cfg(test)]
mod tests {
    use suite_platform::{FakeRunner, Output};

    use super::*;

    #[test]
    fn attach_copy_detach() {
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().join("mnt");
        std::fs::create_dir_all(mount.join("Demo.app")).unwrap();
        let r = FakeRunner::succeed_all();
        let out = copy_app(&r, Path::new("/d/Demo.dmg"), &mount, None, &dir.path().join("Apps")).unwrap();
        assert!(out.ends_with("Apps/Demo.app"));
        let calls = r.calls();
        assert_eq!(calls[0][..3], ["hdiutil", "attach", "-nobrowse"]);
        assert_eq!(calls[1][0], "ditto");
        assert_eq!(calls[2][..2], ["hdiutil", "detach"]);
    }

    #[test]
    fn detaches_even_when_the_copy_fails() {
        let dir = tempfile::tempdir().unwrap();
        let r = FakeRunner::succeed_all();
        r.on(&["ditto"], Output::failed(1, "disk full"));
        let err = copy_app(&r, Path::new("/d/x.dmg"), &dir.path().join("m"), Some("X.app"), dir.path())
            .unwrap_err();
        assert!(err.to_string().contains("disk full"));
        assert!(r.ran(&["hdiutil", "detach"]));
        let bad = copy_app(&r, Path::new("/d/x.dmg"), &dir.path().join("m"), Some("../X.app"), dir.path());
        assert!(bad.is_err());
    }
}
