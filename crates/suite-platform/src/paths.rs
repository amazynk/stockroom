//! Every location Stockroom reads or writes, computed once. Tests and `--root` put all of
//! them under a temporary folder, so nothing touches the real home or system.

use std::path::{Path, PathBuf};

use suite_catalog::Os;
use suite_identity::{NAME, SLUG};

use crate::PlatformError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
    /// Stockroom's own data: state file, lock, staging.
    pub data: PathBuf,
    /// Where apps are unpacked, one folder per app with version folders inside.
    pub apps: PathBuf,
    /// Partial downloads (resumable).
    pub downloads: PathBuf,
    pub state_file: PathBuf,
    /// `~/.local/bin` (Linux).
    pub bin: PathBuf,
    /// `~/.local/share` (Linux): `applications`, `icons`, `mime` live here.
    pub xdg_data: PathBuf,
    /// `~/.config` (Linux): `systemd/user`, `autostart`.
    pub xdg_config: PathBuf,
    /// `~/Applications` (macOS).
    pub mac_applications: PathBuf,
    /// `~/Library/LaunchAgents` (macOS).
    pub launch_agents: PathBuf,
    /// Start menu programs folder (Windows).
    pub start_menu: PathBuf,
    /// The file system root used for detection (`/usr/bin`, `/Applications`, ...).
    pub sys_root: PathBuf,
    /// Folders searched for commands and other installs.
    pub path_dirs: Vec<PathBuf>,
}

/// The base folders everything else derives from.
#[derive(Debug, Clone)]
pub struct Bases {
    pub home: PathBuf,
    /// `%LOCALAPPDATA%`, `~/.local/share`, or `~/Library/Application Support`.
    pub local_data: PathBuf,
    /// `%APPDATA%` or `~/.config`.
    pub config: PathBuf,
    pub cache: PathBuf,
    pub sys_root: PathBuf,
    pub path_dirs: Vec<PathBuf>,
}

impl Paths {
    pub fn from_bases(os: Os, b: Bases) -> Self {
        let (data, apps, downloads) = match os {
            Os::Linux => {
                let data = b.local_data.join(SLUG);
                (data.clone(), data.join("apps"), b.cache.join(SLUG).join("downloads"))
            }
            Os::Macos => {
                let data = b.local_data.join(NAME);
                (data.clone(), data.join("apps"), b.cache.join(NAME).join("downloads"))
            }
            Os::Windows => {
                let data = b.local_data.join(NAME);
                (data.clone(), b.local_data.join("Programs").join(NAME), data.join("downloads"))
            }
        };
        let home = b.home;
        Self {
            state_file: data.join("state.json"),
            bin: home.join(".local").join("bin"),
            xdg_data: home.join(".local").join("share"),
            xdg_config: home.join(".config"),
            mac_applications: home.join("Applications"),
            launch_agents: home.join("Library").join("LaunchAgents"),
            start_menu: b.config.join("Microsoft").join("Windows").join("Start Menu").join("Programs"),
            data,
            apps,
            downloads,
            home,
            sys_root: b.sys_root,
            path_dirs: b.path_dirs,
        }
    }

    /// Everything under `root`: `root/home/...` for the user, `root/sys/...` for the system.
    pub fn rooted(root: &Path, os: Os) -> Self {
        let home = root.join("home");
        let sys_root = root.join("sys");
        let (local_data, config, cache) = match os {
            Os::Linux => (home.join(".local/share"), home.join(".config"), home.join(".cache")),
            Os::Macos => (
                home.join("Library/Application Support"),
                home.join("Library/Application Support"),
                home.join("Library/Caches"),
            ),
            Os::Windows => {
                (home.join("AppData/Local"), home.join("AppData/Roaming"), home.join("AppData/Local"))
            }
        };
        let path_dirs = vec![sys_root.join("usr/local/bin"), sys_root.join("usr/bin")];
        Self::from_bases(os, Bases { home, local_data, config, cache, sys_root, path_dirs })
    }

    /// The real locations of the current user.
    pub fn real() -> Result<Self, PlatformError> {
        let dirs = directories::BaseDirs::new().ok_or(PlatformError::NoHome)?;
        let os = Os::current();
        let sys_root = if os == Os::Windows {
            let mut drive = std::env::var_os("SystemDrive").unwrap_or_else(|| "C:".into());
            drive.push("\\");
            PathBuf::from(drive)
        } else {
            PathBuf::from("/")
        };
        let path_dirs = std::env::var_os("PATH").map_or_default(|p| std::env::split_paths(&p).collect());
        Ok(Self::from_bases(
            os,
            Bases {
                home: dirs.home_dir().to_owned(),
                local_data: dirs.data_local_dir().to_owned(),
                config: dirs.config_dir().to_owned(),
                cache: dirs.cache_dir().to_owned(),
                sys_root,
                path_dirs,
            },
        ))
    }

    /// A path below the system root, e.g. `sys("usr/bin")`.
    pub fn sys(&self, rel: &str) -> PathBuf {
        self.sys_root.join(rel)
    }

    /// Finds a command in [`Paths::path_dirs`].
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ""] } else { &[""] };
        self.path_dirs
            .iter()
            .find_map(|d| exts.iter().map(|e| d.join(format!("{name}{e}"))).find(|p| p.is_file()))
    }

    /// Expands a catalog data path (`~/...`) under this home. `None` if it isn't one.
    pub fn expand_home(&self, p: &str) -> Option<PathBuf> {
        let rest = p.strip_prefix("~/")?;
        if rest.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
            return None;
        }
        Some(rest.split('/').fold(self.home.clone(), |acc, c| acc.join(c)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_paths_stay_under_root() {
        let root = Path::new("/r");
        for os in [Os::Linux, Os::Macos, Os::Windows] {
            let p = Paths::rooted(root, os);
            for path in [
                &p.home,
                &p.data,
                &p.apps,
                &p.downloads,
                &p.state_file,
                &p.bin,
                &p.xdg_data,
                &p.xdg_config,
                &p.mac_applications,
                &p.launch_agents,
                &p.start_menu,
                &p.sys_root,
            ] {
                assert!(path.starts_with(root), "{path:?}");
            }
        }
        let l = Paths::rooted(root, Os::Linux);
        assert_eq!(l.apps, Path::new("/r/home/.local/share/stockroom/apps"));
        let w = Paths::rooted(root, Os::Windows);
        assert_eq!(w.apps, Path::new("/r/home/AppData/Local/Programs/Stockroom"));
    }

    #[test]
    fn expand_home_refuses_escapes() {
        let p = Paths::rooted(Path::new("/r"), Os::Linux);
        assert_eq!(p.expand_home("~/.config/harbor"), Some(PathBuf::from("/r/home/.config/harbor")));
        assert_eq!(p.expand_home("~/../x"), None);
        assert_eq!(p.expand_home("/etc"), None);
        assert_eq!(p.expand_home("~//x"), None);
    }
}
