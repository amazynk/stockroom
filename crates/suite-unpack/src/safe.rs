//! The checks shared by the tar and zip extractors.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use crate::{Limits, Summary, UnpackError};

/// Validates an archive entry name and returns it as a relative path. `.` and empty
/// components are dropped; anything that could leave the destination is refused.
pub fn safe_relative(name: &str) -> Result<PathBuf, UnpackError> {
    if name.contains('\0') {
        return Err(UnpackError::unsafe_entry(name, "NUL in name"));
    }
    if name.contains('\\') {
        return Err(UnpackError::unsafe_entry(name, "backslash in name"));
    }
    if name.starts_with('/') {
        return Err(UnpackError::unsafe_entry(name, "absolute path"));
    }
    if name.contains(':') {
        return Err(UnpackError::unsafe_entry(name, "drive letter or stream name"));
    }
    let mut out = PathBuf::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(UnpackError::unsafe_entry(name, "parent directory (..)")),
            p => out.push(p),
        }
    }
    // Belt and braces: the components must all be plain names.
    if out.as_os_str().is_empty() || !out.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(UnpackError::unsafe_entry(name, "empty or odd path"));
    }
    Ok(out)
}

/// Checks that a symlink at `link` (relative to the root) pointing to `target` stays inside
/// the root, judged by its text.
pub(crate) fn check_symlink_target(name: &str, link: &Path, target: &str) -> Result<(), UnpackError> {
    if target.is_empty() || target.contains('\0') || target.contains('\\') || target.contains(':') {
        return Err(UnpackError::unsafe_entry(name, "odd symlink target"));
    }
    if target.starts_with('/') {
        return Err(UnpackError::unsafe_entry(name, "symlink to an absolute path"));
    }
    let mut depth: Vec<&std::ffi::OsStr> = link.parent().map_or_default(|p| {
        p.components().filter_map(|c| if let Component::Normal(n) = c { Some(n) } else { None }).collect()
    });
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if depth.pop().is_none() {
                    return Err(UnpackError::unsafe_entry(name, "symlink points outside"));
                }
            }
            p => depth.push(std::ffi::OsStr::new(p)),
        }
    }
    Ok(())
}

/// State of one extraction: the root, the counters and the limits.
#[derive(Debug)]
pub(crate) struct Writer<'a> {
    root: PathBuf,
    limits: &'a Limits,
    pub(crate) summary: Summary,
    entries: u64,
    symlinks: Vec<PathBuf>,
}

impl<'a> Writer<'a> {
    /// `dest` must not exist yet, or be an empty folder.
    pub(crate) fn new(dest: &Path, limits: &'a Limits) -> Result<Self, UnpackError> {
        std::fs::create_dir_all(dest).map_err(|e| UnpackError::io(dest, e))?;
        if std::fs::read_dir(dest).map_err(|e| UnpackError::io(dest, e))?.next().is_some() {
            return Err(UnpackError::Io {
                path: dest.to_owned(),
                source: std::io::Error::new(std::io::ErrorKind::AlreadyExists, "destination is not empty"),
            });
        }
        let root = std::fs::canonicalize(dest).map_err(|e| UnpackError::io(dest, e))?;
        Ok(Self { root, limits, summary: Summary::default(), entries: 0, symlinks: Vec::new() })
    }

    pub(crate) fn count_entry(&mut self) -> Result<(), UnpackError> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(UnpackError::TooBig("too many entries"));
        }
        Ok(())
    }

    /// Creates the parent folders of `rel`, refusing to go through anything but real
    /// folders (a symlink planted earlier in the archive, for instance).
    fn prepare_parent(&self, name: &str, rel: &Path) -> Result<PathBuf, UnpackError> {
        let mut cur = self.root.clone();
        let parts: Vec<_> = rel.components().collect();
        for c in &parts[..parts.len().saturating_sub(1)] {
            cur.push(c);
            match std::fs::symlink_metadata(&cur) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return Err(UnpackError::unsafe_entry(name, "path goes through a symlink or file")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&cur).map_err(|e| UnpackError::io(&cur, e))?;
                }
                Err(e) => return Err(UnpackError::io(&cur, e)),
            }
        }
        Ok(self.root.join(rel))
    }

    pub(crate) fn dir(&mut self, name: &str, rel: &Path) -> Result<(), UnpackError> {
        let path = self.prepare_parent(name, rel)?;
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => return Ok(()),
            Ok(_) => return Err(UnpackError::unsafe_entry(name, "directory replaces another entry")),
            Err(_) => {}
        }
        std::fs::create_dir(&path).map_err(|e| UnpackError::io(&path, e))?;
        self.summary.dirs += 1;
        Ok(())
    }

    /// Writes a regular file from `data`, at most the remaining byte budget.
    pub(crate) fn file(
        &mut self,
        name: &str,
        rel: &Path,
        data: &mut dyn Read,
        mode: Option<u32>,
    ) -> Result<(), UnpackError> {
        let path = self.prepare_parent(name, rel)?;
        // `create_new` refuses an existing entry, including a symlink: no duplicates and
        // no following links.
        let mut f: File =
            OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => UnpackError::unsafe_entry(name, "duplicate entry"),
                _ => UnpackError::io(&path, e),
            })?;
        let budget = (self.limits.max_total_bytes - self.summary.bytes).min(self.limits.max_file_bytes);
        let mut limited = data.take(budget + 1);
        let mut buf = vec![0u8; 128 * 1024];
        let mut written = 0u64;
        loop {
            let n = limited.read(&mut buf).map_err(|e| UnpackError::Corrupt(format!("{name}: {e}")))?;
            if n == 0 {
                break;
            }
            written += n as u64;
            if written > budget {
                return Err(UnpackError::TooBig(if written > self.limits.max_file_bytes {
                    "file too large"
                } else {
                    "total size too large"
                }));
            }
            f.write_all(&buf[..n]).map_err(|e| UnpackError::io(&path, e))?;
        }
        self.summary.bytes += written;
        self.summary.files += 1;
        set_mode(&f, &path, mode)?;
        Ok(())
    }

    pub(crate) fn symlink(&mut self, name: &str, rel: &Path, target: &str) -> Result<(), UnpackError> {
        check_symlink_target(name, rel, target)?;
        let path = self.prepare_parent(name, rel)?;
        if std::fs::symlink_metadata(&path).is_ok() {
            return Err(UnpackError::unsafe_entry(name, "duplicate entry"));
        }
        make_symlink(target, &path).map_err(|e| UnpackError::io(&path, e))?;
        self.symlinks.push(path);
        self.summary.symlinks += 1;
        Ok(())
    }

    /// The final check: every symlink, fully resolved on disk, lands inside the root.
    /// This catches chains whose text looked harmless one link at a time.
    pub(crate) fn finish(self) -> Result<Summary, UnpackError> {
        for link in &self.symlinks {
            let name = link.strip_prefix(&self.root).unwrap_or(link).display().to_string();
            match std::fs::canonicalize(link) {
                Ok(real) if real.starts_with(&self.root) => {}
                Ok(_) => return Err(UnpackError::Unsafe { name, why: "symlink resolves outside" }),
                Err(_) => return Err(UnpackError::Unsafe { name, why: "dangling or looping symlink" }),
            }
        }
        Ok(self.summary)
    }
}

#[cfg(unix)]
fn make_symlink(target: &str, path: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(windows)]
fn make_symlink(target: &str, path: &Path) -> std::io::Result<()> {
    // Symlinks need a privilege on Windows; app zips for Windows don't use them.
    let _ = (target, path);
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks in archives are not supported on Windows",
    ))
}

#[cfg(unix)]
fn set_mode(f: &File, path: &Path, mode: Option<u32>) -> Result<(), UnpackError> {
    use std::os::unix::fs::PermissionsExt;
    // Keep the rwx bits for the owner and r-x for others; drop setuid/setgid/sticky and
    // group/world write.
    let mode = mode.unwrap_or(0o644) & 0o755;
    f.set_permissions(std::fs::Permissions::from_mode(mode | 0o600)).map_err(|e| UnpackError::io(path, e))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn set_mode(_f: &File, _path: &Path, _mode: Option<u32>) -> Result<(), UnpackError> {
    Ok(())
}

/// When an extraction holds exactly one folder (`app-1.0-linux/...`), that folder.
pub fn flatten_single_dir(dir: &Path) -> std::io::Result<PathBuf> {
    let mut entries = std::fs::read_dir(dir)?;
    let (Some(first), None) = (entries.next().transpose()?, entries.next()) else {
        return Ok(dir.to_owned());
    };
    if first.file_type()?.is_dir() { Ok(first.path()) } else { Ok(dir.to_owned()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(safe_relative("a/./b/").unwrap(), Path::new("a/b"));
        assert_eq!(safe_relative("./a").unwrap(), Path::new("a"));
        for bad in ["../a", "a/../../b", "/etc/passwd", "C:/x", "a\\..\\b", "", ".", "a\0b", "file:stream"] {
            assert!(safe_relative(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn symlink_targets() {
        let ok = |l: &str, t: &str| check_symlink_target(l, Path::new(l), t).is_ok();
        assert!(ok("lib/libx.so", "libx.so.1"));
        assert!(ok("lib/libx.so", "../lib64/libx.so.1"));
        assert!(ok("bin/app", "../app"));
        assert!(!ok("bin/app", "../../app"));
        assert!(!ok("link", ".."));
        assert!(!ok("link", "/etc/passwd"));
        assert!(!ok("link", "a/../../b"));
    }
}
