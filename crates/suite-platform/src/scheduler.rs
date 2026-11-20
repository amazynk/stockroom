//! Registering the daily background update with the OS scheduler (PLAN.md §5): a systemd
//! user timer (XDG autostart without systemd), a `LaunchAgent`, or a Task Scheduler task.
//! No resident process. Files go under [`Paths`](crate::Paths); commands go through the
//! [`Runner`](crate::Runner), so tests never touch the real scheduler.

use std::{
    hash::{BuildHasher, RandomState},
    io::Write,
    path::{Path, PathBuf},
};

use suite_catalog::Os;
use suite_identity::{NAME, UPDATE_JOB};

use crate::{Env, PlatformError, runner::args};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerKind {
    SystemdUser,
    XdgAutostart,
    LaunchAgent,
    TaskScheduler,
}

impl SchedulerKind {
    pub fn describe(self) -> &'static str {
        match self {
            Self::SystemdUser => "systemd user timer",
            Self::XdgAutostart => "XDG autostart entry",
            Self::LaunchAgent => "LaunchAgent",
            Self::TaskScheduler => "Task Scheduler task",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleStatus {
    pub kind: SchedulerKind,
    pub enabled: bool,
    /// The file that holds the registration, if any.
    pub file: Option<PathBuf>,
}

/// Windows task name.
pub fn task_name() -> String {
    format!(r"{NAME}\Update")
}

#[derive(Debug)]
pub struct Scheduler<'a> {
    env: &'a Env,
}

impl<'a> Scheduler<'a> {
    pub fn new(env: &'a Env) -> Self {
        Self { env }
    }

    pub fn kind(&self) -> SchedulerKind {
        match self.env.os {
            Os::Linux if self.env.paths.sys("run/systemd/system").is_dir() => SchedulerKind::SystemdUser,
            Os::Linux => SchedulerKind::XdgAutostart,
            Os::Macos => SchedulerKind::LaunchAgent,
            Os::Windows => SchedulerKind::TaskScheduler,
        }
    }

    fn systemd_dir(&self) -> PathBuf {
        self.env.paths.xdg_config.join("systemd").join("user")
    }

    fn timer_file(&self) -> PathBuf {
        self.systemd_dir().join(format!("{UPDATE_JOB}.timer"))
    }

    fn service_file(&self) -> PathBuf {
        self.systemd_dir().join(format!("{UPDATE_JOB}.service"))
    }

    fn autostart_file(&self) -> PathBuf {
        self.env.paths.xdg_config.join("autostart").join(format!("{UPDATE_JOB}.desktop"))
    }

    fn launch_agent_file(&self) -> PathBuf {
        self.env.paths.launch_agents.join(format!("{UPDATE_JOB}.plist"))
    }

    pub fn status(&self) -> ScheduleStatus {
        let kind = self.kind();
        let (enabled, file) = match kind {
            SchedulerKind::SystemdUser => {
                let f = self.timer_file();
                (f.is_file(), Some(f))
            }
            SchedulerKind::XdgAutostart => {
                let f = self.autostart_file();
                (f.is_file(), Some(f))
            }
            SchedulerKind::LaunchAgent => {
                let f = self.launch_agent_file();
                (f.is_file(), Some(f))
            }
            SchedulerKind::TaskScheduler => {
                let out = self.env.runner.run("schtasks", &args(["/Query", "/TN", &task_name()]));
                (out.is_ok_and(|o| o.success), None)
            }
        };
        ScheduleStatus { kind, enabled, file }
    }

    /// Registers `exe update --background` to run daily with jitter.
    pub fn enable(&self, exe: &Path) -> Result<SchedulerKind, PlatformError> {
        let kind = self.kind();
        match kind {
            SchedulerKind::SystemdUser => {
                write_file(&self.service_file(), &systemd_service(exe))?;
                write_file(&self.timer_file(), &systemd_timer())?;
                self.cmd("systemctl", &["--user", "daemon-reload"])?;
                self.cmd("systemctl", &["--user", "enable", "--now", &format!("{UPDATE_JOB}.timer")])?;
            }
            SchedulerKind::XdgAutostart => write_file(&self.autostart_file(), &autostart_entry(exe))?,
            SchedulerKind::LaunchAgent => {
                let (hour, minute) = jitter_time();
                let f = self.launch_agent_file();
                write_file(&f, &launch_agent_plist(exe, hour, minute))?;
                // Reload so a changed path takes effect; unloading a missing job just fails.
                let _ = self.env.runner.run("launchctl", &args([Path::new("unload"), &f]));
                self.cmd_os("launchctl", &args([Path::new("load"), Path::new("-w"), &f]))?;
            }
            SchedulerKind::TaskScheduler => {
                let (hour, minute) = jitter_time();
                let tr = format!("\"{}\" update --background", exe.display());
                self.cmd(
                    "schtasks",
                    &[
                        "/Create",
                        "/F",
                        "/TN",
                        &task_name(),
                        "/SC",
                        "DAILY",
                        "/ST",
                        &format!("{hour:02}:{minute:02}"),
                        "/TR",
                        &tr,
                    ],
                )?;
            }
        }
        Ok(kind)
    }

    /// Removes the registration. Missing registrations are fine.
    pub fn disable(&self) -> Result<(), PlatformError> {
        match self.kind() {
            SchedulerKind::SystemdUser => {
                if self.timer_file().exists() {
                    let _ = self
                        .cmd("systemctl", &["--user", "disable", "--now", &format!("{UPDATE_JOB}.timer")]);
                }
                remove_file(&self.timer_file())?;
                remove_file(&self.service_file())?;
                let _ = self.cmd("systemctl", &["--user", "daemon-reload"]);
                // An autostart entry from a time without systemd goes too.
                remove_file(&self.autostart_file())?;
            }
            SchedulerKind::XdgAutostart => remove_file(&self.autostart_file())?,
            SchedulerKind::LaunchAgent => {
                let f = self.launch_agent_file();
                if f.exists() {
                    let _ =
                        self.env.runner.run("launchctl", &args([Path::new("unload"), Path::new("-w"), &f]));
                }
                remove_file(&f)?;
            }
            SchedulerKind::TaskScheduler => {
                let out = self.env.runner.run("schtasks", &args(["/Delete", "/F", "/TN", &task_name()]));
                if let Ok(o) = out
                    && !o.success
                {
                    tracing::debug!(stderr = %o.stderr, "no scheduled task to delete");
                }
            }
        }
        Ok(())
    }

    fn cmd(&self, program: &str, a: &[&str]) -> Result<(), PlatformError> {
        self.cmd_os(program, &args(a))
    }

    fn cmd_os(&self, program: &str, a: &[std::ffi::OsString]) -> Result<(), PlatformError> {
        let out = self
            .env
            .runner
            .run(program, a)
            .map_err(|e| PlatformError::Command(program.into(), e.to_string()))?;
        if out.success {
            Ok(())
        } else {
            Err(PlatformError::Command(program.into(), out.stderr.trim().to_owned()))
        }
    }
}

/// A random time of day, so that not every copy of Stockroom asks the server at once.
fn jitter_time() -> (u32, u32) {
    let r = RandomState::new().hash_one(std::time::SystemTime::now());
    let minutes = u32::try_from(r % (24 * 60)).unwrap_or(0);
    (minutes / 60, minutes % 60)
}

/// Quotes a path for a systemd `ExecStart=` line.
fn systemd_quote(p: &Path) -> String {
    let s = p.display().to_string().replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%");
    format!("\"{s}\"")
}

fn systemd_service(exe: &Path) -> String {
    format!(
        "[Unit]\nDescription={NAME}: install app updates\n\n[Service]\nType=oneshot\n\
         ExecStart={} update --background\nNice=10\nIOSchedulingClass=idle\n",
        systemd_quote(exe)
    )
}

fn systemd_timer() -> String {
    format!(
        "[Unit]\nDescription={NAME}: daily update check\n\n[Timer]\nOnCalendar=daily\n\
         RandomizedDelaySec=6h\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"
    )
}

/// Quotes an argument for a desktop entry `Exec=` key.
pub fn desktop_exec_quote(s: &str) -> String {
    let escaped: String = s
        .chars()
        .flat_map(|c| match c {
            '"' | '`' | '$' | '\\' => vec!['\\', c],
            '%' => vec!['%', '%'],
            c => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

fn autostart_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={NAME} updates\nExec={} update --background\n\
         NoDisplay=true\nX-GNOME-Autostart-Delay=120\n",
        desktop_exec_quote(&exe.display().to_string())
    )
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn launch_agent_plist(exe: &Path, hour: u32, minute: u32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{UPDATE_JOB}</string>
    <key>ProgramArguments</key>
    <array><string>{}</string><string>update</string><string>--background</string></array>
    <key>StartCalendarInterval</key>
    <dict><key>Hour</key><integer>{hour}</integer><key>Minute</key><integer>{minute}</integer></dict>
    <key>ProcessType</key><string>Background</string>
    <key>LowPriorityIO</key><true/>
</dict>
</plist>
"#,
        xml_escape(&exe.display().to_string())
    )
}

fn write_file(path: &Path, contents: &str) -> Result<(), PlatformError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| PlatformError::io(dir, e))?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| PlatformError::io(&tmp, e))?;
    f.write_all(contents.as_bytes()).map_err(|e| PlatformError::io(&tmp, e))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| PlatformError::io(path, e))
}

fn remove_file(path: &Path) -> Result<(), PlatformError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(PlatformError::io(path, e)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::runner::{FakeRunner, Output};

    fn env(os: Os, systemd: bool) -> (tempfile::TempDir, Env, Arc<FakeRunner>) {
        let dir = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::succeed_all());
        let mut env = Env::for_tests(dir.path(), os);
        env.runner = runner.clone();
        if systemd {
            std::fs::create_dir_all(env.paths.sys("run/systemd/system")).unwrap();
        }
        (dir, env, runner)
    }

    #[test]
    fn systemd_timer_on_and_off() {
        let (_d, env, runner) = env(Os::Linux, true);
        let s = Scheduler::new(&env);
        assert_eq!(s.kind(), SchedulerKind::SystemdUser);
        assert!(!s.status().enabled);
        s.enable(Path::new("/opt/my apps/stockroom")).unwrap();
        let st = s.status();
        assert!(st.enabled);
        assert!(st.file.unwrap().starts_with(&env.paths.home));
        let service = std::fs::read_to_string(s.service_file()).unwrap();
        assert!(service.contains("ExecStart=\"/opt/my apps/stockroom\" update --background"));
        assert!(runner.ran(&["systemctl", "--user", "enable", "--now"]));
        s.disable().unwrap();
        assert!(!s.status().enabled);
        assert!(!s.service_file().exists());
        assert!(runner.ran(&["systemctl", "--user", "disable", "--now"]));
    }

    #[test]
    fn xdg_autostart_without_systemd() {
        let (_d, env, runner) = env(Os::Linux, false);
        let s = Scheduler::new(&env);
        assert_eq!(s.kind(), SchedulerKind::XdgAutostart);
        s.enable(Path::new("/home/u/.local/bin/stockroom")).unwrap();
        let entry = std::fs::read_to_string(s.autostart_file()).unwrap();
        assert!(entry.contains("Exec=\"/home/u/.local/bin/stockroom\" update --background"));
        assert_eq!(runner.calls(), Vec::<Vec<String>>::new());
        s.disable().unwrap();
        assert!(!s.status().enabled);
    }

    #[test]
    fn launch_agent() {
        let (_d, env, runner) = env(Os::Macos, false);
        let s = Scheduler::new(&env);
        s.enable(Path::new("/Users/u/Applications/Stockroom & Co.app/Contents/MacOS/stockroom")).unwrap();
        let f = s.launch_agent_file();
        let v = plist::Value::from_file(&f).unwrap();
        let d = v.as_dictionary().unwrap();
        assert_eq!(d.get("Label").unwrap().as_string(), Some(UPDATE_JOB));
        let prog = d.get("ProgramArguments").unwrap().as_array().unwrap();
        assert!(prog[0].as_string().unwrap().ends_with("Stockroom & Co.app/Contents/MacOS/stockroom"));
        assert!(runner.ran(&["launchctl", "load", "-w"]));
        s.disable().unwrap();
        assert!(!f.exists());
    }

    #[test]
    fn schtasks() {
        let (_d, env, runner) = env(Os::Windows, false);
        runner.on(&["schtasks", "/Query"], Output::failed(1, "not found"));
        let s = Scheduler::new(&env);
        assert!(!s.status().enabled);
        s.enable(Path::new(r"C:\Users\u\AppData\Local\Programs\Stockroom\stockroom.exe")).unwrap();
        let create =
            runner.calls().into_iter().find(|c| c.get(1).map(String::as_str) == Some("/Create")).unwrap();
        assert!(create.contains(&r"Stockroom\Update".to_owned()));
        assert!(create.last().unwrap().ends_with("stockroom.exe\" update --background"));
        runner.on(&["schtasks", "/Query"], Output::ok("Stockroom\\Update Ready"));
        assert!(s.status().enabled);
        s.disable().unwrap();
        assert!(runner.ran(&["schtasks", "/Delete", "/F"]));
    }

    #[test]
    fn quoting() {
        assert_eq!(desktop_exec_quote("a \"b\" 100%"), "\"a \\\"b\\\" 100%%\"");
        assert_eq!(systemd_quote(Path::new("/a%b")), "\"/a%%b\"");
        let (h, m) = jitter_time();
        assert!(h < 24 && m < 60);
    }
}
