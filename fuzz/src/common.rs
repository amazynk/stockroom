//! Shared by the archive targets.

use std::path::Path;

/// Small limits keep each run fast; bombs must hit them, not the disk.
pub fn limits() -> suite_unpack::Limits {
    suite_unpack::Limits { max_total_bytes: 4 << 20, max_file_bytes: 1 << 20, max_entries: 512 }
}

/// Only `out` may exist in `base`, and every symlink inside must resolve inside.
pub fn assert_contained(base: &Path) {
    let names: Vec<_> = std::fs::read_dir(base).expect("read base").map(|e| e.expect("entry").file_name()).collect();
    assert!(names.is_empty() || names == ["out"], "escaped: {names:?}");
}
