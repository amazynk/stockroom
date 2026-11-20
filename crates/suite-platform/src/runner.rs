//! Running external commands (`flatpak`, `systemctl`, `hdiutil`, `schtasks`, ...) behind a
//! trait, so tests script the answers and `--root` never changes the real system.

use std::{
    ffi::{OsStr, OsString},
    fmt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};

/// What a command printed and how it ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(stdout: impl Into<String>) -> Self {
        Self { success: true, code: Some(0), stdout: stdout.into(), stderr: String::new() }
    }

    pub fn failed(code: i32, stderr: impl Into<String>) -> Self {
        Self { success: false, code: Some(code), stdout: String::new(), stderr: stderr.into() }
    }
}

/// Runs a program with arguments and captures its output.
pub trait Runner: fmt::Debug + Send + Sync {
    fn run(&self, program: &str, args: &[OsString]) -> std::io::Result<Output>;

    /// Starts a program without waiting for it (opening an app or a link). Fakes record
    /// the call like [`Runner::run`].
    fn spawn(&self, program: &str, args: &[OsString]) -> std::io::Result<()> {
        let out = self.run(program, args)?;
        if out.success { Ok(()) } else { Err(std::io::Error::other(out.stderr)) }
    }
}

/// Helper to build an argument list from strings and paths.
pub fn args<I, S>(items: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    items.into_iter().map(|s| s.as_ref().to_owned()).collect()
}

/// Runs real commands.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[OsString]) -> std::io::Result<Output> {
        tracing::debug!(program, ?args, "run");
        let out = Command::new(program).args(args).stdin(Stdio::null()).output()?;
        Ok(Output {
            success: out.status.success(),
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn spawn(&self, program: &str, args: &[OsString]) -> std::io::Result<()> {
        tracing::debug!(program, ?args, "spawn");
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        // Reaped when it ends, so it never lingers as a zombie while Stockroom runs.
        std::thread::Builder::new().name("reap".into()).spawn(move || drop(child.wait()))?;
        Ok(())
    }
}

/// The runner of `--root`, where nothing outside the root may change: commands are recorded
/// and answered with success without running, except the macOS unpacking tools (`hdiutil`,
/// `ditto`), which run for real when every path they are given is inside the root. They only
/// mount an image there and copy out of it, so a disk image installs under a root as it
/// would for real.
#[derive(Debug)]
pub struct RootedRunner {
    root: PathBuf,
    dry: FakeRunner,
    /// Whether the unpacking tools exist here and are wanted (macOS following macOS).
    unpack_for_real: bool,
}

impl RootedRunner {
    pub fn new(root: &Path, unpack_for_real: bool) -> Self {
        Self { root: root.to_owned(), dry: FakeRunner::succeed_all(), unpack_for_real }
    }

    /// Every command line seen so far, program first, run or not.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.dry.calls()
    }

    fn confined(&self, program: &str, args: &[OsString]) -> bool {
        if !self.unpack_for_real || !matches!(program, "hdiutil" | "ditto") {
            return false;
        }
        let mut paths = args.iter().map(Path::new).filter(|p| p.is_absolute()).peekable();
        paths.peek().is_some() && paths.all(|p| p.starts_with(&self.root))
    }
}

impl Runner for RootedRunner {
    fn run(&self, program: &str, args: &[OsString]) -> std::io::Result<Output> {
        let dry = self.dry.run(program, args)?;
        if self.confined(program, args) { SystemRunner.run(program, args) } else { Ok(dry) }
    }
}

/// A scripted runner: records every call and answers from rules; unknown commands fail as
/// "not found". Used by tests and by `--root`, where nothing outside the root may change.
#[derive(Debug, Default)]
pub struct FakeRunner {
    rules: Mutex<Vec<Rule>>,
    calls: Mutex<Vec<Vec<String>>>,
}

/// A command's effect in a test: what it does to the (temporary) disk and what it answers.
pub type Handler = Box<dyn Fn(&[String]) -> Output + Send + Sync>;

struct Rule {
    /// Matches when the command line starts with these words.
    prefix: Vec<String>,
    answer: Answer,
}

enum Answer {
    Fixed(Output),
    Call(Handler),
}

impl fmt::Debug for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let answer = match &self.answer {
            Answer::Fixed(o) => format!("{o:?}"),
            Answer::Call(_) => "handler".to_owned(),
        };
        f.debug_struct("Rule").field("prefix", &self.prefix).field("answer", &answer).finish()
    }
}

impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// A runner that answers every command with success and no output (dry run).
    pub fn succeed_all() -> Self {
        let r = Self::new();
        r.on(&[], Output::ok(""));
        r
    }

    /// Answers command lines starting with `prefix` (program first). Later rules win.
    pub fn on(&self, prefix: &[&str], output: Output) {
        self.push(prefix, Answer::Fixed(output));
    }

    /// Answers command lines starting with `prefix` by calling `handler` with the whole
    /// line (program first), so a test can emulate a command's effect on its temporary
    /// folders (`hdiutil attach` filling a mount point, `ditto` copying a bundle).
    pub fn on_call(&self, prefix: &[&str], handler: impl Fn(&[String]) -> Output + Send + Sync + 'static) {
        self.push(prefix, Answer::Call(Box::new(handler)));
    }

    fn push(&self, prefix: &[&str], answer: Answer) {
        let rule = Rule { prefix: prefix.iter().map(|s| (*s).to_owned()).collect(), answer };
        lock(&self.rules).push(rule);
    }

    /// Every command line run so far, program first.
    pub fn calls(&self) -> Vec<Vec<String>> {
        lock(&self.calls).clone()
    }

    /// Whether a command line starting with `prefix` was run.
    pub fn ran(&self, prefix: &[&str]) -> bool {
        self.calls().iter().any(|c| c.len() >= prefix.len() && c.iter().zip(prefix).all(|(a, b)| a == b))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Runner for FakeRunner {
    fn run(&self, program: &str, args: &[OsString]) -> std::io::Result<Output> {
        let mut line = vec![program.to_owned()];
        line.extend(args.iter().map(|a| a.to_string_lossy().into_owned()));
        tracing::debug!(?line, "fake run");
        lock(&self.calls).push(line.clone());
        let rules = lock(&self.rules);
        let hit = rules
            .iter()
            .rev()
            .find(|r| r.prefix.len() <= line.len() && r.prefix.iter().zip(&line).all(|(a, b)| a == b));
        match hit.map(|r| &r.answer) {
            Some(Answer::Fixed(o)) => Ok(o.clone()),
            Some(Answer::Call(h)) => Ok(h(&line)),
            None => Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{program}: not found"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_runner_matches_longest_recent_rule() {
        let r = FakeRunner::new();
        r.on(&["flatpak"], Output::failed(1, "no"));
        r.on(&["flatpak", "info"], Output::ok("ID: x"));
        assert!(r.run("flatpak", &args(["info", "x"])).unwrap().success);
        assert!(!r.run("flatpak", &args(["list"])).unwrap().success);
        assert!(r.run("brew", &[]).is_err());
        assert!(r.ran(&["flatpak", "info"]));
        assert_eq!(r.calls().len(), 3);
        r.on_call(&["brew"], |line| Output::ok(line.join(" ")));
        assert_eq!(r.run("brew", &args(["list"])).unwrap().stdout, "brew list");
    }

    #[test]
    fn rooted_runner_only_unpacks_inside_its_root() {
        let r = RootedRunner::new(Path::new("/r"), true);
        assert!(r.confined("ditto", &args(["--noqtn", "/r/a/X.app", "/r/b/X.app"])));
        assert!(r.confined("hdiutil", &args(["detach", "/r/mount"])));
        assert!(!r.confined("ditto", &args(["--noqtn", "/r/a/X.app", "/Applications/X.app"])));
        assert!(!r.confined("ditto", &args(["--noqtn", "/rx/a", "/r/b"])));
        assert!(!r.confined("hdiutil", &args(["info"])));
        assert!(!r.confined("launchctl", &args(["load", "/r/a.plist"])));
        assert!(!RootedRunner::new(Path::new("/r"), false).confined("ditto", &args(["/r/a", "/r/b"])));
        // Everything else is recorded and answered, never run.
        assert!(r.run("launchctl", &args(["load", "-w", "/r/a.plist"])).unwrap().success);
        assert_eq!(r.calls(), [["launchctl", "load", "-w", "/r/a.plist"]]);
    }
}
