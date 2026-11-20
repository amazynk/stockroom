//! `AppImages` are single executables: check the magic, copy, make executable.

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use crate::UnpackError;

/// Whether `file` starts like a type-2 `AppImage` (ELF with `AI\x02` at offset 8).
pub fn is_appimage(file: &Path) -> std::io::Result<bool> {
    let mut head = [0u8; 11];
    let mut f = std::fs::File::open(file)?;
    if f.read_exact(&mut head).is_err() {
        return Ok(false);
    }
    Ok(&head[..4] == b"\x7fELF" && &head[8..11] == b"AI\x02")
}

/// Copies the `AppImage` into `dir` as `name` with mode 0755 and returns its path.
pub fn place(file: &Path, dir: &Path, name: &str) -> Result<PathBuf, UnpackError> {
    if !is_appimage(file).map_err(|e| UnpackError::io(file, e))? {
        return Err(UnpackError::WrongFormat("AppImage"));
    }
    std::fs::create_dir_all(dir).map_err(|e| UnpackError::io(dir, e))?;
    let dest = dir.join(name);
    std::fs::copy(file, &dest).map_err(|e| UnpackError::io(&dest, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| UnpackError::io(&dest, e))?;
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_only_appimages() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good");
        let mut bytes = b"\x7fELF\x02\x01\x01\x00AI\x02".to_vec();
        bytes.extend_from_slice(&[0; 64]);
        std::fs::write(&good, &bytes).unwrap();
        let bad = dir.path().join("bad");
        std::fs::write(&bad, b"#!/bin/sh\necho hi\n").unwrap();
        let placed = place(&good, &dir.path().join("out"), "App.AppImage").unwrap();
        assert_eq!(std::fs::read(&placed).unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&placed).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert!(matches!(place(&bad, dir.path(), "x"), Err(UnpackError::WrongFormat(_))));
    }
}
