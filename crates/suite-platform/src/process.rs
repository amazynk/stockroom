//! Is an app running? Found by executable path: a process whose executable lives under the
//! app's folder.

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningProcess {
    pub pid: u32,
    pub exe: PathBuf,
}

pub trait ProcessProbe: fmt::Debug + Send + Sync {
    /// Processes whose executable is inside `dir`.
    fn running_under(&self, dir: &Path) -> Vec<RunningProcess>;
}

fn canonical(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned())
}

/// The real process list: `/proc` on Linux, `sysinfo` elsewhere.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemProcesses;

#[cfg(target_os = "linux")]
impl ProcessProbe for SystemProcesses {
    fn running_under(&self, dir: &Path) -> Vec<RunningProcess> {
        let dir = canonical(dir);
        let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            // Other users' processes can't be read; they can't be running our per-user copy.
            let Ok(exe) = std::fs::read_link(e.path().join("exe")) else { continue };
            // A replaced or deleted executable reads as "/path (deleted)".
            let exe = match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
                Some(s) => PathBuf::from(s),
                None => exe,
            };
            if exe.starts_with(&dir) {
                out.push(RunningProcess { pid, exe });
            }
        }
        out
    }
}

#[cfg(not(target_os = "linux"))]
impl ProcessProbe for SystemProcesses {
    fn running_under(&self, dir: &Path) -> Vec<RunningProcess> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let dir = canonical(dir);
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        sys.processes()
            .iter()
            .filter_map(|(pid, p)| {
                let exe = p.exe()?;
                exe.starts_with(&dir).then(|| RunningProcess { pid: pid.as_u32(), exe: exe.to_owned() })
            })
            .collect()
    }
}

/// A fixed list of "running" executables, for tests.
#[derive(Debug, Default)]
pub struct FakeProcesses {
    exes: Mutex<Vec<PathBuf>>,
}

impl FakeProcesses {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&self, exe: &Path) {
        lock(&self.exes).push(canonical(exe));
    }

    pub fn stop_all(&self) {
        lock(&self.exes).clear();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ProcessProbe for FakeProcesses {
    fn running_under(&self, dir: &Path) -> Vec<RunningProcess> {
        let dir = canonical(dir);
        lock(&self.exes)
            .iter()
            .enumerate()
            .filter(|(_, e)| e.starts_with(&dir))
            .map(|(i, e)| RunningProcess { pid: u32::try_from(i).unwrap_or(u32::MAX), exe: e.clone() })
            .collect()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn finds_a_real_child_by_executable_path() {
        let dir = tempfile::tempdir().unwrap();
        // Coreutils may be a multi-call binary that dispatches on its name: keep the name.
        let exe = dir.path().join("sleep");
        std::fs::copy("/bin/sleep", &exe).unwrap();
        let mut child = std::process::Command::new(&exe).arg("30").spawn().unwrap();
        // Until the child has exec'd, /proc shows the test binary; give it a moment.
        let mut found = Vec::new();
        for _ in 0..200 {
            found = SystemProcesses.running_under(dir.path());
            if !found.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(found.iter().any(|p| p.pid == child.id()), "{found:?}");
        assert_eq!(SystemProcesses.running_under(dir.path()), []);
    }
}
