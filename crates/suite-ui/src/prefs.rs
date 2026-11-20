//! The window's own settings (theme and language), kept next to Stockroom's state. Install
//! settings live in the state file and belong to `suite-core`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::look::ThemeChoice;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Prefs {
    pub theme: ThemeChoice,
    /// A language code from `i18n::LANGUAGES`; `None` follows the system.
    pub language: Option<String>,
}

impl Prefs {
    /// Missing or unreadable files give the defaults.
    pub(crate) fn load(path: Option<&Path>) -> Self {
        path.and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self, path: Option<&Path>) {
        let Some(path) = path else { return };
        let saved = serde_json::to_vec_pretty(self)
            .map_err(|e| e.to_string())
            .and_then(|bytes| suite_install::state::write_atomic(path, &bytes).map_err(|e| e.to_string()));
        if let Err(e) = saved {
            tracing::warn!("window settings not saved: {e}");
        }
    }
}

/// `ui.json` in Stockroom's data folder.
pub(crate) fn path_in(data: &Path) -> PathBuf {
    data.join("ui.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(dir.path());
        assert_eq!(Prefs::load(Some(&path)), Prefs::default());
        let p = Prefs { theme: ThemeChoice::Dark, language: Some("nl".into()) };
        p.save(Some(&path));
        assert_eq!(Prefs::load(Some(&path)), p);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Prefs::load(Some(&path)), Prefs::default());
        std::fs::write(&path, r#"{"theme":"light"}"#).unwrap();
        assert_eq!(Prefs::load(Some(&path)).theme, ThemeChoice::Light);
    }
}
