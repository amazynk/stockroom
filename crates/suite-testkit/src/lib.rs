//! Test support for Stockroom: a local fake release server, catalog builders and archive
//! builders, including the malicious archives every extractor must refuse.

pub mod archive;
mod server;

pub use server::{Fault, Server};
use sha2::{Digest, Sha256};
use suite_catalog::{
    App, AppStatus, Arch, Asset, AssetKind, Catalog, Channel, DataPaths, HostFacts, Layout, Os, PlatformIds,
    Release, Requirements, Sha256Hex,
};
use time::{Duration, OffsetDateTime};

pub fn sha256(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_digest(&Sha256::digest(bytes))
}

/// A typical computer of the given OS with the processor the tests run on, which is the one
/// [`Fixture`] publishes its assets for.
pub fn facts(os: Os) -> HostFacts {
    HostFacts { arch: Arch::current(), ..HostFacts::example(os) }
}

/// The desktop ID used by [`fake_app`].
pub fn desktop_id(app: &str) -> String {
    format!("com.example.{app}")
}

/// A Linux app tarball laid out like Specular's: one top folder holding the executable and
/// `share/{applications,icons}`. The executable is a shell script printing its version.
pub fn fake_app(app: &str, version: &str) -> Vec<u8> {
    let script = format!("#!/bin/sh\necho \"{app} {version}\"\n");
    fake_app_with_exe(app, version, script.into_bytes())
}

/// Like [`fake_app`] with the given executable contents.
pub fn fake_app_with_exe(app: &str, version: &str, exe: Vec<u8>) -> Vec<u8> {
    let top = format!("{app}-{version}-linux-x86_64");
    let id = desktop_id(app);
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName={app}\nExec={app} %F\nTryExec={app}\nIcon={id}\n\
         MimeType=image/x-{app};\n\n[Desktop Action new]\nName=New\nExec={app} --new\n"
    );
    archive::TarGz::new()
        .dir(&format!("{top}/"))
        .file(&format!("{top}/{app}"), exe, 0o755)
        .file(&format!("{top}/share/applications/{id}.desktop"), desktop, 0o644)
        .file(&format!("{top}/share/icons/hicolor/scalable/apps/{id}.svg"), "<svg/>", 0o644)
        .file(&format!("{top}/share/doc/README"), "readme", 0o644)
        .build()
}

/// The catalog layout of [`fake_app`].
pub fn fake_layout(app: &str) -> Layout {
    let id = desktop_id(app);
    Layout {
        executable: Some(app.to_owned()),
        icon: Some(format!("share/icons/hicolor/scalable/apps/{id}.svg")),
        desktop: Some(format!("share/applications/{id}.desktop")),
        file_types: Vec::new(),
    }
}

/// An app entry with no releases.
pub fn app_entry(id: &str) -> App {
    App {
        id: id.to_owned(),
        name: id.to_owned(),
        pitch: format!("{id} does things"),
        description: String::new(),
        icon: None,
        homepage: "https://example.com".into(),
        license: "MIT".into(),
        category: "Utility".into(),
        status: AppStatus::Available,
        ids: PlatformIds {
            desktop: Some(desktop_id(id)),
            flatpak: Some(desktop_id(id)),
            executables: vec![id.to_owned()],
            ..PlatformIds::default()
        },
        data_paths: DataPaths { linux: vec![format!("~/.config/{id}")], ..DataPaths::default() },
        releases: Vec::new(),
        vendor: None,
        part: None,
        mobile: suite_catalog::Mobile::default(),
        extras: Vec::new(),
    }
}

/// A catalog published yesterday.
pub fn catalog(version: u64, apps: Vec<App>) -> Catalog {
    let now = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap_or_else(|e| panic!("{e}"));
    Catalog { schema: suite_catalog::SCHEMA, version, published: now - Duration::days(1), apps }
}

/// A release server: serves apps at `/assets/...` and the catalog at `/catalog.json`.
#[derive(Debug)]
pub struct Fixture {
    pub server: Server,
    pub catalog: Catalog,
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Fixture {
    pub fn new() -> Self {
        Self { server: Server::start(), catalog: catalog(1, Vec::new()) }
    }

    pub fn catalog_url(&self) -> String {
        self.server.url("/catalog.json")
    }

    /// Serves `bytes` as a release asset of `app` and adds the release to the catalog.
    pub fn add_release(&mut self, app: &str, version: &str, kind: AssetKind, bytes: Vec<u8>) -> Asset {
        self.add_release_on(app, version, Channel::Stable, kind, bytes)
    }

    pub fn add_release_on(
        &mut self,
        app: &str,
        version: &str,
        channel: Channel,
        kind: AssetKind,
        bytes: Vec<u8>,
    ) -> Asset {
        let path = format!("/assets/{app}-{version}.{kind}");
        let asset = Asset {
            os: Os::Linux,
            arch: Arch::current(),
            kind,
            url: self.server.url(&path),
            size: bytes.len() as u64,
            sha256: Some(sha256(&bytes)),
            requires: Requirements::default(),
            layout: if kind == AssetKind::TarGz { fake_layout(app) } else { Layout::default() },
            store: None,
            pinned: false,
            note: None,
        };
        self.server.put(&path, bytes);
        if self.catalog.app(app).is_none() {
            self.catalog.apps.push(app_entry(app));
        }
        let entry = self.catalog.apps.iter_mut().find(|a| a.id == app).unwrap_or_else(|| unreachable!());
        let version: semver::Version = version.parse().unwrap_or_else(|e| panic!("{e}"));
        entry.releases.retain(|r| r.version != version);
        entry.releases.push(Release {
            version,
            channel,
            date: None,
            notes_url: None,
            assets: vec![asset.clone()],
        });
        asset
    }

    /// Serves the current catalog, bumping its serial.
    pub fn publish(&mut self) {
        self.catalog.version += 1;
        let json = self.catalog.to_json_pretty().unwrap_or_else(|e| panic!("{e}"));
        self.server.put("/catalog.json", json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_serves_a_valid_catalog() {
        let mut f = Fixture::new();
        f.add_release("demo", "1.0.0", AssetKind::TarGz, fake_app("demo", "1.0.0"));
        f.publish();
        let c = Catalog::from_json(&fetch(&f.server, "/catalog.json")).unwrap();
        assert_eq!(c.apps[0].releases.len(), 1);
    }

    fn fetch(server: &Server, path: &str) -> Vec<u8> {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(server.base_url().trim_start_matches("http://")).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        let pos = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        out.split_off(pos + 4)
    }

    #[test]
    fn malicious_archives_build() {
        assert_ne!(archive::malicious_tars(), [] as [(&str, std::vec::Vec<u8>); 0]);
        assert_ne!(archive::malicious_zips(), [] as [(&str, std::vec::Vec<u8>); 0]);
        assert!(archive::tar_bomb(4).len() < 64 * 1024);
    }
}
