//! The whole window on fake family data, without a display (Slint's software renderer into
//! memory): the first frame comes quickly, an idle window draws nothing, and installs,
//! updates, settings and the compact install window go through `suite-core`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

#[path = "support/fake.rs"]
mod fake;

use std::time::{Duration, Instant};

use slint::{ComponentHandle, LogicalSize, Model};
use suite_ui::{App, ThemeChoice, headless};

/// CPU time from building the window to its first frame. Debug builds run the generated
/// UI code unoptimised.
const FIRST_FRAME: Duration =
    if cfg!(debug_assertions) { Duration::from_millis(600) } else { Duration::from_millis(120) };

/// Thread CPU time where the platform reports it, which machine load doesn't inflate.
fn cpu() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    Some(Duration::from_nanos(stat.split_whitespace().next()?.parse().ok()?))
}

/// Spins until `done` holds (or fails after `limit`).
fn until(windows: &headless::Windows, limit: Duration, what: &str, done: impl Fn() -> bool) {
    let end = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for {what}; {}", dump());
        headless::spin(windows, Duration::from_millis(20));
    }
}

thread_local! {
    static SHOWN: std::cell::RefCell<Option<suite_ui_markup::ui::AppWindow>> = const { std::cell::RefCell::new(None) };
}

/// What the window shows, for failure messages.
fn dump() -> String {
    SHOWN.with_borrow(|w| {
        w.as_ref().map_or_default(|ui| {
            let cards: Vec<String> =
                ui.get_family().iter().map(|c| format!("{}: {}", c.id, c.status)).collect();
            let jobs: Vec<String> = ui
                .get_jobs()
                .iter()
                .map(|j| format!("{} [{}] {} {}", j.title, j.state, j.message, j.error_detail))
                .collect();
            format!("cards {cards:?}, jobs {jobs:?}, notice {:?}", ui.get_notice())
        })
    })
}

fn watch(app: &App) {
    SHOWN.with_borrow_mut(|w| *w = Some(app.window().clone_strong()));
}

fn card_status(app: &App, id: &str) -> String {
    let ui = app.window();
    ui.get_family()
        .iter()
        .chain(ui.get_others().iter())
        .find(|c| c.id == id)
        .map_or_default(|c| c.status.to_string())
}

/// Slint's platform is set once per process, so the scenarios share one thread and one
/// headless platform.
#[test]
fn window() {
    let windows = headless::Headless::install().unwrap();
    first_frame_is_quick_and_idle_draws_nothing(&windows);
    installs_updates_and_settings_go_through_the_core(&windows);
    language_and_theme_switch_live(&windows);
}

fn first_frame_is_quick_and_idle_draws_nothing(windows: &headless::Windows) {
    let root = tempfile::tempdir().unwrap();
    let family = fake::family(root.path());
    let (wall, cpu_at) = (Instant::now(), cpu());
    let app = App::new(family.core.clone(), None).unwrap();
    app.window().window().set_size(LogicalSize::new(1120.0, 760.0));
    app.show().unwrap();
    assert!(windows.draw(), "no first frame");
    let took = cpu_at.zip(cpu()).map_or_else(|| wall.elapsed(), |(a, b)| b.saturating_sub(a));
    eprintln!("first frame: {took:?} CPU, {:?} wall", wall.elapsed());
    assert!(took < FIRST_FRAME, "first frame took {took:?} of CPU, over {FIRST_FRAME:?}");

    // The list loads after the first frame: the cached copy (without 1.2.0), then the
    // server's.
    until(windows, Duration::from_secs(20), "the app list", || app.window().get_updates().row_count() == 1);
    assert_eq!(app.window().get_others().row_count(), 1, "Zynk, after the family");
    assert_eq!(card_status(&app, "specular"), "Installed 1.1.0 · 1.2.0 is ready");
    assert_eq!(card_status(&app, "zynk"), "Installed by Flatpak");
    assert_eq!(app.window().get_updates().row_count(), 1);
    headless::spin(windows, Duration::from_millis(500));

    // Nothing moves: no frames at all for three seconds.
    let before = windows.frames();
    headless::spin(windows, Duration::from_secs(3));
    assert_eq!(windows.frames() - before, 0, "an idle window drew frames");

    // The check can see frames: a page change is drawn.
    app.show_page(3);
    headless::spin(windows, Duration::from_millis(100));
    assert!(windows.frames() > before, "a change wasn't drawn");
}

fn installs_updates_and_settings_go_through_the_core(windows: &headless::Windows) {
    let root = tempfile::tempdir().unwrap();
    let family = fake::family(root.path());
    let app = App::new(family.core.clone(), None).unwrap();
    watch(&app);
    app.show().unwrap();
    until(windows, Duration::from_secs(20), "the app list", || app.window().get_updates().row_count() == 1);
    let ui = app.window();

    // "Install all" installs the family's apps that aren't there yet (bindery only: Almanac
    // can't run here, Foundry isn't out) — one app isn't "all", so it's offered per card.
    assert_eq!(ui.get_install_all(), "");
    ui.invoke_act("bindery".into());
    until(windows, Duration::from_secs(30), "bindery installed", || {
        card_status(&app, "bindery").starts_with("Installed")
    });
    assert!(family.core.state().unwrap().apps.contains_key("bindery"));

    // Update all, then the history shows it.
    ui.invoke_update_all();
    until(windows, Duration::from_secs(30), "the update", || {
        card_status(&app, "specular") == "Installed · 1.2.0"
    });
    assert_eq!(ui.get_updates().row_count(), 0);
    let log: Vec<String> = ui.get_log().iter().map(|l| l.text.to_string()).collect();
    assert_eq!(log.first().map(String::as_str), Some("Updated Specular from 1.1.0 to 1.2.0"), "{log:?}");

    // The page: policy, rollback and remove (with its data).
    app.open_page("specular");
    assert_eq!(ui.get_app().rollback, "Go back to 1.1.0");
    ui.invoke_set_policy("specular".into(), 2);
    until(windows, Duration::from_secs(10), "the policy", || ui.get_app().policy == 2);
    ui.invoke_rollback("specular".into());
    until(windows, Duration::from_secs(30), "the rollback", || {
        card_status(&app, "specular").starts_with("Installed 1.1.0")
    });
    let data = family.core.env().paths.home.join(".config/specular");
    std::fs::create_dir_all(&data).unwrap();
    ui.invoke_remove("specular".into(), true);
    until(windows, Duration::from_secs(30), "the removal", || {
        !family.core.state().unwrap().apps.contains_key("specular")
    });
    assert!(!data.exists(), "settings and data went too");

    // A failure is plain: Zynk's pinned download moved on.
    ui.invoke_show_page(2);
    app.install_window("zynk", None).unwrap();
    until(windows, Duration::from_secs(30), "the failure", || {
        app.window().get_jobs().iter().any(|j| j.state == 3)
    });
    let failed = ui.get_jobs().iter().find(|j| j.state == 3).unwrap();
    assert!(failed.message.contains("released a new build"), "{}", failed.message);
    assert!(failed.can_retry);
    let compact = app.compact_window().unwrap();
    assert_eq!(compact.get_state(), 2);
    assert_eq!(compact.get_heading(), "Zynk wasn't installed");

    // Settings: automatic updates go through the (sandboxed) scheduler.
    assert!(ui.get_auto_update(), "on by default, and registered at start");
    assert!(suite_platform::Scheduler::new(family.core.env()).status().enabled);
    ui.invoke_set_auto_update(false);
    until(windows, Duration::from_secs(10), "automatic updates off", || !ui.get_auto_update());
    assert!(!suite_platform::Scheduler::new(family.core.env()).status().enabled);
    ui.invoke_set_skip_metered(false);
    until(windows, Duration::from_secs(10), "metered", || !ui.get_skip_metered());

    // Opening an installed app goes through the (fake) runner, never the real program.
    ui.invoke_open_app("harbor".into());
    until(windows, Duration::from_secs(10), "the launch", || {
        family.runner.calls().iter().any(|c| c[0].ends_with("harbor/2.4.0/harbor"))
    });
    // Opening Zynk's Flatpak copy asks Flatpak.
    ui.invoke_open_other("zynk".into(), 0);
    until(windows, Duration::from_secs(10), "flatpak run", || {
        family.runner.ran(&["flatpak", "run", "it.zynk.Zynk"])
    });
}

fn language_and_theme_switch_live(windows: &headless::Windows) {
    let root = tempfile::tempdir().unwrap();
    let family = fake::family(root.path());
    let app = App::new(family.core.clone(), None).unwrap();
    watch(&app);
    app.show().unwrap();
    until(windows, Duration::from_secs(20), "the app list", || app.window().get_family().row_count() == 5);
    app.set_look(ThemeChoice::Dark, Some("de"));
    headless::spin(windows, Duration::from_millis(100));
    let ui = app.window();
    assert_eq!(ui.get_theme(), 2);
    assert_eq!(ui.get_language(), 2);
    assert_ne!(card_status(&app, "foundry"), "Coming soon", "Rust strings follow the language");
    app.set_look(ThemeChoice::Light, Some("en"));
    headless::spin(windows, Duration::from_millis(100));
    assert_eq!(card_status(&app, "foundry"), "Coming soon");
}
