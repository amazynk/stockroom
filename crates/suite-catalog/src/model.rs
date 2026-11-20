//! The catalog document (PLAN.md §3).

use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    error::CatalogError,
    host::{Arch, Os},
};

/// The catalog format this build understands. A catalog with a higher number changed in a
/// way older Suites can't work with, and is refused with a plain "update Stockroom" message.
///
/// Additions don't need a new number: unknown fields are ignored, and an app, release,
/// add-on or asset this build can't read (a new kind of package, a new store) is left out
/// of what it sees ([`known`]), so the list can grow without every copy of Stockroom being updated
/// first. What it does read still has to pass [`Catalog::validate`] as a whole.
pub const SCHEMA: u32 = 1;

/// Reads a list, leaving out the entries this build doesn't understand.
fn known<'de, D, T>(de: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let entries = Vec::<serde_json::Value>::deserialize(de)?;
    Ok(entries
        .into_iter()
        .filter_map(|entry| match T::deserialize(entry) {
            Ok(known) => Some(known),
            Err(e) => {
                tracing::debug!("a catalog entry this version of Stockroom can't read was left out: {e}");
                None
            }
        })
        .collect())
}

/// The whole catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    /// Format version; see [`SCHEMA`].
    pub schema: u32,
    /// Increasing serial; a client never goes back to a lower one than it has seen, so a
    /// stale copy on the way can't undo a newer list.
    pub version: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub published: OffsetDateTime,
    #[serde(deserialize_with = "known")]
    pub apps: Vec<App>,
}

/// One app of the family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    /// Short, path-safe ID (`[a-z0-9-]`), e.g. `specular`. Used in folder names.
    pub id: String,
    /// Its number in the amazynk catalogue (`AMZ-01`), shown beside its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    pub name: String,
    /// One-line pitch.
    pub pitch: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<Icon>,
    pub homepage: String,
    pub license: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub status: AppStatus,
    #[serde(default)]
    pub ids: PlatformIds,
    /// Settings and data folders, removed only on request (`stockroom remove --purge`).
    #[serde(default)]
    pub data_paths: DataPaths,
    /// Who makes the app, when it isn't one of the family (shown, and listed after them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// Phone versions, for a "Get it on your phone" code in the app window.
    #[serde(default, skip_serializing_if = "Mobile::is_empty")]
    pub mobile: Mobile,
    /// Optional add-ons (a command-line tool, for instance), installed only on request.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "known")]
    pub extras: Vec<Extra>,
    #[serde(default, deserialize_with = "known")]
    pub releases: Vec<Release>,
}

/// Whether an app can be installed yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppStatus {
    #[default]
    Available,
    /// Listed but without releases yet: shown as "coming soon".
    Unreleased,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Icon {
    pub url: String,
    pub sha256: Sha256Hex,
}

/// The app's identifiers on each platform, used to find installs made by other means.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformIds {
    /// Freedesktop/Flatpak application ID, also the desktop file stem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flatpak: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macos_bundle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_aumid: Option<String>,
    /// Uninstall key name (or display name) used by the app's own MSI/installer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_uninstall_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homebrew_cask: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winget: Option<String>,
    /// Executable names to look for on PATH and in `/usr/bin`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executables: Vec<String>,
    /// Windows package family name of a Microsoft Store (AppX/MSIX) install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appx_family: Option<String>,
}

/// Per-OS data folders. Paths start with `~/` (home) and use `/` separators.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPaths {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linux: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macos: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<String>,
}

impl DataPaths {
    pub fn for_os(&self, os: Os) -> &[String] {
        match os {
            Os::Linux => &self.linux,
            Os::Macos => &self.macos,
            Os::Windows => &self.windows,
        }
    }
}

/// Release channel. `beta` sees stable releases too.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Channel {
    type Err = CatalogError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stable" => Ok(Self::Stable),
            "beta" => Ok(Self::Beta),
            other => Err(CatalogError::Invalid(format!("unknown channel {other:?}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: semver::Version,
    #[serde(default)]
    pub channel: Channel,
    #[serde(default, with = "time::serde::rfc3339::option", skip_serializing_if = "Option::is_none")]
    pub date: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_url: Option<String>,
    #[serde(deserialize_with = "known")]
    pub assets: Vec<Asset>,
}

/// How an asset is packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetKind {
    #[serde(rename = "tar.gz")]
    TarGz,
    #[serde(rename = "zip")]
    Zip,
    #[serde(rename = "dmg")]
    Dmg,
    #[serde(rename = "appimage")]
    AppImage,
    #[serde(rename = "flatpak-bundle")]
    FlatpakBundle,
    #[serde(rename = "msi")]
    Msi,
    /// Installed and updated by an app store; Stockroom only opens the store page.
    #[serde(rename = "store")]
    Store,
}

impl AssetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TarGz => "tar.gz",
            Self::Zip => "zip",
            Self::Dmg => "dmg",
            Self::AppImage => "appimage",
            Self::FlatpakBundle => "flatpak-bundle",
            Self::Msi => "msi",
            Self::Store => "store",
        }
    }
}

impl fmt::Display for AssetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub os: Os,
    pub arch: Arch,
    pub kind: AssetKind,
    /// The download (for a store asset: the store's web page).
    pub url: String,
    /// Bytes; 0 for a store asset.
    #[serde(default)]
    pub size: u64,
    /// Required for everything Stockroom downloads; absent for a store asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Sha256Hex>,
    #[serde(default, skip_serializing_if = "Requirements::is_empty")]
    pub requires: Requirements,
    #[serde(default, skip_serializing_if = "Layout::is_empty")]
    pub layout: Layout,
    /// For `kind = "store"`: which store and how to open it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<StoreLink>,
    /// The vendor publishes no checksum: size and SHA-256 were recorded ("pinned") by the
    /// publish tool from a moving "latest" URL. When the vendor ships a new build, the
    /// download no longer matches and nothing is installed until the catalog is refreshed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Where an app that Stockroom doesn't install itself is had from: an app store, or the
/// app's own website.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Store {
    Microsoft,
    MacAppStore,
    /// Not a store: the app's own page, which says how to install it (an app that builds
    /// from source or sets itself up with its own script).
    Website,
}

impl Store {
    pub fn name(self) -> &'static str {
        match self {
            Self::Microsoft => "Microsoft Store",
            Self::MacAppStore => "Mac App Store",
            Self::Website => "website",
        }
    }

    /// The URL scheme its deep links use.
    pub fn scheme(self) -> &'static str {
        match self {
            Self::Microsoft => "ms-windows-store",
            Self::MacAppStore => "macappstore",
            Self::Website => "https",
        }
    }
}

/// Where an app lives in a store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreLink {
    pub store: Store,
    pub product_id: String,
    /// The store's web page.
    pub url: String,
    /// Deep link that opens the store app on the product.
    pub link: String,
}

/// Phone apps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mobile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ios: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub android: Option<String>,
}

impl Mobile {
    pub fn is_empty(&self) -> bool {
        self.ios.is_none() && self.android.is_none()
    }
}

/// An optional add-on of an app, with its own version and assets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extra {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<semver::Version>,
    #[serde(deserialize_with = "known")]
    pub assets: Vec<Asset>,
}

/// What a computer needs to run an asset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirements {
    /// x86-64-v3 (AVX2, BMI1/2, FMA, ...).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub x86_64_v3: bool,
    /// Minimum glibc, e.g. `"2.35"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glibc: Option<String>,
    /// Minimum macOS, e.g. `"12.0"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macos: Option<String>,
    /// Minimum Windows build number, e.g. `19041`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_build: Option<u32>,
}

impl Requirements {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Where things are inside the unpacked asset, relative to its root. A single top-level
/// folder in an archive is stripped, so `specular` means `specular-1.0-linux-x86_64/specular`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    /// The main executable (for a dmg: the `.app` bundle name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<String>,
    /// File extensions the app opens (`pdf`, `cbz`), offered as Windows "Open with"
    /// associations when the user asks for them. Linux and macOS read the app's own desktop
    /// file and Info.plist instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_types: Vec<String>,
}

impl Layout {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A lower-case hex SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Hex(String);

impl Sha256Hex {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Hex-encodes a raw 32-byte digest.
    pub fn from_digest(digest: &[u8]) -> Self {
        use fmt::Write as _;
        let mut s = String::with_capacity(64);
        for b in digest {
            let _ = write!(s, "{b:02x}");
        }
        Self(s)
    }
}

impl TryFrom<String> for Sha256Hex {
    type Error = CatalogError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        let s = s.to_ascii_lowercase();
        if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(Self(s))
        } else {
            Err(CatalogError::Invalid(format!("not a SHA-256 hex digest: {s:?}")))
        }
    }
}

impl std::str::FromStr for Sha256Hex {
    type Err = CatalogError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl From<Sha256Hex> for String {
    fn from(h: Sha256Hex) -> Self {
        h.0
    }
}

impl fmt::Display for Sha256Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Catalog {
    /// Parses JSON and checks the rules in [`Catalog::validate`].
    pub fn from_json(bytes: &[u8]) -> Result<Self, CatalogError> {
        let catalog: Self =
            serde_json::from_slice(bytes).map_err(|e| CatalogError::Malformed(e.to_string()))?;
        if catalog.schema > SCHEMA {
            return Err(CatalogError::UnsupportedSchema(catalog.schema));
        }
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn to_json_pretty(&self) -> Result<Vec<u8>, CatalogError> {
        let mut out = serde_json::to_vec_pretty(self).map_err(|e| CatalogError::Malformed(e.to_string()))?;
        out.push(b'\n');
        Ok(out)
    }

    pub fn app(&self, id: &str) -> Option<&App> {
        self.apps.iter().find(|a| a.id == id)
    }

    /// Rules that keep a wrong catalog from doing damage: path-safe IDs and layout paths,
    /// HTTPS URLs, unique versions.
    pub fn validate(&self) -> Result<(), CatalogError> {
        let mut ids = std::collections::BTreeSet::new();
        for app in &self.apps {
            validate_app_id(&app.id)?;
            if !ids.insert(app.id.as_str()) {
                return Err(CatalogError::Invalid(format!("duplicate app id {:?}", app.id)));
            }
            if let Some(icon) = &app.icon {
                validate_url(&icon.url)?;
            }
            if let Some(part) = &app.part {
                let fits = (1..=12).contains(&part.len())
                    && part.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-');
                if !fits {
                    return Err(CatalogError::Invalid(format!("{}: bad part number {part:?}", app.id)));
                }
            }
            for exe in &app.ids.executables {
                validate_file_name(exe)?;
            }
            for os in [Os::Linux, Os::Macos, Os::Windows] {
                for p in app.data_paths.for_os(os) {
                    validate_data_path(p)?;
                }
            }
            let mut versions = std::collections::BTreeSet::new();
            for release in &app.releases {
                if !versions.insert(&release.version) {
                    return Err(CatalogError::Invalid(format!(
                        "{}: duplicate release {}",
                        app.id, release.version
                    )));
                }
                for asset in &release.assets {
                    validate_asset(&app.id, asset)?;
                }
            }
            for extra in &app.extras {
                validate_app_id(&extra.id)?;
                for asset in &extra.assets {
                    validate_asset(&app.id, asset)?;
                }
            }
            for link in [&app.mobile.ios, &app.mobile.android].into_iter().flatten() {
                validate_url(link)?;
            }
        }
        Ok(())
    }
}

fn validate_asset(app: &str, asset: &Asset) -> Result<(), CatalogError> {
    validate_url(&asset.url)?;
    if asset.kind == AssetKind::Store {
        let store = asset
            .store
            .as_ref()
            .ok_or_else(|| CatalogError::Invalid(format!("{app}: store asset without store details")))?;
        validate_url(&store.url)?;
        if store.store == Store::Website {
            validate_url(&store.link)?;
        }
        let scheme_ok = store.link.strip_prefix(store.store.scheme()).is_some_and(|r| r.starts_with("://"));
        if !scheme_ok || store.product_id.is_empty() {
            return Err(CatalogError::Invalid(format!(
                "{app}: bad {} link {:?}",
                store.store.name(),
                store.link
            )));
        }
        return Ok(());
    }
    if asset.size == 0 || asset.sha256.is_none() {
        return Err(CatalogError::Invalid(format!("{app}: a downloaded asset needs a size and a sha256")));
    }
    for p in [&asset.layout.executable, &asset.layout.icon, &asset.layout.desktop].into_iter().flatten() {
        validate_relative_path(p)?;
    }
    Ok(())
}

/// App IDs become folder names: keep them boring.
pub fn validate_app_id(id: &str) -> Result<(), CatalogError> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-');
    if ok { Ok(()) } else { Err(CatalogError::Invalid(format!("bad app id {id:?}"))) }
}

fn validate_file_name(name: &str) -> Result<(), CatalogError> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0']);
    if ok { Ok(()) } else { Err(CatalogError::Invalid(format!("bad file name {name:?}"))) }
}

/// A relative path with `/` separators and no `..`, empty or absolute components.
pub fn validate_relative_path(path: &str) -> Result<(), CatalogError> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return Err(CatalogError::Invalid(format!("bad relative path {path:?}")));
    }
    for part in path.split('/') {
        validate_file_name(part)?;
    }
    Ok(())
}

fn validate_data_path(path: &str) -> Result<(), CatalogError> {
    match path.strip_prefix("~/") {
        Some(rest) => validate_relative_path(rest),
        None => Err(CatalogError::Invalid(format!("data path must start with ~/: {path:?}"))),
    }
}

/// HTTPS only, except plain HTTP to the loopback interface (tests and local mirrors).
pub fn validate_url(raw: &str) -> Result<(), CatalogError> {
    let url = url::Url::parse(raw).map_err(|e| CatalogError::Invalid(format!("bad url {raw:?}: {e}")))?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d == "localhost",
        None => false,
    };
    match url.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        _ => Err(CatalogError::Invalid(format!("url must be https: {raw:?}"))),
    }
}
