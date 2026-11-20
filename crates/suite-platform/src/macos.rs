//! macOS app bundles, read with the `plist` crate on any OS (tests build bundles in
//! temporary folders on Linux).

use std::path::Path;

/// What an `.app` bundle's `Contents/Info.plist` says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleInfo {
    /// `CFBundleIdentifier`.
    pub id: String,
    /// `CFBundleShortVersionString`.
    pub version: Option<String>,
    /// `CFBundleExecutable`: the program in `Contents/MacOS`.
    pub executable: Option<String>,
}

/// Reads `bundle/Contents/Info.plist` (XML or binary). `None` when it's missing, unreadable
/// or has no identifier.
pub fn bundle_info(bundle: &Path) -> Option<BundleInfo> {
    let v = plist::Value::from_file(bundle.join("Contents").join("Info.plist")).ok()?;
    let d = v.as_dictionary()?;
    let text = |key: &str| d.get(key).and_then(plist::Value::as_string).map(str::to_owned);
    Some(BundleInfo {
        id: text("CFBundleIdentifier")?,
        version: text("CFBundleShortVersionString"),
        executable: text("CFBundleExecutable"),
    })
}

/// A minimal `Info.plist` for tests and fixtures.
pub fn info_plist(id: &str, version: &str, executable: &str) -> String {
    let esc = crate::scheduler::xml_escape;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>{}</string>
    <key>CFBundleShortVersionString</key><string>{}</string>
    <key>CFBundleExecutable</key><string>{}</string>
    <key>CFBundlePackageType</key><string>APPL</string>
</dict>
</plist>
"#,
        esc(id),
        esc(version),
        esc(executable)
    )
}

/// Whether the current user can create entries in `dir` (`/Applications` for an install for
/// everyone, which on macOS admins can write without elevating). Tries a probe file, since
/// permission bits alone don't account for ACLs.
pub fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".suite-probe-{}", std::process::id()));
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_info_plist() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Demo & Co.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        assert_eq!(bundle_info(&app), None);
        std::fs::write(app.join("Contents/Info.plist"), info_plist("com.example.demo", "2.1", "demo"))
            .unwrap();
        let info = bundle_info(&app).unwrap();
        assert_eq!(info.id, "com.example.demo");
        assert_eq!(info.version.as_deref(), Some("2.1"));
        assert_eq!(info.executable.as_deref(), Some("demo"));
        std::fs::write(app.join("Contents/Info.plist"), "<plist/>").unwrap();
        assert_eq!(bundle_info(&app), None);
        assert!(writable(dir.path()));
        assert!(!writable(&dir.path().join("missing")));
    }
}
