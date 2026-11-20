//! Installing, updating, rolling back and removing apps, with atomic `current` switching,
//! the previous version kept, per-OS integration and a crash-safe state file (PLAN.md §4).

mod installer;
pub mod integrate;
pub mod pointer;
pub mod settle;
pub mod state;

use std::path::{Path, PathBuf};

pub use installer::{Event, Failpoint, Installer, Outcome, Stage};
pub use state::{
    AppState, Change, ChangeKind, Created, HISTORY_LIMIT, InstallKind, Policy, Scope, Settings, State,
};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("another copy of Stockroom is installing or updating right now; try again in a moment")]
    Busy,
    #[error("{0} is not installed by Stockroom")]
    NotInstalled(String),
    #[error("there is no previous version to go back to")]
    NoPrevious,
    #[error("{app} is running (process {pids:?}); close it first")]
    Running { app: String, pids: Vec<u32> },
    #[error("{0}")]
    Unsupported(String),
    #[error("the download doesn't contain the expected program {0:?}")]
    LayoutMissing(String),
    #[error(transparent)]
    Fetch(#[from] suite_fetch::FetchError),
    #[error(transparent)]
    Unpack(#[from] suite_unpack::UnpackError),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("state: {0}")]
    State(String),
    #[error("registry: {0}")]
    Registry(String),
    #[error("shortcut: {0}")]
    Shortcut(String),
    #[error(
        "{app} released a new build; the catalog needs refreshing before it can be installed (nothing was changed)"
    )]
    PinnedChanged { app: String },
    #[error("stopped at failpoint {0:?}")]
    Failpoint(Failpoint),
}

impl InstallError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io { path: path.to_owned(), source }
    }
}
