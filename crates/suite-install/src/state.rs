//! The state file: what Stockroom installed and created, as versioned JSON written
//! atomically (temporary file, fsync, rename).

use std::{
    collections::BTreeMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use suite_catalog::{Channel, Layout};
use suite_platform::Hive;
use time::OffsetDateTime;

use crate::InstallError;

/// Format of the state file. Bump on incompatible changes and migrate in [`State::load`].
pub const STATE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub schema: u32,
    #[serde(default)]
    pub apps: BTreeMap<String, AppState>,
    #[serde(default)]
    pub catalog: CatalogState,
    #[serde(default)]
    pub settings: Settings,
    /// What changed, oldest first, for the app window's log; at most [`HISTORY_LIMIT`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Change>,
}

/// How many [`Change`]s the state keeps.
pub const HISTORY_LIMIT: usize = 200;

/// One finished change to an app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub app: String,
    pub name: String,
    pub what: ChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ChangeKind {
    Installed {
        version: semver::Version,
    },
    Updated {
        from: semver::Version,
        to: semver::Version,
    },
    RolledBack {
        from: semver::Version,
        to: semver::Version,
    },
    Removed {
        version: semver::Version,
        purged: bool,
    },
    /// Handed to an app store, which installs it.
    OpenedStore {
        store: suite_catalog::Store,
    },
}

impl State {
    /// Appends to the history, dropping the oldest entries past [`HISTORY_LIMIT`].
    pub fn record(&mut self, change: Change) {
        self.history.push(change);
        let excess = self.history.len().saturating_sub(HISTORY_LIMIT);
        self.history.drain(..excess);
    }
}

/// What is remembered about catalogs: the newest serial and the last background check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogState {
    /// Highest catalog serial accepted so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_version: Option<u64>,
    /// Last background check, for "at most daily".
    #[serde(default, with = "time::serde::rfc3339::option", skip_serializing_if = "Option::is_none")]
    pub last_check: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Background updates registered with the OS scheduler.
    pub auto_update: bool,
    /// Skip background updates on metered connections.
    pub skip_metered: bool,
    /// Prefer Flatpak bundles on Linux.
    pub prefer_flatpak: bool,
    /// Who new installs are for.
    #[serde(default)]
    pub scope: Scope,
}

impl Default for Settings {
    fn default() -> Self {
        Self { auto_update: true, skip_metered: true, prefer_flatpak: false, scope: Scope::User }
    }
}

/// Who an install is for (PLAN.md §6, "install location").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// This user only, without admin rights (the default).
    #[default]
    User,
    /// Everyone on the computer; needs admin rights. Not offered until the installers can
    /// do it (see `Core::all_users_supported`).
    AllUsers,
}

/// How updates for an app are applied (PLAN.md §5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Install updates automatically (default).
    #[default]
    Auto,
    /// Ask first.
    Ask,
    /// Never update.
    Never,
}

impl std::str::FromStr for Policy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(Self::Auto),
            "ask" => Ok(Self::Ask),
            "never" => Ok(Self::Never),
            other => Err(format!("unknown policy {other:?} (auto, ask, never)")),
        }
    }
}

impl std::fmt::Display for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
            Self::Never => "never",
        })
    }
}

/// How an app is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallKind {
    /// Unpacked from a tar.gz or zip into version folders.
    Archive,
    /// A single `AppImage` file in each version folder.
    AppImage,
    /// Installed by `flatpak --user`; only the version is tracked.
    Flatpak,
    /// A `.app` bundle from a dmg, in version folders and copied to ~/Applications.
    MacBundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppState {
    pub name: String,
    pub kind: InstallKind,
    pub channel: Channel,
    #[serde(default)]
    pub policy: Policy,
    pub current: semver::Version,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<semver::Version>,
    /// Version folders on disk (current, previous, and older ones still in use).
    #[serde(default)]
    pub versions: Vec<semver::Version>,
    /// The main executable relative to a version folder (bundle name for `MacBundle`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flatpak_id: Option<String>,
    /// Desktop/Flatpak ID, used for the desktop file and icons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_id: Option<String>,
    /// Name of the `~/.local/bin` command (Linux).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin_name: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pitch: String,
    /// The layout of each version on disk (by version), for re-integration on rollback.
    #[serde(default)]
    pub layouts: BTreeMap<String, Layout>,
    #[serde(with = "time::serde::rfc3339")]
    pub installed: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated: OffsetDateTime,
    /// Everything created outside the app's own folder; removal deletes exactly these.
    #[serde(default)]
    pub created: Vec<Created>,
    /// The user asked for the app's file types to be registered (Windows "Open with").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub associate: bool,
}

impl AppState {
    /// The main executable of `version` (relative to its folder).
    pub fn exe_for(&self, version: &semver::Version) -> Option<&str> {
        self.layouts
            .get(&version.to_string())
            .and_then(|l| l.executable.as_deref())
            .or(self.executable.as_deref().filter(|_| *version == self.current))
    }

    /// Whether `path` is a file, link or folder Stockroom created for this app.
    pub fn created_path(&self, path: &Path) -> bool {
        self.created.iter().any(|c| c.path() == Some(path))
    }
}

/// Something Stockroom created for an app, outside its version folders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Created {
    File {
        path: PathBuf,
    },
    Symlink {
        path: PathBuf,
    },
    /// A folder Stockroom made and owns entirely (a copied `.app` bundle).
    Dir {
        path: PathBuf,
    },
    RegistryKey {
        hive: Hive,
        key: String,
    },
    /// One value in a key Stockroom shares with others (`.ext\OpenWithProgids`). Removal
    /// deletes the value, then the key and its parents only if nothing else is left in them.
    RegistryValue {
        hive: Hive,
        key: String,
        name: String,
    },
}

impl Created {
    /// The file system path, for entries that are one.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::File { path } | Self::Symlink { path } | Self::Dir { path } => Some(path),
            Self::RegistryKey { .. } | Self::RegistryValue { .. } => None,
        }
    }
}

impl State {
    pub fn new() -> Self {
        Self { schema: STATE_SCHEMA, ..Self::default() }
    }

    /// Loads the state; a missing file is an empty state.
    pub fn load(path: &Path) -> Result<Self, InstallError> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(e) => return Err(InstallError::io(path, e)),
        };
        let state: Self = serde_json::from_slice(&bytes)
            .map_err(|e| InstallError::State(format!("{} is damaged: {e}", path.display())))?;
        if state.schema > STATE_SCHEMA {
            return Err(InstallError::State(format!(
                "{} was written by a newer version of Stockroom (format {})",
                path.display(),
                state.schema
            )));
        }
        Ok(Self { schema: STATE_SCHEMA, ..state })
    }

    /// Writes the state atomically: a crash leaves either the old or the new file.
    pub fn save(&self, path: &Path) -> Result<(), InstallError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| InstallError::State(e.to_string()))?;
        write_atomic(path, &bytes)
    }
}

/// Writes `bytes` to `path` via a temporary file in the same folder, fsynced, renamed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    let dir =
        path.parent().ok_or_else(|| InstallError::State(format!("{} has no folder", path.display())))?;
    std::fs::create_dir_all(dir).map_err(|e| InstallError::io(dir, e))?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_owned();
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);
    let mut f = File::create(&tmp).map_err(|e| InstallError::io(&tmp, e))?;
    f.write_all(bytes).map_err(|e| InstallError::io(&tmp, e))?;
    f.sync_all().map_err(|e| InstallError::io(&tmp, e))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| InstallError::io(path, e))?;
    sync_dir(dir);
    Ok(())
}

/// Makes a rename durable (Unix); best effort.
pub(crate) fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_atomic_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/state.json");
        assert_eq!(State::load(&path).unwrap(), State::new());
        let mut s = State::new();
        s.catalog.last_version = Some(7);
        let now = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap();
        s.apps.insert(
            "demo".into(),
            AppState {
                name: "Demo".into(),
                kind: InstallKind::Archive,
                channel: Channel::Beta,
                policy: Policy::Ask,
                current: "1.2.0".parse().unwrap(),
                previous: Some("1.1.0".parse().unwrap()),
                versions: vec!["1.1.0".parse().unwrap(), "1.2.0".parse().unwrap()],
                executable: Some("demo".into()),
                flatpak_id: None,
                desktop_id: Some("com.example.demo".into()),
                bin_name: Some("demo".into()),
                pitch: String::new(),
                layouts: BTreeMap::new(),
                installed: now,
                updated: now,
                created: vec![
                    Created::Symlink { path: "/x/bin/demo".into() },
                    Created::RegistryKey { hive: Hive::CurrentUser, key: "Software\\X".into() },
                    Created::RegistryValue {
                        hive: Hive::CurrentUser,
                        key: "Software\\Classes\\.demo\\OpenWithProgids".into(),
                        name: "Stockroom.demo".into(),
                    },
                ],
                associate: true,
            },
        );
        s.save(&path).unwrap();
        assert!(!dir.path().join("sub/state.json.tmp").exists());
        assert_eq!(State::load(&path).unwrap(), s);
    }

    #[test]
    fn refuses_damaged_or_newer_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(State::load(&path), Err(InstallError::State(_))));
        std::fs::write(&path, r#"{"schema": 99}"#).unwrap();
        assert!(State::load(&path).unwrap_err().to_string().contains("newer version of Stockroom"));
        std::fs::write(&path, r#"{"schema": 1}"#).unwrap();
        assert_eq!(State::load(&path).unwrap(), State::new());
    }

    #[test]
    fn history_keeps_the_newest() {
        let mut s = State::new();
        let at = OffsetDateTime::UNIX_EPOCH;
        for i in 0..HISTORY_LIMIT + 5 {
            s.record(Change {
                at,
                app: format!("a{i}"),
                name: "A".into(),
                what: ChangeKind::Installed { version: "1.0.0".parse().unwrap() },
            });
        }
        assert_eq!(s.history.len(), HISTORY_LIMIT);
        assert_eq!(s.history[0].app, "a5");
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""kind":"installed""#), "{json}");
        let back: State = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        // Older state files have neither the history nor the scope.
        let old: State = serde_json::from_str(
            r#"{"schema":1,"settings":{"auto_update":false,"skip_metered":true,"prefer_flatpak":false}}"#,
        )
        .unwrap();
        assert_eq!(old.settings.scope, Scope::User);
        assert_eq!(old.history, []);
    }
}
