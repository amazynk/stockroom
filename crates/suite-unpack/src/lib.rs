//! Turning a verified download into files on disk, safely (PLAN.md §4).
//!
//! Archives are extracted into a fresh folder. Every entry is checked before anything is
//! written: no `..`, no absolute or drive paths, no backslashes, no hard links or device
//! files, no symlinks that point outside the folder, no writing through a symlink, no
//! duplicate entries, and size and entry-count limits against archive bombs. Permission
//! bits are kept on Unix (minus setuid/setgid and group/world write).

pub mod appimage;
pub mod dmg;
pub mod flatpak;
mod safe;
mod tar_gz;
mod zip_archive;

use std::path::{Path, PathBuf};

pub use safe::{flatten_single_dir, safe_relative};
pub use tar_gz::{extract_tar_gz, extract_tar_gz_reader, extract_tar_reader};
pub use zip_archive::{extract_zip, extract_zip_reader};

#[derive(Debug, thiserror::Error)]
pub enum UnpackError {
    #[error("unsafe archive entry {name:?}: {why}")]
    Unsafe { name: String, why: &'static str },
    #[error("archive exceeds the limit: {0}")]
    TooBig(&'static str),
    #[error("the archive is damaged: {0}")]
    Corrupt(String),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{0}")]
    Command(String),
    #[error("not a valid {0}")]
    WrongFormat(&'static str),
}

impl UnpackError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io { path: path.to_owned(), source }
    }

    pub(crate) fn unsafe_entry(name: &str, why: &'static str) -> Self {
        Self::Unsafe { name: name.to_owned(), why }
    }
}

/// Limits against archive bombs.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Total bytes written for all files.
    pub max_total_bytes: u64,
    /// Bytes for any one file.
    pub max_file_bytes: u64,
    /// Entries of any kind.
    pub max_entries: u64,
}

impl Default for Limits {
    fn default() -> Self {
        // Specular's binary alone is 144 MB; leave room, but not unbounded.
        Self { max_total_bytes: 8 << 30, max_file_bytes: 4 << 30, max_entries: 200_000 }
    }
}

/// What an extraction produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes: u64,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use suite_testkit::archive::{self, TarGz, Zip};

    use super::*;

    fn small() -> Limits {
        Limits { max_total_bytes: 8 << 20, max_file_bytes: 4 << 20, max_entries: 100 }
    }

    #[test]
    fn extracts_a_normal_tarball() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out");
        let tgz = TarGz::new()
            .dir("app-1.0/")
            .file("app-1.0/app", "#!/bin/sh\n", 0o4755)
            .dir("app-1.0/lib")
            .file("app-1.0/lib/libx.so.1", "lib", 0o644)
            .symlink("app-1.0/lib/libx.so", "libx.so.1")
            .symlink("app-1.0/bin", ".")
            .build();
        let s = extract_tar_gz_reader(Cursor::new(tgz), &dest, &small()).unwrap();
        assert_eq!((s.files, s.symlinks), (2, 2));
        let root = flatten_single_dir(&dest).unwrap();
        assert!(root.ends_with("app-1.0"));
        assert_eq!(std::fs::read_to_string(root.join("lib/libx.so")).unwrap(), "lib");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // setuid is dropped, rwx kept.
            assert_eq!(std::fs::metadata(root.join("app")).unwrap().permissions().mode() & 0o7777, 0o755);
        }
    }

    #[test]
    fn extracts_a_normal_zip() {
        let dir = tempfile::tempdir().unwrap();
        let zip = Zip::new()
            .dir("App/")
            .file("App/app.exe", "MZ", 0o644)
            .file("App/readme.txt", "hi", 0o644)
            .build();
        let s = extract_zip_reader(Cursor::new(zip), &dir.path().join("z"), &small()).unwrap();
        assert_eq!(s.files, 2);
        assert_eq!(std::fs::read(dir.path().join("z/App/app.exe")).unwrap(), b"MZ");
    }

    fn assert_nothing_outside(dir: &Path) {
        // Only the destination folder may exist next to where the archive was unpacked.
        let names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["out"], "something escaped");
    }

    #[test]
    fn refuses_every_malicious_tar() {
        for (name, bytes) in archive::malicious_tars() {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().join("base");
            let dest = base.join("out");
            let r = extract_tar_gz_reader(Cursor::new(bytes), &dest, &small());
            assert!(matches!(r, Err(UnpackError::Unsafe { .. })), "{name}: {r:?}");
            assert_nothing_outside(&base);
        }
    }

    #[test]
    fn refuses_every_malicious_zip() {
        for (name, bytes) in archive::malicious_zips() {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().join("base");
            let dest = base.join("out");
            let r = extract_zip_reader(Cursor::new(bytes), &dest, &small());
            assert!(matches!(r, Err(UnpackError::Unsafe { .. })), "{name}: {r:?}");
            assert_nothing_outside(&base);
        }
    }

    #[test]
    fn symlink_chain_that_escapes_on_disk_is_caught() {
        // Each link looks fine alone: `up` -> `top/..` stays inside by its text, but `top`
        // is a link to `.`, so on disk `up` is the parent of the root.
        let dir = tempfile::tempdir().unwrap();
        let tgz = TarGz::new().symlink("up", "top/..").symlink("top", ".").build();
        let r = extract_tar_gz_reader(Cursor::new(tgz), &dir.path().join("out"), &small());
        assert!(matches!(r, Err(UnpackError::Unsafe { why: "symlink resolves outside", .. })), "{r:?}");
    }

    #[test]
    fn dangling_symlinks_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let tgz = TarGz::new().symlink("x", "missing").build();
        assert!(extract_tar_gz_reader(Cursor::new(tgz), &dir.path().join("out"), &small()).is_err());
    }

    #[test]
    fn bombs_hit_the_limits() {
        let dir = tempfile::tempdir().unwrap();
        let r = extract_tar_gz_reader(Cursor::new(archive::tar_bomb(16)), &dir.path().join("a"), &small());
        assert!(matches!(r, Err(UnpackError::TooBig(_))), "{r:?}");
        let r = extract_zip_reader(Cursor::new(archive::zip_bomb(16)), &dir.path().join("b"), &small());
        assert!(matches!(r, Err(UnpackError::TooBig(_))), "{r:?}");
        let r = extract_tar_gz_reader(
            Cursor::new(archive::tar_many_entries(150)),
            &dir.path().join("c"),
            &small(),
        );
        assert!(matches!(r, Err(UnpackError::TooBig("too many entries"))), "{r:?}");
        // The total budget, across many files that are each under the per-file limit.
        let many =
            (0..5).fold(TarGz::new(), |t, i| t.file(&format!("f{i}"), vec![0u8; 3 << 20], 0o644)).build();
        let r = extract_tar_gz_reader(Cursor::new(many), &dir.path().join("d"), &small());
        assert!(matches!(r, Err(UnpackError::TooBig("total size too large"))), "{r:?}");
    }

    #[test]
    fn refuses_a_non_empty_destination_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("existing"), "x").unwrap();
        let tgz = TarGz::new().file("a", "x", 0o644).build();
        assert!(extract_tar_gz_reader(Cursor::new(tgz), dir.path(), &small()).is_err());
        let r = extract_tar_gz_reader(Cursor::new(b"not gzip".to_vec()), &dir.path().join("g"), &small());
        assert!(matches!(r, Err(UnpackError::Corrupt(_))));
        let r = extract_zip_reader(Cursor::new(b"not zip".to_vec()), &dir.path().join("h"), &small());
        assert!(matches!(r, Err(UnpackError::Corrupt(_))));
    }
}
