//! Fake family data for the demo and the window tests: a local release server (suite-testkit)
//! with a catalog, a computer that lives in a temporary folder, and a few apps already
//! installed. Nothing touches the real home, registry, scheduler or network.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "test support"
)]

use std::{path::Path, sync::Arc, time::Duration};

use suite_catalog::{AppStatus, AssetKind, Channel, Mobile, Os, Requirements};
use suite_core::{CatalogSource, Core};
use suite_fetch::{Fetcher, Options};
use suite_install::Policy;
use suite_platform::{
    Env, FakeRunner, Metered, Output, Paths, network::FixedNetwork, process::FakeProcesses,
    registry::MemRegistry,
};
use suite_testkit::{Fixture, fake_app};
use time::OffsetDateTime;

pub(crate) struct Family {
    pub(crate) fixture: Fixture,
    pub(crate) core: Arc<Core>,
    pub(crate) runner: Arc<FakeRunner>,
}

fn describe(fx: &mut Fixture, id: &str, name: &str, pitch: &str, description: &str) {
    let app = fx.catalog.apps.iter_mut().find(|a| a.id == id).unwrap();
    app.name = name.into();
    app.pitch = pitch.into();
    app.description = description.into();
    app.homepage = format!("https://amazynk.com/{id}");
    app.license = "GPL-3.0-or-later".into();
}

fn with_release_details(fx: &mut Fixture, id: &str) {
    let app = fx.catalog.apps.iter_mut().find(|a| a.id == id).unwrap();
    for r in &mut app.releases {
        r.date = Some(OffsetDateTime::now_utc() - time::Duration::days(9));
        r.notes_url = Some(format!("https://github.com/amazynk/{id}/releases/tag/v{}", r.version));
        for a in &mut r.assets {
            a.requires =
                Requirements { x86_64_v3: true, glibc: Some("2.35".into()), ..Requirements::default() };
        }
    }
}

/// The family on a fake server; `root` is the pretend computer.
pub(crate) fn family(root: &Path) -> Family {
    let mut fx = Fixture::new();
    let big = |app: &str, version: &str| {
        // Incompressible padding makes the downloads big enough to show progress.
        let mut exe = format!("#!/bin/sh\nexit 0\n# {app} {version}\n").into_bytes();
        let mut x = 0x2545_f491_4f6c_dd1d_u64;
        exe.extend((0..1_500_000).map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        }));
        suite_testkit::fake_app_with_exe(app, version, exe)
    };
    fx.add_release("specular", "1.0.0", AssetKind::TarGz, big("specular", "1.0.0"));
    fx.add_release("harbor", "2.4.0", AssetKind::TarGz, big("harbor", "2.4.0"));
    fx.add_release("bindery", "0.9.2", AssetKind::TarGz, big("bindery", "0.9.2"));
    fx.add_release("almanac", "0.3.0", AssetKind::TarGz, fake_app("almanac", "0.3.0"));
    fx.add_release("zynk", "3.4.1", AssetKind::TarGz, fake_app("zynk", "3.4.1"));
    fx.catalog.apps.push(suite_testkit::app_entry("foundry"));
    describe(
        &mut fx,
        "specular",
        "Specular",
        "Open and flip through large photos instantly",
        "A photo viewer that opens 100-megapixel images in a blink and keeps up while you flip through a folder. Colour-managed, with HDR on screens that have it.",
    );
    describe(
        &mut fx,
        "harbor",
        "Harbor",
        "Downloads that finish, fast and in order",
        "A download manager with many connections per file, a queue that respects your network, torrents and video pages, and progress you can trust.",
    );
    describe(
        &mut fx,
        "bindery",
        "bindery",
        "Pack and unpack archives at full speed",
        "Creates and opens zip, 7z, tar and more, using every core. Shows what's inside before you unpack.",
    );
    describe(
        &mut fx,
        "almanac",
        "Almanac",
        "Fast, private PDF viewer and editor",
        "Reads, annotates, fills in, signs and redacts PDF files on its own engine.",
    );
    describe(
        &mut fx,
        "foundry",
        "Foundry",
        "Find what you lost on your disks",
        "Finds lost files and big folders across all your disks, in seconds.",
    );
    describe(
        &mut fx,
        "zynk",
        "Zynk",
        "Your files on all your devices",
        "Keeps folders in step between your computers and phone, end to end encrypted.",
    );
    for id in ["specular", "harbor", "bindery", "zynk"] {
        with_release_details(&mut fx, id);
    }
    {
        let almanac = fx.catalog.apps.iter_mut().find(|a| a.id == "almanac").unwrap();
        almanac.releases[0].assets[0].requires.glibc = Some("2.45".into());
        almanac.releases[0].assets[0].requires.x86_64_v3 = true;
    }
    {
        let foundry = fx.catalog.apps.iter_mut().find(|a| a.id == "foundry").unwrap();
        foundry.status = AppStatus::Unreleased;
    }
    {
        let zynk = fx.catalog.apps.iter_mut().find(|a| a.id == "zynk").unwrap();
        zynk.vendor = Some("Zynk".into());
        zynk.ids.flatpak = Some("it.zynk.Zynk".into());
        zynk.mobile = Mobile {
            android: Some("https://play.google.com/store/apps/details?id=it.zynk".into()),
            ios: Some("https://apps.apple.com/app/zynk/id1543210987".into()),
        };
        // Pinned from the vendor's "latest" link, which has since moved on.
        zynk.releases[0].assets[0].pinned = true;
    }
    let mut moved = fake_app("zynk", "3.4.1");
    let last = moved.len() - 1;
    moved[last] ^= 0xFF;
    fx.server.put("/assets/zynk-3.4.1.tar.gz", moved);
    fx.publish();

    let runner = Arc::new(FakeRunner::new());
    runner.on(&[], Output::ok(""));
    runner.on(&["flatpak"], Output::failed(1, "not installed"));
    runner.on(
        &["flatpak", "info", "--user", "it.zynk.Zynk"],
        Output::ok("Ref: app/it.zynk.Zynk\nVersion: 3.3.0\n"),
    );
    let env = Env {
        os: Os::Linux,
        paths: Paths::rooted(root, Os::Linux),
        runner: Arc::clone(&runner) as Arc<dyn suite_platform::Runner>,
        registry: Arc::new(MemRegistry::new()),
        processes: Arc::new(FakeProcesses::new()),
        network: Arc::new(FixedNetwork(Metered::No)),
        sandboxed: true,
    };
    // `flatpak` is on this pretend computer.
    let flatpak = env.paths.sys("usr/bin/flatpak");
    std::fs::create_dir_all(flatpak.parent().unwrap()).unwrap();
    std::fs::write(&flatpak, "").unwrap();
    let fetcher = Fetcher::new(Options { backoff: Duration::from_millis(5), ..Options::default() });
    let core = Core::new(env, fetcher, CatalogSource::Url(fx.catalog_url()))
        .with_facts(suite_testkit::facts(Os::Linux));

    // Specular 1.0 then 1.1 (so it can go back), Harbor 2.4; then 1.2 and a Harbor beta appear.
    let catalog = core.load_catalog().unwrap();
    core.install(&catalog, "specular", None, &mut |_| {}).unwrap();
    core.install(&catalog, "harbor", None, &mut |_| {}).unwrap();
    core.set_policy("harbor", Policy::Ask).unwrap();
    fx.add_release("specular", "1.1.0", AssetKind::TarGz, big("specular", "1.1.0"));
    fx.publish();
    let catalog = core.load_catalog().unwrap();
    core.update(&catalog, "specular", &mut |_| {}).unwrap();
    fx.add_release("specular", "1.2.0", AssetKind::TarGz, big("specular", "1.2.0"));
    fx.add_release_on(
        "harbor",
        "2.5.0-beta.1",
        Channel::Beta,
        AssetKind::TarGz,
        big("harbor", "2.5.0-beta.1"),
    );
    with_release_details(&mut fx, "specular");
    with_release_details(&mut fx, "harbor");
    fx.publish();
    Family { fixture: fx, core: Arc::new(core), runner }
}
