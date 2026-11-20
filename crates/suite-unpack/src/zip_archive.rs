use std::{
    fs::File,
    io::{Read, Seek},
    path::Path,
};

use crate::{Limits, Summary, UnpackError, safe::Writer, safe_relative};

/// Extracts the `.zip` at `archive` into `dest` (created; must be empty).
pub fn extract_zip(archive: &Path, dest: &Path, limits: &Limits) -> Result<Summary, UnpackError> {
    let f = File::open(archive).map_err(|e| UnpackError::io(archive, e))?;
    extract_zip_reader(std::io::BufReader::new(f), dest, limits)
}

/// Extracts a zip from a seekable reader into `dest`.
pub fn extract_zip_reader(
    reader: impl Read + Seek,
    dest: &Path,
    limits: &Limits,
) -> Result<Summary, UnpackError> {
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| UnpackError::Corrupt(e.to_string()))?;
    if archive.len() as u64 > limits.max_entries {
        return Err(UnpackError::TooBig("too many entries"));
    }
    let mut w = Writer::new(dest, limits)?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| UnpackError::Corrupt(e.to_string()))?;
        w.count_entry()?;
        let name = file.name().map_err(|e| UnpackError::Corrupt(e.to_string()))?.into_owned();
        if file.encrypted() {
            return Err(UnpackError::unsafe_entry(&name, "encrypted entry"));
        }
        let rel = safe_relative(&name)?;
        if file.is_symlink() {
            // The link target is the entry's content; a sane one is short.
            let mut target = String::new();
            (&mut file)
                .take(4096)
                .read_to_string(&mut target)
                .map_err(|_| UnpackError::unsafe_entry(&name, "symlink target is not UTF-8"))?;
            w.symlink(&name, &rel, &target)?;
        } else if file.is_dir() {
            w.dir(&name, &rel)?;
        } else {
            if file.size() > limits.max_file_bytes {
                return Err(UnpackError::TooBig("file too large"));
            }
            let mode = file.unix_mode();
            w.file(&name, &rel, &mut file, mode)?;
        }
    }
    w.finish()
}
