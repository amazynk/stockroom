#![no_main]

use suite_fuzz_common::{assert_contained, limits};

#[path = "../src/common.rs"]
mod suite_fuzz_common;

// Any zip: extraction never panics and never writes outside its folder.
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let base = tempfile::tempdir().expect("temp dir");
    let _ = suite_unpack::extract_zip_reader(std::io::Cursor::new(data), &base.path().join("out"), &limits());
    assert_contained(base.path());
});
