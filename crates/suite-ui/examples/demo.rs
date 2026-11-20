//! The app window on fake family data: `cargo run -p suite-ui --example demo`.
//!
//! The apps come from a local test server (suite-testkit) and install into a temporary
//! folder; nothing outside it changes. Specular has an update, Harbor a beta, Almanac needs a
//! newer computer, Foundry is coming soon, and Zynk is installed by Flatpak, has phone
//! apps, and its pinned download has moved on.
//!
//! - `STOCKROOM_DEMO_THEME=light|dark`, `STOCKROOM_DEMO_LANG=de|es|fr|nl|en` pick the look.
//! - `STOCKROOM_DEMO_SNAPSHOT=<dir>` draws every screen in both themes without a display (the
//!   software renderer into memory, window contents only) and saves them as PNGs there.
//! - Window timings are logged under `suite_ui::timing`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout, reason = "a demo")]

#[path = "../tests/support/fake.rs"]
mod fake;

use std::{path::Path, rc::Rc, time::Duration};

use slint::{ComponentHandle, LogicalSize};
use suite_core::{CoreEvent, ErrorKind, Job};
use suite_install::Stage;
use suite_ui::{App, ThemeChoice, headless};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    suite_ui::timing::launched(std::time::Instant::now());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("suite_ui::timing=info")),
        )
        .init();
    let root = tempfile::tempdir()?;
    let family = fake::family(root.path());
    if let Some(dir) = std::env::var_os("STOCKROOM_DEMO_SNAPSHOT") {
        return snapshots(&family, Path::new(&dir));
    }
    suite_ui::init()?;
    let app = App::new(family.core.clone(), None)?;
    let theme = match std::env::var("STOCKROOM_DEMO_THEME").as_deref() {
        Ok("light") => ThemeChoice::Light,
        Ok("dark") => ThemeChoice::Dark,
        _ => ThemeChoice::System,
    };
    app.set_look(theme, std::env::var("STOCKROOM_DEMO_LANG").ok().as_deref());
    app.show()?;
    slint::run_event_loop_until_quit()?;
    Ok(())
}

/// A download in progress (a steady 5.6 MB/s, fed by a timer for as long as the demo
/// runs), so Activity has something to show.
fn activity(app: &Rc<App>, windows: &headless::Windows) -> slint::Timer {
    app.inject(CoreEvent::Queued { id: 901, job: Job::Update { app: "harbor".into() } });
    app.inject(CoreEvent::Started { id: 901 });
    app.inject(CoreEvent::Stage { id: 901, stage: Stage::Download });
    let total = 148_200_000;
    let done = std::cell::Cell::new(0_u64);
    let feeder = slint::Timer::default();
    let a = Rc::clone(app);
    feeder.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
        done.set((done.get() + 1_400_000).min(total - 1));
        a.inject_progress(901, done.get(), total);
    });
    headless::spin(windows, Duration::from_secs(4));
    feeder
}

fn save(window: &slint::Window, dir: &Path, name: &str) {
    let shot = window.take_snapshot().expect("snapshot");
    let png = suite_ui::encode_png(shot.width(), shot.height(), shot.as_bytes()).expect("png");
    let path = dir.join(format!("{name}.png"));
    std::fs::write(&path, png).expect("write");
    println!("{}", path.display());
}

fn snapshots(family: &fake::Family, dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let windows = headless::Headless::install()?;
    let app = Rc::new(App::new(family.core.clone(), None)?);
    let window = app.window().window();
    window.set_size(LogicalSize::new(1120.0, 760.0));
    app.show()?;
    headless::spin(&windows, Duration::from_millis(1500));
    // Zynk's pinned download moved on: a real failure, from the real queue.
    app.inject(CoreEvent::Queued { id: 900, job: Job::Install { app: "zynk".into(), channel: None } });
    app.inject(CoreEvent::Started { id: 900 });
    app.inject(CoreEvent::Failed {
        id: 900,
        error: "Zynk released a new build; the catalog needs refreshing before it can be installed (nothing was changed)".into(),
        kind: ErrorKind::NeedsRefresh,
    });
    let _feeder = activity(&app, &windows);

    let languages: Vec<String> =
        std::env::var("STOCKROOM_DEMO_LANGS").map_or_default(|l| l.split(',').map(str::to_owned).collect());
    for (theme, tname) in [(ThemeChoice::Light, "light"), (ThemeChoice::Dark, "dark")] {
        app.set_look(theme, Some("en"));
        let shot = |name: &str, height: f32| {
            window.set_size(LogicalSize::new(1120.0, height));
            headless::spin(&windows, Duration::from_millis(300));
            save(window, dir, &format!("{name}-{tname}"));
        };
        app.show_page(0);
        shot("home", 760.0);
        for id in ["specular", "harbor", "zynk", "almanac", "foundry"] {
            app.open_page(id);
            shot(&format!("page-{id}"), 1000.0);
        }
        app.open_page("specular");
        app.window().set_removing(true);
        shot("remove-dialog", 760.0);
        app.window().set_removing(false);
        app.show_page(2);
        shot("activity", 900.0);
        app.show_page(3);
        shot("settings", 760.0);
        app.show_page(0);
        window.set_size(LogicalSize::new(700.0, 760.0));
        headless::spin(&windows, Duration::from_millis(300));
        save(window, dir, &format!("home-narrow-{tname}"));
    }
    for lang in &languages {
        app.set_look(ThemeChoice::Light, Some(lang));
        for (page, name) in [(0, "home"), (2, "activity"), (3, "settings")] {
            app.show_page(page);
            window.set_size(LogicalSize::new(1120.0, 760.0));
            headless::spin(&windows, Duration::from_millis(300));
            save(window, dir, &format!("{name}-{lang}"));
        }
        app.open_page("specular");
        headless::spin(&windows, Duration::from_millis(300));
        save(window, dir, &format!("page-specular-{lang}"));
    }

    // The compact window: a real install of bindery from the local server, then Open.
    app.set_look(ThemeChoice::Light, Some("en"));
    app.install_window("bindery", None)?;
    let compact = app.compact_window().expect("compact window");
    compact.window().set_size(LogicalSize::new(460.0, 352.0));
    for (theme, tname) in [(ThemeChoice::Light, "light"), (ThemeChoice::Dark, "dark")] {
        app.set_look(theme, Some("en"));
        headless::spin(&windows, Duration::from_millis(1500));
        save(compact.window(), dir, &format!("install-window-done-{tname}"));
    }
    // And a failure: Zynk's moved download.
    app.install_window("zynk", None)?;
    let compact = app.compact_window().expect("compact window");
    compact.window().set_size(LogicalSize::new(460.0, 452.0));
    for (theme, tname) in [(ThemeChoice::Light, "light"), (ThemeChoice::Dark, "dark")] {
        app.set_look(theme, Some("en"));
        headless::spin(&windows, Duration::from_millis(1500));
        save(compact.window(), dir, &format!("install-window-failed-{tname}"));
    }
    Ok(())
}
