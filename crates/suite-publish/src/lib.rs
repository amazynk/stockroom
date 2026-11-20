//! Building the catalog (PLAN.md §3): app metadata and icons from `catalog/apps`, releases
//! and assets from GitHub, every asset checked against its `.sha256` sibling by downloading
//! it.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use suite_catalog::{
    App, AppStatus, Arch, Asset, AssetKind, Catalog, Channel, DataPaths, Extra, Icon, Layout, Mobile, Os,
    PlatformIds, Release, Requirements, SCHEMA, Sha256Hex, StoreLink,
};
use suite_fetch::Fetcher;
use time::OffsetDateTime;

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("{path}: {message}")]
    Meta { path: PathBuf, message: String },
    #[error(transparent)]
    Fetch(#[from] suite_fetch::FetchError),
    #[error("GitHub answered something unexpected: {0}")]
    GitHub(String),
    #[error("{asset}: checksum mismatch (.sha256 says {listed}, the file is {actual})")]
    Checksum { asset: String, listed: String, actual: Sha256Hex },
    #[error("{asset}: size mismatch (GitHub says {listed}, downloaded {actual})")]
    Size { asset: String, listed: u64, actual: u64 },
    #[error(transparent)]
    Catalog(#[from] suite_catalog::CatalogError),
    #[error("pinning: {0}")]
    Pinned(String),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
}

/// One `catalog/apps/<id>.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppMeta {
    pub id: String,
    pub name: String,
    pub pitch: String,
    #[serde(default)]
    pub description: String,
    pub homepage: String,
    pub license: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub status: AppStatus,
    /// `owner/repo` on GitHub.
    #[serde(default)]
    pub github: Option<String>,
    /// The app's icon: a PNG file next to this one (`specular.png`), square, drawn to its
    /// edges. It is published beside the catalog under a name that carries its checksum.
    #[serde(default)]
    pub icon: Option<String>,
    /// The icon file's contents, read by [`read_apps`].
    #[serde(skip)]
    pub icon_png: Option<Vec<u8>>,
    /// Its number in the amazynk catalogue (`AMZ-01`).
    #[serde(default)]
    pub part: Option<String>,
    /// Place among the family's apps (lowest first); apps without one follow in file order.
    #[serde(default)]
    pub order: Option<u32>,
    #[serde(default)]
    pub ids: PlatformIds,
    #[serde(default)]
    pub data_paths: DataPaths,
    #[serde(default)]
    pub assets: Vec<AssetRule>,
    /// Maker, for apps that aren't the family's; listed after the family.
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub mobile: Mobile,
    #[serde(default)]
    pub extras: Vec<ExtraMeta>,
}

/// An optional add-on; its assets are pinned from `latest` URLs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtraMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub assets: Vec<AssetRule>,
}

/// One asset per release, found one of three ways:
/// - `name`: a GitHub release asset, with `{version}` in the name and a `.sha256` next to it;
/// - `latest`: a vendor URL that always serves the newest build, without a checksum. It is
///   downloaded and pinned (final URL, size, SHA-256) at publish time; `name` then is the
///   expected file name, with `{version}` or `{build}` as wildcards;
/// - `store`: an app-store listing; nothing is downloaded.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub latest: Option<String>,
    #[serde(default)]
    pub store: Option<StoreLink>,
    /// Shown with the asset (pinned and store assets get one automatically).
    #[serde(default)]
    pub note: Option<String>,
    pub os: Os,
    pub arch: Arch,
    pub kind: AssetKind,
    #[serde(default)]
    pub requires: Requirements,
    #[serde(default)]
    pub layout: Layout,
}

/// Reads every `*.toml` in `dir`, sorted by file name.
pub fn read_apps(dir: &Path) -> Result<Vec<AppMeta>, PublishError> {
    let io = |e| PublishError::Io { path: dir.to_owned(), source: e };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(io)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    files
        .iter()
        .map(|p| {
            let text =
                std::fs::read_to_string(p).map_err(|e| PublishError::Io { path: p.clone(), source: e })?;
            let bad = |message: String| PublishError::Meta { path: p.clone(), message };
            let mut meta: AppMeta = toml::from_str(&text).map_err(|e| bad(e.to_string()))?;
            suite_catalog::model::validate_app_id(&meta.id).map_err(|e| bad(e.to_string()))?;
            if let Some(name) = &meta.icon {
                let png = Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
                if name.contains(['/', '\\']) || !png {
                    return Err(bad(format!("icon {name:?} must be a .png file next to this one")));
                }
                let file = dir.join(name);
                let png =
                    std::fs::read(&file).map_err(|e| PublishError::Io { path: file.clone(), source: e })?;
                if !png.starts_with(PNG_SIGNATURE) || png.len() as u64 > MAX_ICON {
                    return Err(bad(format!("{name} must be a PNG of at most {} KB", MAX_ICON / 1024)));
                }
                meta.icon_png = Some(png);
            }
            Ok(meta)
        })
        .collect()
}

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
/// Icons are small: Stockroom refuses to download larger ones.
const MAX_ICON: u64 = 256 * 1024;

/// The published file name of an app's icon. It carries the start of the checksum, so a
/// changed icon gets a new address and no cache serves the old one for it.
fn icon_file_name(id: &str, sha256: &Sha256Hex) -> String {
    format!("{id}-{}.png", &sha256.as_str()[..12])
}

fn sha256_of(bytes: &[u8]) -> Sha256Hex {
    Sha256Hex::from_digest(&<sha2::Sha256 as sha2::Digest>::digest(bytes))
}

/// The icon files to publish beside the catalog (in `icons/`): name and contents.
pub fn icon_files(apps: &[AppMeta]) -> Vec<(String, &[u8])> {
    apps.iter()
        .filter_map(|a| {
            let png = a.icon_png.as_deref()?;
            Some((icon_file_name(&a.id, &sha256_of(png)), png))
        })
        .collect()
}

/// Where the icons are served from by default: `icons/` next to the catalog.
pub fn default_icon_base() -> String {
    let dir = suite_identity::CATALOG_URL.rsplit_once('/').map_or(suite_identity::CATALOG_URL, |(d, _)| d);
    format!("{dir}/icons")
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    published_at: Option<OffsetDateTime>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// GitHub API base, e.g. `https://api.github.com` (tests use the fake server).
    pub api: String,
    /// Newest releases to include per app.
    pub releases: usize,
    /// Catalog serial; default: the publish time in seconds (always increasing).
    pub serial: Option<u64>,
    /// The address the icons are published at; an icon is `<base>/<id>-<checksum>.png`.
    pub icon_base: String,
    /// The catalog published last time. A release asset that is listed there with the same
    /// address, size and `.sha256` is taken from it instead of being downloaded again.
    pub reuse: Option<Catalog>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            api: "https://api.github.com".into(),
            releases: 3,
            serial: None,
            icon_base: default_icon_base(),
            reuse: None,
        }
    }
}

impl Options {
    /// Whether the last catalog already vouches for these exact bytes.
    fn checked_before(&self, url: &str, size: u64, listed: &Sha256Hex) -> bool {
        let Some(prev) = &self.reuse else { return false };
        prev.apps
            .iter()
            .flat_map(|a| a.releases.iter().flat_map(|r| &r.assets))
            .any(|a| !a.pinned && a.url == url && a.size == size && a.sha256.as_ref() == Some(listed))
    }
}

/// Parses a `.sha256` file: the first token is the hex digest (`sha256sum` format).
pub fn parse_sha256_file(text: &str) -> Option<Sha256Hex> {
    text.split_whitespace().next()?.parse().ok()
}

/// Builds the catalog: metadata plus verified GitHub releases.
pub fn build(
    apps: &[AppMeta],
    fetcher: &Fetcher,
    opts: &Options,
    progress: &mut dyn FnMut(&str),
) -> Result<Catalog, PublishError> {
    let now = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap_or_else(|_| OffsetDateTime::now_utc());
    let mut out = Vec::new();
    // The family first, its released apps before those to come, then other makers' apps;
    // within each group by `order`, then file order (the sort is stable).
    let mut apps: Vec<&AppMeta> = apps.iter().collect();
    apps.sort_by_key(|a| (a.vendor.is_some(), a.status != AppStatus::Available, a.order.unwrap_or(u32::MAX)));
    for meta in apps {
        let available = meta.status == AppStatus::Available;
        let releases = match &meta.github {
            Some(repo) if available => releases_for(meta, repo, fetcher, opts, progress)?,
            None if available => pinned_release(meta, fetcher, now, progress)?,
            _ => Vec::new(),
        };
        let mut extras = Vec::new();
        if available {
            for x in &meta.extras {
                let (assets, names) = pin_rules(meta, &x.assets, fetcher, now, progress)?;
                if assets.is_empty() {
                    continue;
                }
                extras.push(Extra {
                    id: x.id.clone(),
                    name: x.name.clone(),
                    description: x.description.clone(),
                    version: Some(version_from_names(&names, now, &assets)),
                    assets,
                });
            }
        }
        out.push(App {
            id: meta.id.clone(),
            name: meta.name.clone(),
            pitch: meta.pitch.clone(),
            description: meta.description.clone(),
            icon: meta.icon_png.as_deref().map(|png| {
                let sha256 = sha256_of(png);
                let url =
                    format!("{}/{}", opts.icon_base.trim_end_matches('/'), icon_file_name(&meta.id, &sha256));
                Icon { url, sha256 }
            }),
            homepage: meta.homepage.clone(),
            license: meta.license.clone(),
            category: meta.category.clone(),
            status: meta.status,
            ids: meta.ids.clone(),
            data_paths: meta.data_paths.clone(),
            releases,
            vendor: meta.vendor.clone(),
            part: meta.part.clone(),
            mobile: meta.mobile.clone(),
            extras,
        });
    }
    let serial = opts.serial.unwrap_or_else(|| u64::try_from(now.unix_timestamp()).unwrap_or(0));
    let catalog = Catalog { schema: SCHEMA, version: serial, published: now, apps: out };
    catalog.validate()?;
    Ok(catalog)
}

fn releases_for(
    meta: &AppMeta,
    repo: &str,
    fetcher: &Fetcher,
    opts: &Options,
    progress: &mut dyn FnMut(&str),
) -> Result<Vec<Release>, PublishError> {
    let mut gh: Vec<GhRelease> = Vec::new();
    for page in 1.. {
        let url =
            format!("{}/repos/{repo}/releases?per_page=100&page={page}", opts.api.trim_end_matches('/'));
        let bytes = fetcher.get_bytes(&url, 32 << 20)?;
        let batch: Vec<GhRelease> =
            serde_json::from_slice(&bytes).map_err(|e| PublishError::GitHub(format!("{url}: {e}")))?;
        let n = batch.len();
        gh.extend(batch);
        if n < 100 {
            break;
        }
    }
    let mut parsed: Vec<(semver::Version, GhRelease)> = gh
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| r.tag_name.trim_start_matches('v').parse().ok().map(|v| (v, r)))
        .collect();
    parsed.sort_by(|a, b| b.0.cmp(&a.0));
    parsed.truncate(opts.releases);

    let mut out = Vec::new();
    for (version, r) in parsed {
        let by_name: BTreeMap<&str, &GhAsset> = r.assets.iter().map(|a| (a.name.as_str(), a)).collect();
        let mut assets = Vec::new();
        for rule in &meta.assets {
            if let Some(store) = &rule.store {
                assets.push(store_asset(rule, store));
                continue;
            }
            let Some(pattern) = &rule.name else { continue };
            let name = pattern.replace("{version}", &version.to_string());
            let Some(a) = by_name.get(name.as_str()) else {
                tracing::warn!(app = %meta.id, %version, asset = %name, "not in this release; skipped");
                continue;
            };
            let sum_name = format!("{name}.sha256");
            let Some(sum) = by_name.get(sum_name.as_str()) else {
                tracing::warn!(app = %meta.id, %version, asset = %name, "no .sha256 published; skipped");
                continue;
            };
            let listed_text = fetcher.get_bytes(&sum.browser_download_url, 4096)?;
            let listed = parse_sha256_file(&String::from_utf8_lossy(&listed_text))
                .ok_or_else(|| PublishError::GitHub(format!("{sum_name}: no digest in it")))?;
            let (size, actual) = if opts.checked_before(&a.browser_download_url, a.size, &listed) {
                progress(&format!("{} {version}: {name} is as checked last time", meta.id));
                (a.size, listed.clone())
            } else {
                progress(&format!("{} {version}: checking {name} ({} MB)", meta.id, a.size / 1_000_000));
                fetcher.hash_url(&a.browser_download_url, &mut |_| {})?
            };
            if actual != listed {
                return Err(PublishError::Checksum { asset: name, listed: listed.to_string(), actual });
            }
            if size != a.size {
                return Err(PublishError::Size { asset: name, listed: a.size, actual: size });
            }
            assets.push(Asset {
                os: rule.os,
                arch: rule.arch,
                kind: rule.kind,
                url: a.browser_download_url.clone(),
                size,
                sha256: Some(actual),
                requires: rule.requires.clone(),
                layout: rule.layout.clone(),
                store: None,
                pinned: false,
                note: rule.note.clone(),
            });
        }
        if assets.is_empty() {
            continue;
        }
        let channel = if r.prerelease || !version.pre.is_empty() { Channel::Beta } else { Channel::Stable };
        out.push(Release { version, channel, date: r.published_at, notes_url: r.html_url, assets });
    }
    Ok(out)
}

fn store_asset(rule: &AssetRule, store: &StoreLink) -> Asset {
    Asset {
        os: rule.os,
        arch: rule.arch,
        kind: AssetKind::Store,
        url: store.url.clone(),
        size: 0,
        sha256: None,
        requires: rule.requires.clone(),
        layout: Layout::default(),
        store: Some(store.clone()),
        pinned: false,
        note: rule.note.clone().or_else(|| {
            Some(match store.store {
                suite_catalog::Store::Website => "Installed from its website".to_owned(),
                s => format!("Installed and updated by the {}", s.name()),
            })
        }),
    }
}

/// Downloads every `latest` rule and pins it; store rules pass through. Returns the assets
/// and the downloaded file names (for the version).
fn pin_rules(
    meta: &AppMeta,
    rules: &[AssetRule],
    fetcher: &Fetcher,
    now: OffsetDateTime,
    progress: &mut dyn FnMut(&str),
) -> Result<(Vec<Asset>, Vec<String>), PublishError> {
    let maker = meta.vendor.as_deref().unwrap_or(&meta.name);
    let mut assets = Vec::new();
    let mut names = Vec::new();
    for rule in rules {
        if let Some(store) = &rule.store {
            assets.push(store_asset(rule, store));
            continue;
        }
        let Some(latest) = &rule.latest else { continue };
        progress(&format!("{}: pinning {latest}", meta.id));
        let remote = fetcher.inspect(latest, &mut |_| {})?;
        let file = remote.file_name.clone().unwrap_or_default();
        if let Some(pattern) = &rule.name
            && !matches_pattern(pattern, &file)
        {
            return Err(PublishError::Pinned(format!("{latest} served {file:?}, expected {pattern:?}")));
        }
        names.push(file.clone());
        let date = now.date();
        assets.push(Asset {
            os: rule.os,
            arch: rule.arch,
            kind: rule.kind,
            url: remote.final_url,
            size: remote.size,
            sha256: Some(remote.sha256),
            requires: rule.requires.clone(),
            layout: rule.layout.clone(),
            store: None,
            pinned: true,
            note: Some(rule.note.clone().unwrap_or_else(|| {
                format!(
                    "{maker} publishes no checksum; size and SHA-256 of {file} pinned from {latest} on {date}"
                )
            })),
        });
    }
    Ok((assets, names))
}

/// `pattern` with `{version}` / `{build}` as wildcards.
fn matches_pattern(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> =
        pattern.split(['{', '}']).enumerate().filter(|(i, _)| i % 2 == 0).map(|(_, p)| p).collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            let Some(r) = rest.strip_prefix(p) else { return false };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            let Some(pos) = rest.find(p) else { return false };
            rest = &rest[pos + p.len()..];
        }
    }
    false
}

/// One release for an app whose assets come from `latest` URLs and stores.
fn pinned_release(
    meta: &AppMeta,
    fetcher: &Fetcher,
    now: OffsetDateTime,
    progress: &mut dyn FnMut(&str),
) -> Result<Vec<Release>, PublishError> {
    let (assets, names) = pin_rules(meta, &meta.assets, fetcher, now, progress)?;
    if assets.is_empty() {
        return Ok(Vec::new());
    }
    let version = version_from_names(&names, now, &assets);
    Ok(vec![Release { version, channel: Channel::Stable, date: Some(now), notes_url: None, assets }])
}

/// A version for pinned downloads: one found in a file name (`app-v1.6.0.tgz`), else the
/// pin date with the vendor's build hash (`2026.11.15+build.091ee20dff0c`), so later pins
/// sort after earlier ones.
pub fn version_from_names(names: &[String], now: OffsetDateTime, assets: &[Asset]) -> semver::Version {
    if let Some(v) = names.iter().find_map(|n| version_in(n)) {
        return v;
    }
    let hash = names
        .iter()
        .find_map(|n| build_hash(n))
        .or_else(|| assets.iter().find_map(|a| a.sha256.as_ref()).map(|h| h.as_str()[..12].to_owned()))
        .unwrap_or_else(|| "unknown".into());
    let d = now.date();
    format!("{}.{}.{}+build.{hash}", d.year(), u8::from(d.month()), d.day())
        .parse()
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}

/// The first `[v]X.Y.Z` in a file name.
fn version_in(name: &str) -> Option<semver::Version> {
    let b = name.as_bytes();
    (0..b.len()).find_map(|i| {
        if !b[i].is_ascii_digit() || (i > 0 && (b[i - 1].is_ascii_digit() || b[i - 1] == b'.')) {
            return None;
        }
        let end = b[i..].iter().position(|c| !(c.is_ascii_digit() || *c == b'.')).map_or(b.len(), |p| i + p);
        let parts: Vec<&str> = name[i..end].trim_end_matches('.').split('.').collect();
        if parts.len() >= 3 && parts[..3].iter().all(|p| !p.is_empty()) {
            parts[..3].join(".").parse().ok()
        } else {
            None
        }
    })
}

/// A trailing hex build ID in a file name: `zynk-ui-linux-x86_64-091ee20dff0c.AppImage`.
fn build_hash(name: &str) -> Option<String> {
    let stem = name.split('.').next()?;
    let last = stem.rsplit('-').next()?;
    (last.len() >= 6 && last.bytes().all(|c| c.is_ascii_hexdigit())).then(|| last.to_ascii_lowercase())
}

/// Writes `catalog.json` into `out`.
pub fn write_catalog(catalog: &Catalog, out: &Path) -> Result<(), PublishError> {
    let json = catalog.to_json_pretty()?;
    Catalog::from_json(&json)?;
    std::fs::create_dir_all(out).map_err(|e| PublishError::Io { path: out.to_owned(), source: e })?;
    let p = out.join("catalog.json");
    std::fs::write(&p, json).map_err(|e| PublishError::Io { path: p, source: e })
}

/// Writes the apps' icons into `out/icons`, to be uploaded with the catalog. Icons of
/// earlier catalogs stay there: a copy of Stockroom still showing an older list asks for them.
pub fn write_icons(apps: &[AppMeta], out: &Path) -> Result<usize, PublishError> {
    let dir = out.join("icons");
    let files = icon_files(apps);
    if !files.is_empty() {
        std::fs::create_dir_all(&dir).map_err(|e| PublishError::Io { path: dir.clone(), source: e })?;
    }
    for (name, png) in &files {
        let p = dir.join(name);
        std::fs::write(&p, png).map_err(|e| PublishError::Io { path: p, source: e })?;
    }
    Ok(files.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_app_metadata_parses() {
        let catalog = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../catalog");
        // Only the apps whose source is public on GitHub are published.
        let apps = read_apps(&catalog.join("apps")).unwrap();
        let ids: Vec<&str> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["dozed", "pagebara", "specular"]);
        let specular = apps.iter().find(|a| a.id == "specular").unwrap();
        assert_eq!(specular.github.as_deref(), Some("amazynk/specular"));
        assert_eq!(specular.assets.len(), 6);
        // Each has an icon, published under a name with its checksum.
        let icons = icon_files(&apps);
        assert_eq!(icons.len(), 3);
        assert!(icons.iter().all(|(name, png)| name.contains('-') && png.starts_with(PNG_SIGNATURE)));
        // With no GitHub lookups, the catalog builds and validates.
        let offline: Vec<AppMeta> = apps
            .into_iter()
            .map(|mut a| {
                a.github = None;
                a
            })
            .collect();
        let offline: Vec<AppMeta> = offline
            .into_iter()
            .map(|mut a| {
                // Keep only the store listings: nothing is downloaded.
                a.assets.retain(|r| r.store.is_some());
                a.extras.clear();
                a
            })
            .collect();
        let c = build(&offline, &Fetcher::default(), &Options::default(), &mut |_| {}).unwrap();
        // The family's apps in their order, the released ones first.
        let order: Vec<&str> = c.apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(order, ["specular", "dozed", "pagebara"]);
        // Pagebara has no release yet: listed as coming, with nothing to install.
        let p = c.app("pagebara").unwrap();
        assert_eq!(p.status, AppStatus::Unreleased);
        assert_eq!(p.releases.len(), 0);
        let icon = c.app("specular").unwrap().icon.as_ref().unwrap();
        assert!(icon.url.starts_with("https://amazynk.com/stockroom/icons/specular-"), "{}", icon.url);
        assert!(icon.url.ends_with(&format!("{}.png", &icon.sha256.as_str()[..12])), "{}", icon.url);
        // Dozed sets itself up from its page: listed for Linux, never downloaded.
        let d = c.app("dozed").unwrap();
        assert!(d.releases[0].assets.iter().all(|a| {
            a.os == Os::Linux && a.store.as_ref().is_some_and(|s| s.store == suite_catalog::Store::Website)
        }));
    }

    #[test]
    fn versions_from_file_names() {
        let day = time::Date::from_calendar_date(2026, time::Month::November, 15).unwrap();
        let now = OffsetDateTime::new_utc(day, time::Time::MIDNIGHT);
        let v = |n: &str| version_from_names(&[n.to_owned()], now, &[]).to_string();
        assert_eq!(v("zynk-cli-linux-x86_64-v1.6.0.tgz"), "1.6.0");
        assert_eq!(v("zynk-ui-linux-x86_64-091ee20dff0c.AppImage"), "2026.11.15+build.091ee20dff0c");
        assert_eq!(v("thing.bin"), "2026.11.15+build.unknown");
        let p = "zynk-ui-linux-x86_64-{build}.AppImage";
        assert!(matches_pattern(p, "zynk-ui-linux-x86_64-091ee20dff0c.AppImage"));
        assert!(!matches_pattern(p, "other.AppImage"));
        assert!(matches_pattern("exact.bin", "exact.bin"));
    }

    #[test]
    fn sha256_files() {
        let h = "973d4dd3c1f40197ae576f489239da1747df9a75858750bd9c55bd78f329210d";
        assert_eq!(
            parse_sha256_file(&format!("{h}  specular-0.1.1-linux-x86_64.tar.gz\n")).unwrap().as_str(),
            h
        );
        assert!(parse_sha256_file("").is_none());
        assert!(parse_sha256_file("nothex  file").is_none());
    }
}
