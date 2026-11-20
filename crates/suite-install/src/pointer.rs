//! The `current` pointer of an app folder: which version folder is live.
//!
//! On Unix it is a symlink `current -> <version>`, switched atomically by renaming a new
//! symlink over it. On Windows it is `current.txt`, switched by temp file + rename: the
//! version on the first line and the main executable (relative to the version folder, `/`
//! separated) on the second, which is what the launcher shim (`suite-shim`) starts. One file
//! holds both, so they switch together.

use std::path::{Path, PathBuf};

use suite_catalog::Os;

use crate::InstallError;

pub const CURRENT: &str = "current";
pub const CURRENT_TXT: &str = "current.txt";

fn uses_symlink(os: Os) -> bool {
    cfg!(unix) && os != Os::Windows
}

/// The version `app_dir`'s pointer names, if any.
pub fn read(os: Os, app_dir: &Path) -> Option<semver::Version> {
    if uses_symlink(os) {
        let t = std::fs::read_link(app_dir.join(CURRENT)).ok()?;
        t.to_str()?.parse().ok()
    } else {
        std::fs::read_to_string(app_dir.join(CURRENT_TXT)).ok()?.lines().next()?.trim().parse().ok()
    }
}

/// Atomically points `app_dir`'s pointer at `version`, whose main program is `exe` (used by
/// the Windows launcher shim; a symlink needs no such hint).
pub fn set(os: Os, app_dir: &Path, version: &semver::Version, exe: Option<&str>) -> Result<(), InstallError> {
    if uses_symlink(os) {
        symlink_atomic(Path::new(&version.to_string()), &app_dir.join(CURRENT))
    } else {
        let text = match exe {
            Some(exe) => format!("{version}\n{exe}\n"),
            None => format!("{version}\n"),
        };
        crate::state::write_atomic(&app_dir.join(CURRENT_TXT), text.as_bytes())
    }
}

/// Removes the pointer (the app is going away).
pub fn clear(app_dir: &Path) {
    let _ = std::fs::remove_file(app_dir.join(CURRENT));
    let _ = std::fs::remove_file(app_dir.join(CURRENT_TXT));
}

/// Creates or replaces the symlink `link -> target` with a rename, never leaving it absent.
#[cfg(unix)]
pub fn symlink_atomic(target: &Path, link: &Path) -> Result<(), InstallError> {
    let mut tmp_name = link.file_name().unwrap_or_default().to_owned();
    tmp_name.push(".suite-new");
    let tmp: PathBuf = link.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp).map_err(|e| InstallError::io(&tmp, e))?;
    std::fs::rename(&tmp, link).map_err(|e| InstallError::io(link, e))?;
    if let Some(dir) = link.parent() {
        crate::state::sync_dir(dir);
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn symlink_atomic(target: &Path, link: &Path) -> Result<(), InstallError> {
    let _ = (target, PathBuf::new());
    Err(InstallError::Unsupported(format!("symlinks are not used on this OS ({})", link.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_atomically() {
        let dir = tempfile::tempdir().unwrap();
        for os in [Os::Linux, Os::Windows] {
            let app = dir.path().join(os.as_str());
            std::fs::create_dir_all(&app).unwrap();
            assert_eq!(read(os, &app), None);
            set(os, &app, &"1.0.0".parse().unwrap(), None).unwrap();
            assert_eq!(read(os, &app), Some("1.0.0".parse().unwrap()));
            set(os, &app, &"1.1.0".parse().unwrap(), Some("bin/demo.exe")).unwrap();
            assert_eq!(read(os, &app), Some("1.1.0".parse().unwrap()));
            assert_eq!(std::fs::read_dir(&app).unwrap().count(), 1, "no leftovers");
            if os == Os::Windows {
                let text = std::fs::read_to_string(app.join(CURRENT_TXT)).unwrap();
                assert_eq!(text, "1.1.0\nbin/demo.exe\n");
            }
            clear(&app);
            assert_eq!(read(os, &app), None);
        }
    }
}
