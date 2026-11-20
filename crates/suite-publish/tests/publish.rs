//! Publishing against a fake GitHub API on the local test server.

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use suite_catalog::{AppStatus, AssetKind, Catalog, Channel};
use suite_fetch::Fetcher;
use suite_publish::{AppMeta, Options, PublishError, build, read_apps, write_catalog};
use suite_testkit::{Server, fake_app, sha256};

const META: &str = r#"
id = "demo"
name = "Demo"
pitch = "Does things"
homepage = "https://example.com/demo"
license = "MIT"
github = "example/demo"

[ids]
desktop = "com.example.demo"
executables = ["demo"]

[[assets]]
name = "demo-{version}-linux-x86_64.tar.gz"
os = "linux"
arch = "x86_64"
kind = "tar.gz"
requires = { x86_64_v3 = true, glibc = "2.35" }
layout = { executable = "demo" }

[[assets]]
name = "Demo-{version}-windows-x64.zip"
os = "windows"
arch = "x86_64"
kind = "zip"
"#;

const SOON: &str = r#"
id = "later"
name = "Later"
pitch = "Coming soon"
homepage = "https://example.com/later"
license = "GPL-3.0-or-later"
status = "unreleased"
github = "example/later"
"#;

/// Serves releases like GitHub does: the API listing plus the files and their .sha256.
fn serve_release(
    server: &Server,
    version: &str,
    prerelease: bool,
    files: &[(&str, Vec<u8>, Option<String>)],
) -> serde_json::Value {
    let mut assets = Vec::new();
    for (name, bytes, sum) in files {
        let path = format!("/dl/v{version}/{name}");
        server.put(&path, bytes.clone());
        assets.push(
            serde_json::json!({"name": name, "size": bytes.len(), "browser_download_url": server.url(&path)}),
        );
        if let Some(sum) = sum {
            let sum_name = format!("{name}.sha256");
            let p = format!("/dl/v{version}/{sum_name}");
            server.put(&p, sum.clone());
            assets.push(serde_json::json!({"name": sum_name, "size": sum.len(), "browser_download_url": server.url(&p)}));
        }
    }
    serde_json::json!({
        "tag_name": format!("v{version}"), "draft": false, "prerelease": prerelease,
        "html_url": format!("https://github.com/example/demo/releases/tag/v{version}"),
        "published_at": "2026-11-13T20:59:33Z", "assets": assets
    })
}

fn apps_dir(dir: &Path) -> std::path::PathBuf {
    let apps = dir.join("apps");
    std::fs::create_dir_all(&apps).unwrap();
    std::fs::write(apps.join("demo.toml"), META).unwrap();
    std::fs::write(apps.join("later.toml"), SOON).unwrap();
    apps
}

#[test]
fn icons_are_published_under_their_checksum() {
    let dir = tempfile::tempdir().unwrap();
    let apps_path = apps_dir(dir.path());
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), b"pixels"].concat();
    std::fs::write(apps_path.join("later.png"), &png).unwrap();
    std::fs::write(apps_path.join("later.toml"), format!("{SOON}icon = \"later.png\"\n")).unwrap();
    let apps = read_apps(&apps_path).unwrap();
    let server = Server::start();
    server.put("/repos/example/demo/releases", b"[]".to_vec());
    let o = Options { icon_base: "https://example.com/stockroom/icons/".into(), ..opts(&server) };
    let catalog = build(&apps, &Fetcher::default(), &o, &mut |_| {}).unwrap();
    let icon = catalog.app("later").unwrap().icon.clone().unwrap();
    assert_eq!(icon.sha256, sha256(&png));
    let name = format!("later-{}.png", &icon.sha256.as_str()[..12]);
    assert_eq!(icon.url, format!("https://example.com/stockroom/icons/{name}"));
    assert_eq!(catalog.app("demo").unwrap().icon, None);
    let out = dir.path().join("out");
    assert_eq!(suite_publish::write_icons(&apps, &out).unwrap(), 1);
    assert_eq!(std::fs::read(out.join("icons").join(name)).unwrap(), png);
    // Not a PNG, or a file somewhere else: refused when the metadata is read.
    std::fs::write(apps_path.join("later.png"), b"GIF89a").unwrap();
    assert!(matches!(read_apps(&apps_path), Err(PublishError::Meta { .. })));
    std::fs::write(apps_path.join("later.toml"), format!("{SOON}icon = \"../later.png\"\n")).unwrap();
    assert!(matches!(read_apps(&apps_path), Err(PublishError::Meta { .. })));
}

fn opts(server: &Server) -> Options {
    Options { api: server.base_url(), serial: Some(42), ..Options::default() }
}

#[test]
fn builds_checks_and_writes() {
    let server = Server::start();
    let tgz = |v: &str| fake_app("demo", v);
    let ok = |name: &str, bytes: Vec<u8>| {
        let sum = format!("{}  {name}\n", sha256(&bytes));
        (name.to_owned(), bytes, Some(sum))
    };
    let r1 = {
        let (n, b, s) = ok("demo-1.0.0-linux-x86_64.tar.gz", tgz("1.0.0"));
        let (n2, b2, s2) = ok("Demo-1.0.0-windows-x64.zip", b"zip".to_vec());
        serve_release(
            &server,
            "1.0.0",
            false,
            &[(&n, b, s), (&n2, b2, s2), ("notes.txt", b"x".to_vec(), None)],
        )
    };
    let r2 = {
        let (n, b, s) = ok("demo-1.1.0-beta.1-linux-x86_64.tar.gz", tgz("1.1.0-beta.1"));
        // No .sha256 for this one: skipped.
        serve_release(
            &server,
            "1.1.0-beta.1",
            true,
            &[(&n, b, s), ("Demo-1.1.0-beta.1-windows-x64.zip", b"zip".to_vec(), None)],
        )
    };
    let draft = serde_json::json!({"tag_name": "v9.0.0", "draft": true, "prerelease": false, "assets": []});
    let junk = serde_json::json!({"tag_name": "nightly", "draft": false, "prerelease": true, "assets": []});
    server.put(
        "/repos/example/demo/releases",
        serde_json::to_vec(&serde_json::json!([r2, draft, junk, r1])).unwrap(),
    );

    let dir = tempfile::tempdir().unwrap();
    let apps = read_apps(&apps_dir(dir.path())).unwrap();
    let catalog = build(&apps, &Fetcher::default(), &opts(&server), &mut |_| {}).unwrap();
    assert_eq!(catalog.version, 42);
    let demo = catalog.app("demo").unwrap();
    let versions: Vec<(String, Channel, usize)> =
        demo.releases.iter().map(|r| (r.version.to_string(), r.channel, r.assets.len())).collect();
    assert_eq!(versions, [("1.1.0-beta.1".into(), Channel::Beta, 1), ("1.0.0".into(), Channel::Stable, 2)]);
    let a = &demo.releases[1].assets[0];
    assert_eq!((a.kind, a.requires.glibc.as_deref()), (AssetKind::TarGz, Some("2.35")));
    assert_eq!(catalog.app("later").unwrap().status, AppStatus::Unreleased);
    assert_eq!(catalog.app("later").unwrap().releases, [] as [suite_catalog::Release; 0]);
    assert_eq!(server.hits("/repos/example/later/releases"), 0);

    // Publishing again with the last catalog at hand downloads nothing it checked before.
    let file = "/dl/v1.0.0/demo-1.0.0-linux-x86_64.tar.gz";
    let before = server.hits(file);
    assert!(before > 0);
    let again = Options { reuse: Some(catalog.clone()), ..opts(&server) };
    let second = build(&apps, &Fetcher::default(), &again, &mut |_| {}).unwrap();
    assert_eq!(second.apps, catalog.apps);
    assert_eq!(server.hits(file), before);
    // A file whose published checksum changed is checked afresh, and caught if it lies.
    server.put(&format!("{file}.sha256"), format!("{}  replaced\n", sha256(b"other bytes")));
    let r = build(&apps, &Fetcher::default(), &again, &mut |_| {});
    assert!(matches!(r, Err(PublishError::Checksum { .. })), "{r:?}");
    assert!(server.hits(file) > before);
    server.put(&format!("{file}.sha256"), format!("{}  restored\n", sha256(&tgz("1.0.0"))));

    // The file written is the catalog built.
    let out = dir.path().join("out");
    write_catalog(&catalog, &out).unwrap();
    assert_eq!(Catalog::from_json(&std::fs::read(out.join("catalog.json")).unwrap()).unwrap(), catalog);
}

#[test]
fn a_wrong_checksum_stops_the_publish() {
    let server = Server::start();
    let bytes = fake_app("demo", "1.0.0");
    let lie = format!("{}  demo-1.0.0-linux-x86_64.tar.gz\n", sha256(b"something else"));
    let r = serve_release(&server, "1.0.0", false, &[("demo-1.0.0-linux-x86_64.tar.gz", bytes, Some(lie))]);
    server.put("/repos/example/demo/releases", serde_json::to_vec(&serde_json::json!([r])).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let apps: Vec<AppMeta> = read_apps(&apps_dir(dir.path())).unwrap();
    let err = build(&apps, &Fetcher::default(), &opts(&server), &mut |_| {}).unwrap_err();
    assert!(matches!(err, PublishError::Checksum { .. }), "{err}");
}

#[test]
fn bad_metadata_is_reported_with_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let apps = dir.path().join("apps");
    std::fs::create_dir_all(&apps).unwrap();
    std::fs::write(
        apps.join("x.toml"),
        "id = \"Bad Id\"\nname = \"x\"\npitch = \"\"\nhomepage = \"\"\nlicense = \"\"\n",
    )
    .unwrap();
    let err = read_apps(&apps).unwrap_err();
    assert!(err.to_string().contains("x.toml"), "{err}");
    std::fs::write(apps.join("x.toml"), "id = \"x\"\nunknown_key = 1\n").unwrap();
    assert!(matches!(read_apps(&apps), Err(PublishError::Meta { .. })));
}

const PINNED: &str = r#"
id = "vend"
name = "Vend"
vendor = "Vend Inc."
pitch = "Not ours"
homepage = "https://example.com/vend"
license = "Proprietary (freeware)"
category = "Transfer"

[mobile]
android = "https://play.google.com/store/apps/details?id=example.vend"

[[assets]]
latest = "LATEST_UI"
name = "vend-ui-linux-x86_64-{build}.AppImage"
os = "linux"
arch = "x86_64"
kind = "appimage"

[[assets]]
os = "windows"
arch = "x86_64"
kind = "store"
store = { store = "microsoft", product_id = "9ABC", url = "https://apps.microsoft.com/store/detail/vend/9ABC", link = "ms-windows-store://pdp/?productid=9ABC" }

[[extras]]
id = "cli"
name = "Command-line tool"

[[extras.assets]]
latest = "LATEST_CLI"
name = "vend-cli-linux-x86_64-{version}.tgz"
os = "linux"
arch = "x86_64"
kind = "tar.gz"
"#;

#[test]
fn vendor_downloads_without_checksums_are_pinned() {
    let server = Server::start();
    let ui = b"\x7fELF\x02\x01\x01\x00AI\x02 the app".to_vec();
    server.put("/latest/vend-ui-linux-x86_64-0a1b2c3d4e5f.AppImage", ui.clone());
    server.put("/latest/vend-cli-linux-x86_64-v1.6.0.tgz", fake_app("vend", "1.6.0"));
    let dir = tempfile::tempdir().unwrap();
    let apps_dir = apps_dir(dir.path());
    let meta = PINNED
        .replace("LATEST_UI", &server.url("/latest/vend-ui-linux-x86_64-0a1b2c3d4e5f.AppImage"))
        .replace("LATEST_CLI", &server.url("/latest/vend-cli-linux-x86_64-v1.6.0.tgz"));
    // "aaa" sorts first by file name; the vendor app must still come after the family.
    std::fs::write(apps_dir.join("aaa-vend.toml"), meta).unwrap();
    let mut apps = read_apps(&apps_dir).unwrap();
    apps.retain(|a| a.id != "demo");
    let catalog = build(&apps, &Fetcher::default(), &opts(&server), &mut |_| {}).unwrap();
    let ids: Vec<&str> = catalog.apps.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["later", "vend"]);
    let vend = catalog.app("vend").unwrap();
    assert_eq!(
        vend.mobile.android.as_deref(),
        Some("https://play.google.com/store/apps/details?id=example.vend")
    );
    let r = &vend.releases[0];
    assert!(r.version.to_string().ends_with("+build.0a1b2c3d4e5f"), "{}", r.version);
    let ui_asset = r.assets.iter().find(|a| a.kind == AssetKind::AppImage).unwrap();
    assert!(ui_asset.pinned);
    assert_eq!(ui_asset.sha256, Some(sha256(&ui)));
    assert_eq!(ui_asset.size, ui.len() as u64);
    assert!(ui_asset.note.as_deref().unwrap().contains("Vend Inc. publishes no checksum"));
    let store = r.assets.iter().find(|a| a.kind == AssetKind::Store).unwrap();
    assert_eq!(store.store.as_ref().unwrap().product_id, "9ABC");
    assert!(!store.pinned && store.sha256.is_none());
    let cli = &vend.extras[0];
    assert_eq!(cli.version.as_ref().unwrap().to_string(), "1.6.0");
    assert!(cli.assets[0].pinned);

    // A "latest" URL serving something unexpected stops the publish.
    server.remove("/latest/vend-ui-linux-x86_64-0a1b2c3d4e5f.AppImage");
    let err = build(&apps, &Fetcher::default(), &opts(&server), &mut |_| {}).unwrap_err();
    assert!(matches!(err, PublishError::Fetch(_)), "{err}");
}
