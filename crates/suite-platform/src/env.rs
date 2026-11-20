use std::{path::Path, sync::Arc};

use suite_catalog::Os;

use crate::{
    Paths, PlatformError,
    network::{FixedNetwork, Metered, NetworkProbe, SystemNetwork},
    process::{FakeProcesses, ProcessProbe, SystemProcesses},
    registry::{MemRegistry, Registry},
    runner::{FakeRunner, RootedRunner, Runner, SystemRunner},
};

/// The computer as Stockroom sees it. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Env {
    /// The OS whose conventions are followed. Tests may pick another OS than the host's,
    /// which exercises the Windows and macOS integration code on Linux.
    pub os: Os,
    pub paths: Paths,
    pub runner: Arc<dyn Runner>,
    pub registry: Arc<dyn Registry>,
    pub processes: Arc<dyn ProcessProbe>,
    pub network: Arc<dyn NetworkProbe>,
    /// Everything lives under a test or development root; nothing real is changed.
    pub sandboxed: bool,
}

impl Env {
    /// The real computer and the current user.
    pub fn real() -> Result<Self, PlatformError> {
        Ok(Self {
            os: Os::current(),
            paths: Paths::real()?,
            runner: Arc::new(SystemRunner),
            registry: real_registry(),
            processes: Arc::new(SystemProcesses),
            network: Arc::new(SystemNetwork),
            sandboxed: false,
        })
    }

    /// Everything under `root` for `stockroom --root`: paths below it, a persistent fake
    /// registry inside it, and commands that would change the system (systemctl, flatpak,
    /// schtasks, ...) not run at all; see [`RootedRunner`] for the two that are. Running
    /// processes are still the real ones (read-only), so an app started from the root counts
    /// as running.
    pub fn rooted(root: &Path) -> Result<Self, PlatformError> {
        Self::rooted_as(root, Os::current())
    }

    /// [`Env::rooted`] following the conventions of `os`, which need not be this computer's:
    /// the Windows integration can be tried on a Mac, through the stand-ins.
    pub fn rooted_as(root: &Path, os: Os) -> Result<Self, PlatformError> {
        let registry = MemRegistry::persistent(&root.join("registry.json"))
            .map_err(|e| PlatformError::io(&root.join("registry.json"), e))?;
        Ok(Self {
            os,
            paths: Paths::rooted(root, os),
            runner: Arc::new(RootedRunner::new(root, os == Os::Macos && cfg!(target_os = "macos"))),
            registry: Arc::new(registry),
            processes: Arc::new(SystemProcesses),
            network: Arc::new(FixedNetwork(Metered::Unknown)),
            sandboxed: true,
        })
    }

    /// A fully fake environment under `root` for unit tests: commands fail as "not found"
    /// unless scripted, nothing is running, the network isn't metered.
    pub fn for_tests(root: &Path, os: Os) -> Self {
        Self {
            os,
            paths: Paths::rooted(root, os),
            runner: Arc::new(FakeRunner::new()),
            registry: Arc::new(MemRegistry::new()),
            processes: Arc::new(FakeProcesses::new()),
            network: Arc::new(FixedNetwork(Metered::No)),
            sandboxed: true,
        }
    }
}

#[cfg(windows)]
fn real_registry() -> Arc<dyn Registry> {
    Arc::new(crate::registry::WinRegistry)
}

#[cfg(not(windows))]
fn real_registry() -> Arc<dyn Registry> {
    // No registry outside Windows: an empty one that is never saved.
    Arc::new(MemRegistry::new())
}
