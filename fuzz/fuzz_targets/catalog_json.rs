#![no_main]

// Any bytes: parsing never panics, and whatever parses survives a round trip unchanged.
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if let Ok(catalog) = suite_catalog::Catalog::from_json(data) {
        let json = catalog.to_json_pretty().expect("a parsed catalog serialises");
        let again = suite_catalog::Catalog::from_json(&json).expect("a serialised catalog parses");
        assert_eq!(again, catalog);
    }
});
