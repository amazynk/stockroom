//! Everything Stockroom needs to know about, or do to, the computer: facts (OS, CPU, glibc,
//! OS versions), locations, external commands, the Windows registry, running processes,
//! metered networks and the OS scheduler.
//!
//! All of it is reached through an [`Env`], so tests and `--root` point every path at a
//! temporary folder and every command at a fake.

pub mod activity;
mod env;
pub mod facts;
pub mod macos;
pub mod network;
mod paths;
pub mod process;
pub mod registry;
pub mod runner;
pub mod scheduler;

use std::path::{Path, PathBuf};

pub use activity::UserActivity;
pub use env::Env;
pub use network::{Metered, NetworkProbe};
pub use paths::{Bases, Paths};
pub use process::{ProcessProbe, RunningProcess};
pub use registry::{Hive, Registry};
pub use runner::{FakeRunner, Output, Runner, SystemRunner};
pub use scheduler::{ScheduleStatus, Scheduler, SchedulerKind};
pub use suite_catalog::{Arch, HostFacts, Os};

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("no home folder could be found")]
    NoHome,
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{0} failed: {1}")]
    Command(String, String),
}

impl PlatformError {
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io { path: path.to_owned(), source }
    }
}
