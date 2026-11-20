//! `stockroom`: install, update and remove the family's apps from the command line; the
//! scheduled background run; and (without a command) the app window.
//!
//! On Windows `stockroom.exe` is a windowed program, so a download, the Start menu or the
//! scheduled check never opens a console; typed in a terminal, `stockroom` runs `stockroom.com`
//! (`crates/suite-console`), which runs this with the terminal's input and output.

#![windows_subsystem = "windows"]

use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Instant,
};

use clap::{Parser, Subcommand, ValueEnum};
use suite_catalog::{AppStatus, Catalog, Channel, NotInstallable};
use suite_core::{CatalogSource, Core, CoreEvent, Job, JobOutcome, Level, ListOrigin, Queue};
use suite_fetch::Fetcher;
use suite_install::{Failpoint, InstallKind, Installer, Outcome, Policy, Stage};
use suite_platform::{Env, Scheduler};

/// The launcher shim and `stockroom.com` that a Windows release carries inside itself, so a
/// single downloaded `.exe` leaves a complete Stockroom behind (`build.rs`); empty otherwise.
const CARRIED_SHIM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/carried-shim"));
const CARRIED_CONSOLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/carried-console"));

/// Installs, updates and removes the family's apps.
#[derive(Debug, Parser)]
#[command(name = "stockroom", version, about)]
struct Cli {
    /// Keep everything under this folder (testing and development): the home, the apps,
    /// the state, the registry. Nothing outside it is changed.
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,
    /// With --root: follow this system's conventions instead of this computer's. Its
    /// integration then goes through stand-ins (a file for the registry and for shortcuts).
    #[arg(long, global = true, value_enum, value_name = "OS", requires = "root", hide = true)]
    os: Option<OsArg>,
    /// Catalog URL or file.
    #[arg(long, global = true, value_name = "URL|FILE")]
    catalog: Option<String>,
    /// More logging (repeat for more).
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// The family's apps: what's installed, what's available, what this computer can run.
    List,
    /// Install an app.
    Install {
        app: String,
        #[arg(long, value_enum)]
        channel: Option<ChannelArg>,
        /// Show a small window with the progress, then Open (for the family's apps to hand
        /// installs to Stockroom).
        #[arg(long)]
        ui: bool,
    },
    /// Update one app, or all of them.
    Update {
        app: Option<String>,
        #[arg(long, conflicts_with = "app")]
        all: bool,
        /// The scheduled run: at most daily, skips metered networks, follows policies.
        #[arg(long, conflicts_with_all = ["app", "all"])]
        background: bool,
        /// With --background: check now, whatever the schedule and network say.
        #[arg(long, requires = "background")]
        force: bool,
    },
    /// Remove an app Stockroom installed.
    Remove {
        app: String,
        /// Also remove the app's settings and data.
        #[arg(long)]
        purge: bool,
    },
    /// Go back to the previous version.
    Rollback { app: String },
    /// Installed apps, updates and the update schedule.
    Status,
    /// Check the platform, scheduler state and catalog.
    Doctor,
    /// Turn the daily background update on or off.
    Schedule {
        #[arg(value_enum)]
        state: OnOff,
    },
    /// Offer an app for its file types in "Open with" (Windows; per user, never taking
    /// over the default app). Off by default.
    Files {
        app: String,
        #[arg(value_enum)]
        state: OnOff,
    },
    /// Take Stockroom itself off this computer: the daily check, its own copy and menu
    /// entry. The apps it installed stay (remove those first with `remove`).
    Uninstall,
    /// How updates for an app are applied.
    Policy {
        app: String,
        #[arg(value_enum)]
        policy: PolicyArg,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ChannelArg {
    Stable,
    Beta,
}

impl ChannelArg {
    fn channel(self) -> Channel {
        match self {
            Self::Stable => Channel::Stable,
            Self::Beta => Channel::Beta,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OsArg {
    Linux,
    Macos,
    Windows,
}

impl OsArg {
    fn os(self) -> suite_catalog::Os {
        match self {
            Self::Linux => suite_catalog::Os::Linux,
            Self::Macos => suite_catalog::Os::Macos,
            Self::Windows => suite_catalog::Os::Windows,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PolicyArg {
    Auto,
    Ask,
    Never,
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("STOCKROOM_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
        )
        .with_writer(io::stderr)
        .without_time()
        .init();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Hands what this build carries inside itself to the code that keeps Stockroom's copy.
fn carry() {
    if !CARRIED_SHIM.is_empty() && !CARRIED_CONSOLE.is_empty() {
        suite_install::settle::carry(vec![
            (suite_install::integrate::shim_file_name(), CARRIED_SHIM),
            (format!("{}.com", suite_identity::SLUG), CARRIED_CONSOLE),
        ]);
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let started = Instant::now();
    carry();
    let env = match &cli.root {
        Some(root) => {
            std::fs::create_dir_all(root)?;
            let os = cli.os.map_or_else(suite_catalog::Os::current, OsArg::os);
            Env::rooted_as(&std::fs::canonicalize(root)?, os)?
        }
        None => Env::real()?,
    };
    let source = cli.catalog.as_deref().map_or_else(CatalogSource::default_url, CatalogSource::parse);
    let fetcher = Fetcher::default();
    let mut installer = Installer::new(env.clone(), fetcher.clone());
    // Crash tests: debug builds can be told to abort at a point of an install.
    if cfg!(debug_assertions)
        && let Ok(fp) = std::env::var("STOCKROOM_FAILPOINT")
    {
        installer = installer.with_failpoint(Some(fp.parse::<Failpoint>()?), true);
    }
    let mut core = Core::new(env, fetcher, source).with_installer(installer);
    // The real catalog has a stand-in for when it can't be had: the list this build came
    // with. A catalog named with --catalog is used as given, or not at all.
    if cli.catalog.is_none() {
        core = core.with_builtin(suite_core::BUILTIN_CATALOG);
    }
    let core = Arc::new(core);
    let ui = |single_app: Option<suite_ui::SingleApp>| -> Result<ExitCode> {
        suite_ui::run(suite_ui::UiOptions { core: Arc::clone(&core), single_app, started })?;
        Ok(ExitCode::SUCCESS)
    };
    let command = match cli.command {
        // The window repairs leftovers itself, after its first frame. A copy named after an
        // app ("Install Specular", `specular-setup`) opens on that app's install.
        None => {
            let known: Vec<String> =
                core.offline_catalog().map_or_default(|(c, _)| c.apps.into_iter().map(|a| a.id).collect());
            let preset = std::env::current_exe().ok().and_then(|exe| preset_app(&exe, &known));
            return ui(preset.map(|app| suite_ui::SingleApp { app, channel: None }));
        }
        Some(Command::Install { app, channel, ui: true }) => {
            return ui(Some(suite_ui::SingleApp { app, channel: channel.map(ChannelArg::channel) }));
        }
        Some(command) => command,
    };
    // Repair whatever an interrupted run left behind before showing or changing anything.
    match core.installer().recover() {
        Ok(fixed) => fixed.iter().for_each(|f| tracing::info!("repaired: {f}")),
        Err(suite_install::InstallError::Busy) => {}
        Err(e) => tracing::warn!("recovery: {e}"),
    }
    let mut out = io::stdout().lock();

    match command {
        Command::List => list(&core, &mut out),
        Command::Install { app, channel, .. } => {
            run_job(&core, Job::Install { app, channel: channel.map(ChannelArg::channel) }, &mut out)
        }
        Command::Update { background: true, force, .. } => background(&core, force, &mut out),
        Command::Update { app: Some(app), .. } => run_job(&core, Job::Update { app }, &mut out),
        Command::Update { app: None, .. } => update_all(&core, &mut out),
        Command::Remove { app, purge } => run_job(&core, Job::Remove { app, purge }, &mut out),
        Command::Rollback { app } => {
            let o = core.rollback(&app)?;
            report_outcome(&o, &mut out)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Status => status(&core, &mut out),
        Command::Doctor => doctor(&core, &mut out),
        Command::Schedule { state } => schedule(&core, state, &mut out),
        Command::Files { app, state } => {
            let types = core.installer().set_file_associations(&app, matches!(state, OnOff::On))?;
            if types.is_empty() {
                writeln!(out, "{app}: no longer offered for its file types")?;
            } else {
                writeln!(out, "{app}: offered in \"Open with\" for .{}", types.join(", ."))?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Uninstall => {
            let apps = core.state()?.apps.len();
            for path in core.uninstall_suite()? {
                writeln!(out, "removed {}", path.display())?;
            }
            writeln!(out, "the daily check is off and Stockroom's own files are gone")?;
            if apps > 0 {
                writeln!(
                    out,
                    "{apps} app(s) it installed are still here and keep working; start Stockroom again to remove them"
                )?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Policy { app, policy } => {
            let p = match policy {
                PolicyArg::Auto => Policy::Auto,
                PolicyArg::Ask => Policy::Ask,
                PolicyArg::Never => Policy::Never,
            };
            core.set_policy(&app, p)?;
            writeln!(out, "{app}: updates {}", describe_policy(p))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// The app a copy of Stockroom is for, going by the name it was started under: the first
/// known app ID among the words of the file name (`specular-setup`, `Specular-Installer
/// (1).exe`) or, in a macOS bundle, of the bundle's (`Install Specular.app`). This is how one
/// build is handed out with every app: named for it, it installs that app first and offers
/// the others beside it.
fn preset_app(exe: &Path, known: &[String]) -> Option<String> {
    let bundle = exe.ancestors().find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")));
    [exe.file_stem(), bundle.and_then(Path::file_stem)].into_iter().flatten().find_map(|name| {
        let name = name.to_string_lossy().to_lowercase();
        let words: Vec<&str> =
            name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
        known
            .iter()
            // Its own name is never an app to install, even once Stockroom lists itself.
            .filter(|id| id.as_str() != suite_identity::SLUG)
            .find(|id| {
                let parts: Vec<&str> = id.split('-').collect();
                words.windows(parts.len()).any(|w| w == parts)
            })
            .cloned()
    })
}

/// How an app was installed, as `status` says it.
fn describe_kind(k: InstallKind) -> &'static str {
    match k {
        InstallKind::Archive => "archive",
        InstallKind::AppImage => "AppImage",
        InstallKind::Flatpak => "Flatpak",
        InstallKind::MacBundle => "Mac app",
    }
}

fn describe_policy(p: Policy) -> &'static str {
    match p {
        Policy::Auto => "install automatically",
        Policy::Ask => "wait for you",
        Policy::Never => "are not installed",
    }
}

/// The app list to work with. When it isn't the server's, says so on stderr.
fn app_list(core: &Core) -> Result<Catalog> {
    let list = core.catalog()?;
    if let Some((_, why)) = &list.problem {
        let which = match list.origin {
            ListOrigin::BuiltIn => "the one this version came with",
            ListOrigin::Saved | ListOrigin::Server => "the one saved earlier",
        };
        let _ = writeln!(
            io::stderr(),
            "note: the newest app list couldn't be had ({why}); using {which}, from {}",
            list.catalog.published.date()
        );
    }
    Ok(list.catalog)
}

fn list(core: &Core, out: &mut dyn Write) -> Result<ExitCode> {
    let catalog = app_list(core)?;
    let rows = core.rows(&catalog)?;
    let states: Vec<String> = rows
        .iter()
        .map(|row| match (&row.installed, &row.available, row.app.status) {
            (_, _, AppStatus::Unreleased) => "coming soon".to_owned(),
            (Some(i), _, _) => match row.update_available() {
                Some(v) => format!("installed {} (update {v})", i.current),
                None => format!("installed {}", i.current),
            },
            (None, Ok(v), _) => match row.via_store {
                Some(suite_catalog::Store::Website) => "on its website".to_owned(),
                Some(store) => format!("in the {}", store.name()),
                None => format!("available {v}"),
            },
            (None, Err(NotInstallable::Unsupported(_)), _) => "not for this computer".to_owned(),
            (None, Err(e), _) => e.to_string(),
        })
        .collect();
    let width = |cells: &mut dyn Iterator<Item = usize>| cells.max().unwrap_or(0);
    let id_w = width(&mut rows.iter().map(|r| r.app.id.len()));
    let name_w = width(&mut rows.iter().map(|r| r.app.name.chars().count()));
    let state_w = width(&mut states.iter().map(String::len));
    for (row, state) in rows.iter().zip(&states) {
        let by = row.app.vendor.as_deref().map_or_else(String::new, |v| format!(" (by {v})"));
        writeln!(
            out,
            "{:<id_w$}  {:<name_w$}  {state:<state_w$}  {}{by}",
            row.app.id, row.app.name, row.app.pitch
        )?;
        if let Err(why @ NotInstallable::Unsupported(_)) = &row.available {
            writeln!(out, "{:<id_w$}  {why}", "")?;
        }
        for f in &row.others {
            let v = f.version.as_deref().map_or_default(|v| format!(" {v}"));
            let at = f.location.as_ref().map_or_default(|l| format!(" at {}", l.display()));
            writeln!(out, "{:<id_w$}  also installed by {}{v}{at} (updated by its source)", "", f.source)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs one job through the queue, showing its progress on stderr.
fn run_job(core: &Arc<Core>, job: Job, out: &mut dyn Write) -> Result<ExitCode> {
    let catalog = Arc::new(app_list(core)?);
    let mut queue = Queue::start(core.clone(), catalog);
    let id = queue.submit(job);
    let mut progress = ProgressLine::new();
    let end = queue.wait(id, &mut |ev| progress.event(ev));
    queue.finish();
    match end {
        Some(CoreEvent::Finished { outcome, .. }) => {
            match outcome {
                JobOutcome::Installed(o) => report_outcome(&o, out)?,
                JobOutcome::UpToDate => writeln!(out, "already up to date")?,
                JobOutcome::Removed { purged } => {
                    writeln!(out, "removed")?;
                    for p in purged {
                        writeln!(out, "  deleted {}", p.display())?;
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(CoreEvent::Failed { error, .. }) => Err(error.into()),
        _ => Err("the job ended without a result".into()),
    }
}

/// Download progress on stderr: on a terminal one line redrawn in place (share, size, speed,
/// time left); in a log or a pipe a line for every tenth.
#[derive(Debug)]
struct ProgressLine {
    terminal: bool,
    /// When the first progress report came, and how far the download was then (a resumed
    /// download doesn't start at zero).
    start: Option<(Instant, u64)>,
    drawn: Option<Instant>,
    tenth: Option<u64>,
    /// A redrawn line is on screen and needs ending before anything else is written.
    open: bool,
}

impl ProgressLine {
    fn new() -> Self {
        Self { terminal: io::stderr().is_terminal(), start: None, drawn: None, tenth: None, open: false }
    }

    fn end_line(&mut self, err: &mut dyn Write) {
        if std::mem::take(&mut self.open) {
            let _ = writeln!(err);
        }
    }

    fn event(&mut self, ev: &CoreEvent) {
        let mut err = io::stderr().lock();
        match ev {
            CoreEvent::Stage { stage, .. } => {
                self.end_line(&mut err);
                (self.start, self.drawn, self.tenth) = (None, None, None);
                let what = match stage {
                    Stage::Download => "downloading",
                    Stage::Unpack => "unpacking",
                    Stage::Switch => "switching",
                    Stage::Integrate => "adding shortcuts",
                    Stage::Cleanup => "cleaning up",
                };
                let _ = writeln!(err, "{what}...");
            }
            CoreEvent::Progress { done, total, .. } if *total > 0 => {
                let now = Instant::now();
                let (since, from) = *self.start.get_or_insert((now, *done));
                let finished = done >= total;
                if self.terminal {
                    let due = self.drawn.is_none_or(|t| now - t >= suite_core::PROGRESS_INTERVAL);
                    if due || finished {
                        self.drawn = Some(now);
                        let pace = pace(done.saturating_sub(from), *total - (*done).min(*total), now - since);
                        // Carriage return and "erase line": the line is redrawn in place.
                        let _ = write!(
                            err,
                            "\r\x1b[2K  {:>3}%  {:.1} of {:.1} MB{pace}",
                            done * 100 / total,
                            mb(*done),
                            mb(*total)
                        );
                        let _ = err.flush();
                        self.open = true;
                    }
                    if finished {
                        self.end_line(&mut err);
                    }
                } else {
                    let tenth = done * 10 / total;
                    if self.tenth != Some(tenth) {
                        self.tenth = Some(tenth);
                        let _ = writeln!(err, "  {:>3}% of {:.1} MB", tenth * 10, mb(*total));
                    }
                }
            }
            CoreEvent::Finished { .. } | CoreEvent::Failed { .. } => self.end_line(&mut err),
            _ => {}
        }
    }
}

/// Speed and time left once there is something to measure: `  8.2 MB/s  12 s left`.
fn pace(got: u64, left: u64, over: std::time::Duration) -> String {
    let secs = over.as_secs_f64();
    if secs < 1.0 || got == 0 {
        return String::new();
    }
    let speed = mb(got) / secs;
    let remaining = mb(left) / speed;
    let left = if remaining < 90.0 {
        format!("{remaining:.0} s left")
    } else {
        format!("{:.0} min left", remaining / 60.0)
    };
    format!("  {speed:.1} MB/s  {left}")
}

#[allow(clippy::cast_precision_loss)]
fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1e6
}

fn report_outcome(o: &Outcome, out: &mut dyn Write) -> io::Result<()> {
    if let Some(store) = o.opened_store {
        match store {
            suite_catalog::Store::Website => {
                writeln!(out, "opened the website of {}; it has the steps to install it", o.app)?;
            }
            _ => writeln!(
                out,
                "opened {} in the {}; the store installs it and keeps it up to date",
                o.app,
                store.name()
            )?,
        }
    } else if o.unchanged {
        writeln!(out, "{} {} is already installed", o.app, o.to)?;
    } else {
        match &o.from {
            Some(from) => writeln!(out, "{}: {from} -> {}", o.app, o.to)?,
            None => writeln!(out, "{} {} installed", o.app, o.to)?,
        }
    }
    if o.needs_restart {
        writeln!(out, "{} is running; the new version starts next time you open it", o.app)?;
    }
    Ok(())
}

fn update_all(core: &Arc<Core>, out: &mut dyn Write) -> Result<ExitCode> {
    let state = core.state()?;
    if state.apps.is_empty() {
        writeln!(out, "nothing installed")?;
        return Ok(ExitCode::SUCCESS);
    }
    let catalog = Arc::new(app_list(core)?);
    let mut queue = Queue::start(core.clone(), catalog);
    let ids: Vec<(u64, String)> =
        state.apps.keys().map(|a| (queue.submit(Job::Update { app: a.clone() }), a.clone())).collect();
    let mut failed = false;
    let mut progress = ProgressLine::new();
    for (id, app) in ids {
        match queue.wait(id, &mut |ev| progress.event(ev)) {
            Some(CoreEvent::Finished { outcome: JobOutcome::Installed(o), .. }) => report_outcome(&o, out)?,
            Some(CoreEvent::Finished { .. }) => writeln!(out, "{app}: up to date")?,
            Some(CoreEvent::Failed { error, .. }) => {
                failed = true;
                writeln!(out, "{app}: {error}")?;
            }
            _ => {}
        }
    }
    queue.finish();
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn background(core: &Core, force: bool, out: &mut dyn Write) -> Result<ExitCode> {
    let r = core.background_update(force)?;
    if let Some(why) = r.skipped {
        writeln!(out, "skipped: {why}")?;
        return Ok(ExitCode::SUCCESS);
    }
    for o in &r.applied {
        report_outcome(o, out)?;
    }
    for (app, v) in &r.waiting {
        writeln!(out, "{app}: {v} is ready; it waits for you (`stockroom update {app}`)")?;
    }
    for (app, e) in &r.failed {
        writeln!(out, "{app}: {e}")?;
    }
    if r.applied.is_empty() && r.waiting.is_empty() && r.failed.is_empty() {
        writeln!(out, "everything is up to date")?;
    }
    Ok(if r.failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn status(core: &Core, out: &mut dyn Write) -> Result<ExitCode> {
    let state = core.state()?;
    let catalog: Option<Catalog> = match app_list(core) {
        Ok(c) => Some(c),
        Err(e) => {
            writeln!(out, "(catalog unavailable: {e})")?;
            None
        }
    };
    if state.apps.is_empty() {
        writeln!(out, "no apps installed by Stockroom")?;
    }
    for (id, a) in &state.apps {
        let update = catalog
            .as_ref()
            .and_then(|c| core.plan(c, id, a.channel).ok())
            .filter(|(_, r, _)| r.version > a.current)
            .map_or_default(|(_, r, _)| format!(", update {} available", r.version));
        let prev = a.previous.as_ref().map_or_default(|p| format!(", can roll back to {p}"));
        writeln!(
            out,
            "{id:<10} {} ({}, {} channel, updates {}){prev}{update}",
            a.current,
            describe_kind(a.kind),
            a.channel,
            describe_policy(a.policy)
        )?;
    }
    if let Some(c) = &catalog {
        for row in core.rows(c)? {
            for f in &row.others {
                let v = f.version.as_deref().map_or_default(|v| format!(" {v}"));
                writeln!(out, "{:<10} also installed by {}{v}", row.app.id, f.source)?;
            }
        }
    }
    let sched = Scheduler::new(core.env()).status();
    writeln!(
        out,
        "background updates: {} ({})",
        if sched.enabled && state.settings.auto_update { "on" } else { "off" },
        sched.kind.describe()
    )?;
    Ok(ExitCode::SUCCESS)
}

fn doctor(core: &Core, out: &mut dyn Write) -> Result<ExitCode> {
    let mut worst = Level::Ok;
    for c in core.doctor() {
        let tag = match c.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        writeln!(out, "{tag} {:<10} {}", c.name, c.detail)?;
        if c.level == Level::Fail {
            worst = Level::Fail;
        }
    }
    Ok(if worst == Level::Fail { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn schedule(core: &Core, state: OnOff, out: &mut dyn Write) -> Result<ExitCode> {
    let exe = core.settled_exe();
    match core.set_auto_update(matches!(state, OnOff::On), &exe)? {
        Some(kind) => writeln!(out, "background updates on ({})", kind.describe())?,
        None => writeln!(out, "background updates off")?,
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_named_after_an_app_is_for_that_app() {
        let known: Vec<String> = ["specular", "dozed", "my-app", "stockroom"].map(String::from).into();
        let preset = |exe: &str| preset_app(Path::new(exe), &known);
        assert_eq!(preset("/home/u/Downloads/specular-setup").as_deref(), Some("specular"));
        assert_eq!(preset(r"C:\Users\u\Downloads\Specular-Installer (1).exe").as_deref(), Some("specular"));
        assert_eq!(preset("/tmp/get_dozed.AppImage").as_deref(), Some("dozed"));
        assert_eq!(preset("/tmp/install-my-app-now").as_deref(), Some("my-app"));
        assert_eq!(
            preset("/Volumes/Specular/Install Specular.app/Contents/MacOS/stockroom").as_deref(),
            Some("specular")
        );
        // Its own name, a name that only contains an ID, and unknown names open the app list.
        assert_eq!(preset("/usr/local/bin/stockroom"), None);
        assert_eq!(preset("/Applications/Stockroom.app/Contents/MacOS/stockroom"), None);
        assert_eq!(preset("/tmp/spectacular"), None);
        assert_eq!(preset("/tmp/my-setup"), None);
        assert_eq!(preset_app(Path::new("/tmp/specular-setup"), &[]), None);
    }
}
