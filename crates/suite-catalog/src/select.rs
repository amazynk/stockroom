//! Choosing what to install: channel, release order, requirement matching and the best
//! asset for this computer.

use std::fmt;

use crate::{
    host::{Arch, DottedVersion, HostFacts, Os},
    model::{App, AppStatus, Asset, AssetKind, Channel, Release, Requirements},
};

/// Why an asset or app can't be used here, in plain words for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unmet {
    WrongOs { needs: Os },
    WrongArch { needs: Arch },
    NoX86_64V3,
    Glibc { needs: String, have: Option<DottedVersion> },
    Macos { needs: String, have: Option<DottedVersion> },
    WindowsBuild { needs: u32, have: Option<u32> },
    KindUnsupported(AssetKind),
    NoFlatpak,
    BadRequirement(String),
}

impl fmt::Display for Unmet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongOs { needs } => write!(f, "made for {}", needs.name()),
            Self::WrongArch { needs } => write!(f, "made for {} processors", needs.name()),
            Self::NoX86_64V3 => f.write_str("needs a processor with AVX2 (x86-64-v3, 2013 or later)"),
            Self::Glibc { needs, have: Some(h) } => {
                write!(f, "needs glibc {needs} or newer (this system has {h})")
            }
            Self::Glibc { needs, have: None } => write!(f, "needs glibc {needs} or newer (none found)"),
            Self::Macos { needs, have: Some(h) } => {
                write!(f, "needs macOS {needs} or newer (this Mac has {h})")
            }
            Self::Macos { needs, have: None } => write!(f, "needs macOS {needs} or newer"),
            Self::WindowsBuild { needs, have: Some(h) } => {
                write!(f, "needs Windows build {needs} or newer (this PC has {h})")
            }
            Self::WindowsBuild { needs, have: None } => write!(f, "needs Windows build {needs} or newer"),
            Self::KindUnsupported(k) => write!(f, "{k} packages are not supported here yet"),
            Self::NoFlatpak => f.write_str("needs Flatpak"),
            Self::BadRequirement(s) => {
                write!(f, "has a requirement this version of Stockroom can't read: {s}")
            }
        }
    }
}

/// Checks `req` against `host`; empty means it runs.
pub fn unmet_requirements(req: &Requirements, host: &HostFacts) -> Vec<Unmet> {
    let mut out = Vec::new();
    if req.x86_64_v3 && !host.x86_64_v3 {
        out.push(Unmet::NoX86_64V3);
    }
    if let Some(needs) = &req.glibc {
        match needs.parse::<DottedVersion>() {
            Ok(n) if host.glibc.as_ref().is_some_and(|h| *h >= n) => {}
            Ok(_) => out.push(Unmet::Glibc { needs: needs.clone(), have: host.glibc.clone() }),
            Err(_) => out.push(Unmet::BadRequirement(format!("glibc {needs}"))),
        }
    }
    if let Some(needs) = &req.macos {
        match needs.parse::<DottedVersion>() {
            Ok(n) if host.macos.as_ref().is_some_and(|h| *h >= n) => {}
            Ok(_) => out.push(Unmet::Macos { needs: needs.clone(), have: host.macos.clone() }),
            Err(_) => out.push(Unmet::BadRequirement(format!("macOS {needs}"))),
        }
    }
    if let Some(needs) = req.windows_build
        && host.windows_build.is_none_or(|h| h < needs)
    {
        out.push(Unmet::WindowsBuild { needs, have: host.windows_build });
    }
    out
}

/// Preferences that change which asset is chosen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Prefs {
    /// Prefer a Flatpak bundle over a tarball (Linux).
    pub prefer_flatpak: bool,
}

/// Asset kinds this version of Stockroom can install on `os`, best first.
pub fn supported_kinds(os: Os, prefs: Prefs) -> &'static [AssetKind] {
    use AssetKind::{AppImage, Dmg, FlatpakBundle, Store, TarGz, Zip};
    match os {
        // A store or the app's own website comes last everywhere: a copy Stockroom manages is
        // preferred when there is one. (On Linux that is an app that sets itself up from its
        // page, like Dozed; there is no store.)
        Os::Linux if prefs.prefer_flatpak => &[FlatpakBundle, TarGz, AppImage, Store],
        Os::Linux => &[TarGz, AppImage, FlatpakBundle, Store],
        // MSI needs a per-machine install (admin); the portable zip doesn't.
        Os::Windows => &[Zip, Store],
        Os::Macos => &[Dmg, Zip, Store],
    }
}

fn arch_fits(asset: Arch, host: &HostFacts) -> bool {
    asset == host.arch || (asset == Arch::Universal && host.os == Os::Macos)
}

/// Why `asset` can't be installed here (empty: it can).
pub fn asset_unmet(asset: &Asset, host: &HostFacts, prefs: Prefs) -> Vec<Unmet> {
    let mut out = Vec::new();
    if asset.os != host.os {
        out.push(Unmet::WrongOs { needs: asset.os });
        return out;
    }
    // A store serves the right build for the machine itself.
    if asset.kind != AssetKind::Store && !arch_fits(asset.arch, host) {
        out.push(Unmet::WrongArch { needs: asset.arch });
    }
    if !supported_kinds(host.os, prefs).contains(&asset.kind) {
        out.push(Unmet::KindUnsupported(asset.kind));
    }
    if asset.kind == AssetKind::FlatpakBundle && !host.flatpak {
        out.push(Unmet::NoFlatpak);
    }
    // A Flatpak brings its own runtime: glibc and the host libraries don't matter.
    let mut req = asset.requires.clone();
    if asset.kind == AssetKind::FlatpakBundle {
        req.glibc = None;
    }
    out.extend(unmet_requirements(&req, host));
    out
}

/// The best asset of `release` for this computer, or every reason none fits.
pub fn choose_asset<'a>(
    release: &'a Release,
    host: &HostFacts,
    prefs: Prefs,
) -> Result<&'a Asset, Vec<Unmet>> {
    let kinds = supported_kinds(host.os, prefs);
    let mut usable: Vec<&Asset> =
        release.assets.iter().filter(|a| asset_unmet(a, host, prefs).is_empty()).collect();
    // Preferred kind first, then the exact architecture over universal.
    usable.sort_by_key(|a| (kinds.iter().position(|k| *k == a.kind), a.arch == Arch::Universal));
    if let Some(best) = usable.first() {
        return Ok(best);
    }
    let mut reasons: Vec<Unmet> = Vec::new();
    for a in &release.assets {
        for r in asset_unmet(a, host, prefs) {
            if !reasons.contains(&r) {
                reasons.push(r);
            }
        }
    }
    // An OS mismatch is noise when another asset matched the OS but failed for a real reason.
    if reasons.iter().any(|r| !matches!(r, Unmet::WrongOs { .. })) {
        reasons.retain(|r| !matches!(r, Unmet::WrongOs { .. }));
    }
    Err(reasons)
}

/// Whether `release` is offered on `channel`. Stable sees stable releases without a
/// pre-release tag; beta sees everything.
pub fn on_channel(release: &Release, channel: Channel) -> bool {
    match channel {
        Channel::Beta => true,
        Channel::Stable => release.channel == Channel::Stable && release.version.pre.is_empty(),
    }
}

/// Releases on `channel`, newest first (semver order).
pub fn releases_on(app: &App, channel: Channel) -> Vec<&Release> {
    let mut v: Vec<&Release> = app.releases.iter().filter(|r| on_channel(r, channel)).collect();
    v.sort_by(|a, b| b.version.cmp(&a.version));
    v
}

/// Why an app can't be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotInstallable {
    ComingSoon,
    NoRelease { channel: Channel },
    Unsupported(Vec<Unmet>),
}

impl fmt::Display for NotInstallable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ComingSoon => f.write_str("coming soon"),
            Self::NoRelease { channel } => write!(f, "no {channel} release yet"),
            Self::Unsupported(r) => {
                f.write_str("can't run on this computer: ")?;
                for (i, u) in r.iter().enumerate() {
                    if i > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "{u}")?;
                }
                Ok(())
            }
        }
    }
}

/// The newest release on `channel` that has an asset for this computer, and that asset.
/// Unmet reasons are those of the newest release.
pub fn best_installable<'a>(
    app: &'a App,
    channel: Channel,
    host: &HostFacts,
    prefs: Prefs,
) -> Result<(&'a Release, &'a Asset), NotInstallable> {
    if app.status == AppStatus::Unreleased {
        return Err(NotInstallable::ComingSoon);
    }
    let releases = releases_on(app, channel);
    let mut first_reasons = None;
    for r in &releases {
        match choose_asset(r, host, prefs) {
            Ok(a) => return Ok((r, a)),
            Err(reasons) => {
                first_reasons.get_or_insert(reasons);
            }
        }
    }
    match first_reasons {
        Some(r) => Err(NotInstallable::Unsupported(r)),
        None => Err(NotInstallable::NoRelease { channel }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Layout, Sha256Hex};

    fn asset(os: Os, arch: Arch, kind: AssetKind, requires: Requirements) -> Asset {
        Asset {
            os,
            arch,
            kind,
            url: format!("https://example.com/{os}-{arch}-{kind}"),
            size: 1,
            sha256: Some(Sha256Hex::from_digest(&[0; 32])),
            requires,
            layout: Layout::default(),
            store: None,
            pinned: false,
            note: None,
        }
    }

    fn release(v: &str, channel: Channel, assets: Vec<Asset>) -> Release {
        Release { version: v.parse().unwrap(), channel, date: None, notes_url: None, assets }
    }

    fn specular_like() -> Vec<Asset> {
        let req = Requirements { x86_64_v3: true, glibc: Some("2.35".into()), ..Default::default() };
        vec![
            asset(Os::Linux, Arch::X86_64, AssetKind::AppImage, req.clone()),
            asset(Os::Linux, Arch::X86_64, AssetKind::FlatpakBundle, req.clone()),
            asset(Os::Linux, Arch::X86_64, AssetKind::TarGz, req),
            asset(Os::Windows, Arch::X86_64, AssetKind::Msi, Requirements::default()),
            asset(Os::Windows, Arch::X86_64, AssetKind::Zip, Requirements::default()),
            asset(Os::Macos, Arch::Universal, AssetKind::Dmg, Requirements::default()),
        ]
    }

    #[test]
    fn dotted_versions_order_numerically() {
        let v = |s: &str| s.parse::<DottedVersion>().unwrap();
        assert!(v("2.35") > v("2.4"));
        assert_eq!(v("2.35"), v("2.35.0"));
        assert!(v("14.6.1") > v("14.6"));
        assert!("2.x".parse::<DottedVersion>().is_err());
        assert!("".parse::<DottedVersion>().is_err());
    }

    #[test]
    fn requirements() {
        let mut host = HostFacts::example(Os::Linux);
        let req = Requirements { x86_64_v3: true, glibc: Some("2.35".into()), ..Default::default() };
        assert_eq!(unmet_requirements(&req, &host), []);
        host.glibc = Some(DottedVersion::new([2, 31]));
        host.x86_64_v3 = false;
        let unmet = unmet_requirements(&req, &host);
        assert_eq!(unmet.len(), 2);
        assert!(unmet[1].to_string().contains("2.31"));
        host.glibc = None; // musl
        assert!(matches!(unmet_requirements(&req, &host)[1], Unmet::Glibc { have: None, .. }));

        let win = HostFacts::example(Os::Windows);
        let req = Requirements { windows_build: Some(26100), ..Default::default() };
        assert_eq!(
            unmet_requirements(&req, &win),
            vec![Unmet::WindowsBuild { needs: 26100, have: Some(22631) }]
        );
        let mac = HostFacts::example(Os::Macos);
        let req = Requirements { macos: Some("15".into()), ..Default::default() };
        assert_eq!(unmet_requirements(&req, &mac).len(), 1);
        let req = Requirements { macos: Some("12.0".into()), ..Default::default() };
        assert_eq!(unmet_requirements(&req, &mac), []);
    }

    #[test]
    fn picks_best_asset_per_os() {
        let r = release("1.0.0", Channel::Stable, specular_like());
        let linux = HostFacts::example(Os::Linux);
        assert_eq!(choose_asset(&r, &linux, Prefs::default()).unwrap().kind, AssetKind::TarGz);
        let flat = Prefs { prefer_flatpak: true };
        assert_eq!(choose_asset(&r, &linux, flat).unwrap().kind, AssetKind::FlatpakBundle);
        let win = HostFacts::example(Os::Windows);
        assert_eq!(choose_asset(&r, &win, Prefs::default()).unwrap().kind, AssetKind::Zip);
        let mut mac = HostFacts::example(Os::Macos);
        assert_eq!(choose_asset(&r, &mac, Prefs::default()).unwrap().kind, AssetKind::Dmg);
        mac.arch = Arch::Aarch64;
        assert_eq!(choose_asset(&r, &mac, Prefs::default()).unwrap().arch, Arch::Universal);
    }

    #[test]
    fn store_assets_are_chosen_only_without_a_download() {
        let mut store = asset(Os::Windows, Arch::X86_64, AssetKind::Store, Requirements::default());
        store.store = Some(crate::model::StoreLink {
            store: crate::model::Store::Microsoft,
            product_id: "9PP9CNT87P3F".into(),
            url: "https://apps.microsoft.com/store/detail/zynk/9PP9CNT87P3F".into(),
            link: "ms-windows-store://pdp/?productid=9PP9CNT87P3F".into(),
        });
        let mut win = HostFacts::example(Os::Windows);
        win.arch = Arch::Aarch64;
        let r = release("1.0.0", Channel::Stable, vec![store.clone()]);
        assert_eq!(choose_asset(&r, &win, Prefs::default()).unwrap().kind, AssetKind::Store);
        let mut assets = specular_like();
        assets.push(store);
        let r = release("1.0.0", Channel::Stable, assets);
        assert_eq!(
            choose_asset(&r, &HostFacts::example(Os::Windows), Prefs::default()).unwrap().kind,
            AssetKind::Zip
        );
    }

    #[test]
    fn old_glibc_falls_back_to_flatpak_then_explains() {
        let r = release("1.0.0", Channel::Stable, specular_like());
        let mut host = HostFacts::example(Os::Linux);
        host.glibc = Some(DottedVersion::new([2, 31]));
        assert_eq!(choose_asset(&r, &host, Prefs::default()).unwrap().kind, AssetKind::FlatpakBundle);
        host.flatpak = false;
        let reasons = choose_asset(&r, &host, Prefs::default()).unwrap_err();
        assert!(reasons.iter().any(|u| matches!(u, Unmet::Glibc { .. })));
        assert!(reasons.contains(&Unmet::NoFlatpak));
        assert!(!reasons.iter().any(|u| matches!(u, Unmet::WrongOs { .. })));
        host.arch = Arch::Aarch64;
        let reasons = choose_asset(&r, &host, Prefs::default()).unwrap_err();
        assert!(reasons.contains(&Unmet::WrongArch { needs: Arch::X86_64 }));
        // The command line prints these as they are: names people know.
        assert_eq!(Unmet::WrongOs { needs: Os::Linux }.to_string(), "made for Linux");
        assert_eq!(Unmet::WrongOs { needs: Os::Macos }.to_string(), "made for macOS");
        assert_eq!(Unmet::WrongArch { needs: Arch::X86_64 }.to_string(), "made for x86-64 processors");
    }

    #[test]
    fn channels_and_semver_order() {
        let assets = || vec![asset(Os::Linux, Arch::X86_64, AssetKind::TarGz, Requirements::default())];
        let app = App {
            id: "demo".into(),
            name: "Demo".into(),
            pitch: String::new(),
            description: String::new(),
            icon: None,
            homepage: "https://example.com".into(),
            license: "MIT".into(),
            category: String::new(),
            status: AppStatus::Available,
            ids: crate::model::PlatformIds::default(),
            data_paths: crate::model::DataPaths::default(),
            vendor: None,
            part: None,
            mobile: crate::model::Mobile::default(),
            extras: Vec::new(),
            releases: vec![
                release("1.9.0", Channel::Stable, assets()),
                release("1.10.0", Channel::Stable, assets()),
                release("2.0.0-beta.1", Channel::Beta, assets()),
                release("1.11.0-rc.1", Channel::Stable, assets()),
            ],
        };
        let stable: Vec<String> =
            releases_on(&app, Channel::Stable).iter().map(|r| r.version.to_string()).collect();
        assert_eq!(stable, ["1.10.0", "1.9.0"]);
        let beta: Vec<String> =
            releases_on(&app, Channel::Beta).iter().map(|r| r.version.to_string()).collect();
        assert_eq!(beta, ["2.0.0-beta.1", "1.11.0-rc.1", "1.10.0", "1.9.0"]);
        let host = HostFacts::example(Os::Linux);
        let (r, _) = best_installable(&app, Channel::Stable, &host, Prefs::default()).unwrap();
        assert_eq!(r.version.to_string(), "1.10.0");
        let win = HostFacts::example(Os::Windows);
        assert!(matches!(
            best_installable(&app, Channel::Stable, &win, Prefs::default()),
            Err(NotInstallable::Unsupported(_))
        ));
    }

    #[test]
    fn newer_release_needing_more_falls_back_to_older() {
        let new_req = Requirements { glibc: Some("2.40".into()), ..Default::default() };
        let mut app_releases = vec![
            release(
                "1.0.0",
                Channel::Stable,
                vec![asset(Os::Linux, Arch::X86_64, AssetKind::TarGz, Requirements::default())],
            ),
            release(
                "2.0.0",
                Channel::Stable,
                vec![asset(Os::Linux, Arch::X86_64, AssetKind::TarGz, new_req)],
            ),
        ];
        app_releases.reverse();
        let mut host = HostFacts::example(Os::Linux);
        host.flatpak = false;
        let app = App {
            id: "demo".into(),
            name: "Demo".into(),
            pitch: String::new(),
            description: String::new(),
            icon: None,
            homepage: "https://example.com".into(),
            license: "MIT".into(),
            category: String::new(),
            status: AppStatus::Available,
            ids: crate::model::PlatformIds::default(),
            data_paths: crate::model::DataPaths::default(),
            vendor: None,
            part: None,
            mobile: crate::model::Mobile::default(),
            extras: Vec::new(),
            releases: app_releases,
        };
        let (r, _) = best_installable(&app, Channel::Stable, &host, Prefs::default()).unwrap();
        assert_eq!(r.version.to_string(), "1.0.0");
        let mut unreleased = app.clone();
        unreleased.status = AppStatus::Unreleased;
        assert_eq!(
            best_installable(&unreleased, Channel::Stable, &host, Prefs::default()).unwrap_err(),
            NotInstallable::ComingSoon
        );
        let mut empty = app;
        empty.releases.clear();
        assert_eq!(
            best_installable(&empty, Channel::Beta, &host, Prefs::default()).unwrap_err().to_string(),
            "no beta release yet"
        );
    }
}
