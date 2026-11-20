//! Archive builders. Unlike the `tar` crate's safe helpers, these write names and link
//! targets verbatim, so tests can build the malicious archives an attacker would.

use std::io::{Cursor, Write};

use flate2::{Compression, write::GzEncoder};
use zip::write::SimpleFileOptions;

#[derive(Debug, Clone)]
enum TarEntry {
    File { name: Vec<u8>, data: Vec<u8>, mode: u32 },
    Dir { name: Vec<u8> },
    Symlink { name: Vec<u8>, target: Vec<u8> },
    Hardlink { name: Vec<u8>, target: Vec<u8> },
    Fifo { name: Vec<u8> },
}

/// Builds a `.tar.gz` in memory.
#[derive(Debug, Clone, Default)]
pub struct TarGz {
    entries: Vec<TarEntry>,
}

impl TarGz {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn file(mut self, name: &str, data: impl Into<Vec<u8>>, mode: u32) -> Self {
        self.entries.push(TarEntry::File { name: name.into(), data: data.into(), mode });
        self
    }

    #[must_use]
    pub fn dir(mut self, name: &str) -> Self {
        self.entries.push(TarEntry::Dir { name: name.into() });
        self
    }

    #[must_use]
    pub fn symlink(mut self, name: &str, target: &str) -> Self {
        self.entries.push(TarEntry::Symlink { name: name.into(), target: target.into() });
        self
    }

    #[must_use]
    pub fn hardlink(mut self, name: &str, target: &str) -> Self {
        self.entries.push(TarEntry::Hardlink { name: name.into(), target: target.into() });
        self
    }

    #[must_use]
    pub fn fifo(mut self, name: &str) -> Self {
        self.entries.push(TarEntry::Fifo { name: name.into() });
        self
    }

    /// The uncompressed tar.
    pub fn tar(&self) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for e in &self.entries {
            let mut h = tar::Header::new_gnu();
            h.set_mtime(1_700_000_000);
            h.set_uid(1000);
            h.set_gid(1000);
            let (name, data, kind, mode, link): (&[u8], &[u8], tar::EntryType, u32, &[u8]) = match e {
                TarEntry::File { name, data, mode } => (name, data, tar::EntryType::Regular, *mode, b""),
                TarEntry::Dir { name } => (name, b"", tar::EntryType::Directory, 0o755, b""),
                TarEntry::Symlink { name, target } => (name, b"", tar::EntryType::Symlink, 0o777, target),
                TarEntry::Hardlink { name, target } => (name, b"", tar::EntryType::Link, 0o644, target),
                TarEntry::Fifo { name } => (name, b"", tar::EntryType::Fifo, 0o644, b""),
            };
            raw_name(&mut h.as_old_mut().name, name);
            raw_name(&mut h.as_old_mut().linkname, link);
            h.set_entry_type(kind);
            h.set_mode(mode);
            h.set_size(data.len() as u64);
            h.set_cksum();
            b.append(&h, data).unwrap_or_else(|e| panic!("tar: {e}"));
        }
        b.into_inner().unwrap_or_else(|e| panic!("tar: {e}"))
    }

    pub fn build(&self) -> Vec<u8> {
        gzip(&self.tar())
    }
}

fn raw_name(field: &mut [u8; 100], name: &[u8]) {
    assert!(name.len() < 100, "test names must fit the old header");
    field.fill(0);
    field[..name.len()].copy_from_slice(name);
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(data).unwrap_or_else(|e| panic!("gzip: {e}"));
    gz.finish().unwrap_or_else(|e| panic!("gzip: {e}"))
}

/// Builds a `.zip` in memory.
#[derive(Debug, Default)]
pub struct Zip {
    entries: Vec<(String, ZipKind)>,
}

#[derive(Debug)]
enum ZipKind {
    File(Vec<u8>, u32),
    Dir,
    Symlink(String),
}

impl Zip {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn file(mut self, name: &str, data: impl Into<Vec<u8>>, mode: u32) -> Self {
        self.entries.push((name.into(), ZipKind::File(data.into(), mode)));
        self
    }

    #[must_use]
    pub fn dir(mut self, name: &str) -> Self {
        self.entries.push((name.into(), ZipKind::Dir));
        self
    }

    #[must_use]
    pub fn symlink(mut self, name: &str, target: &str) -> Self {
        self.entries.push((name.into(), ZipKind::Symlink(target.into())));
        self
    }

    pub fn build(&self) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, kind) in &self.entries {
            let res = match kind {
                ZipKind::File(data, mode) => {
                    let opts = SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated)
                        .unix_permissions(*mode);
                    w.start_file(name.as_str(), opts).and_then(|()| w.write_all(data).map_err(Into::into))
                }
                ZipKind::Dir => w.add_directory(name.as_str(), SimpleFileOptions::default()),
                ZipKind::Symlink(t) => w.add_symlink(name.as_str(), t.as_str(), SimpleFileOptions::default()),
            };
            res.unwrap_or_else(|e| panic!("zip: {e}"));
        }
        w.finish().unwrap_or_else(|e| panic!("zip: {e}")).into_inner()
    }
}

/// The malicious archives every extractor must refuse, by name.
pub fn malicious_tars() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("parent-dir", TarGz::new().file("../evil", "x", 0o644).build()),
        ("nested-parent-dir", TarGz::new().dir("app").file("app/../../evil", "x", 0o644).build()),
        ("absolute-path", TarGz::new().file("/tmp/evil", "x", 0o644).build()),
        ("symlink-absolute", TarGz::new().symlink("app/link", "/etc/passwd").build()),
        ("symlink-escape", TarGz::new().dir("app").symlink("app/link", "../../outside").build()),
        (
            "write-through-symlink",
            TarGz::new()
                .dir("app")
                .symlink("app/sub", ".")
                .symlink("app/up", "sub/..")
                .file("app/up/../x", "x", 0o644)
                .build(),
        ),
        (
            "symlink-then-file-through-it",
            TarGz::new()
                .dir("app")
                .dir("app/real")
                .symlink("app/link", "real")
                .file("app/link/f", "x", 0o644)
                .build(),
        ),
        ("hardlink", TarGz::new().file("a", "x", 0o644).hardlink("b", "a").build()),
        ("hardlink-outside", TarGz::new().hardlink("b", "/etc/shadow").build()),
        ("fifo", TarGz::new().fifo("pipe").build()),
        ("backslash-name", TarGz::new().file("..\\evil", "x", 0o644).build()),
        ("duplicate-entry", TarGz::new().file("a", "1", 0o644).file("a", "2", 0o644).build()),
    ]
}

pub fn malicious_zips() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("parent-dir", Zip::new().file("../evil", "x", 0o644).build()),
        ("absolute-path", Zip::new().file("/tmp/evil", "x", 0o644).build()),
        ("backslash-parent", Zip::new().file("..\\evil", "x", 0o644).build()),
        ("drive-letter", Zip::new().file("C:/evil", "x", 0o644).build()),
        ("symlink-absolute", Zip::new().symlink("link", "/etc/passwd").build()),
        ("symlink-escape", Zip::new().dir("app/").symlink("app/link", "../../outside").build()),
        (
            "symlink-then-file-through-it",
            Zip::new().dir("app/").symlink("app/link", ".").file("app/link/f", "x", 0o644).build(),
        ),
        ("duplicate-entry", Zip::new().file("a", "1", 0o644).file("./a", "2", 0o644).build()),
    ]
}

/// A tar.gz bomb: `n` MiB of zeros in one file (a few KiB compressed).
pub fn tar_bomb(mib: usize) -> Vec<u8> {
    TarGz::new().file("zeros", vec![0u8; mib << 20], 0o644).build()
}

/// A zip bomb: `n` MiB of zeros in one entry.
pub fn zip_bomb(mib: usize) -> Vec<u8> {
    Zip::new().file("zeros", vec![0u8; mib << 20], 0o644).build()
}

/// Many tiny entries.
pub fn tar_many_entries(n: usize) -> Vec<u8> {
    (0..n).fold(TarGz::new(), |t, i| t.file(&format!("f{i}"), "", 0o644)).build()
}
