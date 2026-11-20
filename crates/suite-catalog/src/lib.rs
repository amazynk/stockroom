//! Stockroom's catalog: what apps exist, which releases and assets they have, the rules a
//! catalog must keep, and which asset suits this computer (PLAN.md §3).

mod error;
pub mod host;
pub mod model;
pub mod select;

pub use error::CatalogError;
pub use host::{Arch, DottedVersion, HostFacts, Os};
pub use model::{
    App, AppStatus, Asset, AssetKind, Catalog, Channel, DataPaths, Extra, Icon, Layout, Mobile, PlatformIds,
    Release, Requirements, SCHEMA, Sha256Hex, Store, StoreLink,
};
pub use select::{NotInstallable, Prefs, Unmet, best_installable, choose_asset, releases_on};

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "schema": 1, "version": 1,
      "published": "2026-11-01T00:00:00Z",
      "apps": [{
        "id": "specular", "name": "Specular", "pitch": "Open and flip through large photos instantly",
        "homepage": "https://amazynk.com/specular", "license": "MIT OR Apache-2.0",
        "ids": {"desktop": "app.specular.Specular", "executables": ["specular"]},
        "data_paths": {"linux": ["~/.config/specular"]},
        "releases": [{"version": "0.1.1", "assets": [{
          "os": "linux", "arch": "x86_64", "kind": "tar.gz",
          "url": "https://example.com/s.tar.gz", "size": 10,
          "sha256": "973D4DD3C1F40197AE576F489239DA1747DF9A75858750BD9C55BD78F329210D",
          "requires": {"x86_64_v3": true, "glibc": "2.35"},
          "layout": {"executable": "specular", "desktop": "share/applications/app.specular.Specular.desktop"}
        }]}],
        "future_field": "ignored"
      }]
    }"#;

    #[test]
    fn parses_and_round_trips() {
        let c = Catalog::from_json(SAMPLE.as_bytes()).unwrap();
        let app = c.app("specular").unwrap();
        assert_eq!(app.status, AppStatus::Available);
        let a = &app.releases[0].assets[0];
        assert_eq!(a.kind, AssetKind::TarGz);
        assert_eq!(
            a.sha256.as_ref().unwrap().as_str(),
            "973d4dd3c1f40197ae576f489239da1747df9a75858750bd9c55bd78f329210d"
        );
        let again = Catalog::from_json(&c.to_json_pretty().unwrap()).unwrap();
        assert_eq!(again, c);
    }

    fn with(from: &str, to: &str) -> Result<Catalog, CatalogError> {
        assert!(SAMPLE.contains(from), "{from}");
        Catalog::from_json(SAMPLE.replace(from, to).as_bytes())
    }

    #[test]
    fn rejects_unsafe_or_wrong_content() {
        assert!(matches!(with(r#""schema": 1"#, r#""schema": 2"#), Err(CatalogError::UnsupportedSchema(2))));
        for (from, to) in [
            (r#""id": "specular""#, r#""id": "../evil""#),
            (r#""id": "specular""#, r#""id": "Spec ular""#),
            (r#""executable": "specular""#, r#""executable": "../../bin/sh""#),
            (r#""executable": "specular""#, r#""executable": "/bin/sh""#),
            ("https://example.com/s.tar.gz", "http://example.com/s.tar.gz"),
            ("https://example.com/s.tar.gz", "file:///etc/passwd"),
            (r#""size": 10"#, r#""size": 0"#),
            ("~/.config/specular", "/etc"),
            ("~/.config/specular", "~/../.."),
        ] {
            assert!(matches!(with(from, to), Err(CatalogError::Invalid(_))), "{to}");
        }
        assert!(matches!(Catalog::from_json(b"{"), Err(CatalogError::Malformed(_))));
        assert!(matches!(with(r#""apps": ["#, r#""apps": {}, "old": ["#), Err(CatalogError::Malformed(_))));
    }

    #[test]
    fn entries_from_a_newer_catalog_are_left_out_not_fatal() {
        let assets = |c: &Catalog| c.app("specular").map(|a| a.releases[0].assets.len());
        // A package kind, a store or a checksum this build can't read: that asset is not
        // offered, and the rest of the list works.
        assert_eq!(assets(&with(r#""kind": "tar.gz""#, r#""kind": "rpm""#).unwrap()), Some(0));
        assert_eq!(assets(&with("973D4DD3", "XYZ").unwrap()), Some(0));
        assert_eq!(assets(&with(r#""arch": "x86_64""#, r#""arch": "riscv64""#).unwrap()), Some(0));
        // A release with a version it can't order, and an app in a state it doesn't know.
        let c = with(r#""version": "0.1.1""#, r#""version": "next""#).unwrap();
        assert_eq!(c.app("specular").unwrap().releases.len(), 0);
        let c =
            with(r#""license": "MIT OR Apache-2.0","#, r#""license": "MIT", "status": "preview","#).unwrap();
        assert_eq!(c.apps.len(), 0);
        // Beside one it does understand.
        let newer = r#"{"id": "later", "name": "Later", "pitch": "p", "homepage": "https://example.com",
                        "license": "MIT", "status": "preview"}, {"id": "specular""#;
        let c = with(
            r#"{
        "id": "specular""#,
            newer,
        )
        .unwrap();
        assert_eq!(c.apps.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["specular"]);
        assert_eq!(assets(&c), Some(1));
        // Unsafe content in an entry it does read still refuses the whole list.
        assert!(matches!(with(r#""id": "specular""#, r#""id": "../evil""#), Err(CatalogError::Invalid(_))));
    }

    #[test]
    fn store_assets_and_mobile_links() {
        let store = r#"{"os": "windows", "arch": "x86_64", "kind": "store",
            "url": "https://apps.microsoft.com/store/detail/zynk/9PP9CNT87P3F",
            "store": {"store": "microsoft", "product_id": "9PP9CNT87P3F",
                      "url": "https://apps.microsoft.com/store/detail/zynk/9PP9CNT87P3F",
                      "link": "LINK"}}"#;
        let json = |link: &str| {
            SAMPLE
                .replace(r#""releases": [{"version": "0.1.1", "assets": [{"#, &format!(r#""releases": [{{"version": "0.1.1", "assets": [{}, {{"#, store.replace("LINK", link)))
                .replace(r#""future_field""#, r#""mobile": {"android": "https://play.google.com/store/apps/details?id=it.zynk"}, "vendor": "Zynk", "future_field""#)
        };
        let c =
            Catalog::from_json(json("ms-windows-store://pdp/?productid=9PP9CNT87P3F").as_bytes()).unwrap();
        let app = c.app("specular").unwrap();
        assert_eq!(app.vendor.as_deref(), Some("Zynk"));
        assert!(app.mobile.android.is_some());
        let a = &app.releases[0].assets[0];
        assert_eq!(
            (a.kind, a.sha256.as_ref(), a.store.as_ref().unwrap().store),
            (AssetKind::Store, None, Store::Microsoft)
        );
        // The deep link must use the store's own scheme.
        let r = Catalog::from_json(json("https://evil.example/x.exe").as_bytes());
        assert!(matches!(r, Err(CatalogError::Invalid(_))), "{r:?}");
        // A downloadable asset without a checksum is refused.
        let r = Catalog::from_json(
            SAMPLE
                .replace(
                    r#""sha256": "973D4DD3C1F40197AE576F489239DA1747DF9A75858750BD9C55BD78F329210D","#,
                    "",
                )
                .as_bytes(),
        );
        assert!(matches!(r, Err(CatalogError::Invalid(_))), "{r:?}");
    }

    #[test]
    fn loopback_http_is_allowed_for_tests() {
        assert!(with("https://example.com/s.tar.gz", "http://127.0.0.1:8080/s.tar.gz").is_ok());
        assert!(with("https://example.com/s.tar.gz", "http://localhost:8080/s.tar.gz").is_ok());
    }
}
