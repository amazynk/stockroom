use std::{fs::File, io::Read, path::Path};

use flate2::read::GzDecoder;
use tar::EntryType;

use crate::{Limits, Summary, UnpackError, safe::Writer, safe_relative};

/// Extracts the `.tar.gz` at `archive` into `dest` (created; must be empty).
pub fn extract_tar_gz(archive: &Path, dest: &Path, limits: &Limits) -> Result<Summary, UnpackError> {
    let f = File::open(archive).map_err(|e| UnpackError::io(archive, e))?;
    extract_tar_gz_reader(std::io::BufReader::new(f), dest, limits)
}

/// Extracts a gzip-compressed tar stream into `dest`.
pub fn extract_tar_gz_reader(
    reader: impl Read,
    dest: &Path,
    limits: &Limits,
) -> Result<Summary, UnpackError> {
    extract_tar_reader(GzDecoder::new(reader), dest, limits)
}

/// Extracts an uncompressed tar stream into `dest`.
pub fn extract_tar_reader(reader: impl Read, dest: &Path, limits: &Limits) -> Result<Summary, UnpackError> {
    let mut archive = tar::Archive::new(reader);
    let mut w = Writer::new(dest, limits)?;
    let entries = archive.entries().map_err(|e| UnpackError::Corrupt(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| UnpackError::Corrupt(e.to_string()))?;
        w.count_entry()?;
        let kind = entry.header().entry_type();
        // Metadata records (long names, PAX) are applied by the tar crate; nothing to write.
        if matches!(
            kind,
            EntryType::XGlobalHeader | EntryType::XHeader | EntryType::GNULongName | EntryType::GNULongLink
        ) {
            continue;
        }
        let raw = entry.path_bytes();
        let name = std::str::from_utf8(&raw)
            .map_err(|_| UnpackError::unsafe_entry(&String::from_utf8_lossy(&raw), "name is not UTF-8"))?
            .to_owned();
        let rel = safe_relative(&name)?;
        match kind {
            EntryType::Directory => w.dir(&name, &rel)?,
            EntryType::Regular | EntryType::Continuous => {
                let mode = entry.header().mode().ok();
                if entry.header().size().unwrap_or(0) > limits.max_file_bytes {
                    return Err(UnpackError::TooBig("file too large"));
                }
                w.file(&name, &rel, &mut entry, mode)?;
            }
            EntryType::Symlink => {
                let target = entry
                    .link_name_bytes()
                    .ok_or_else(|| UnpackError::unsafe_entry(&name, "symlink without target"))?;
                let target = std::str::from_utf8(&target)
                    .map_err(|_| UnpackError::unsafe_entry(&name, "symlink target is not UTF-8"))?
                    .to_owned();
                w.symlink(&name, &rel, &target)?;
            }
            EntryType::Link => return Err(UnpackError::unsafe_entry(&name, "hard link")),
            _ => return Err(UnpackError::unsafe_entry(&name, "device, fifo or unknown entry type")),
        }
    }
    w.finish()
}
