//! The window on the app list this build carries, as a computer of each kind shows it:
//! `cargo run -p suite-ui --example builtin -- <dir> [linux|macos|windows]`.
//!
//! Draws the home screen, the pages of the apps that are out and the install window into
//! PNGs in `<dir>`, without a display (the software renderer into memory) and without the
//! network: the catalog address is one nothing answers at, so the list and the icons are the
//! built-in ones. Everything lives in a temporary folder and no command is run.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout, reason = "a demo")]

use std::{path::Path, sync::Arc, time::Duration};

use slint::{ComponentHandle, LogicalSize};
use suite_catalog::{HostFacts, Os};
use suite_core::{BUILTIN_CATALOG, CatalogSource, Core};
use suite_fetch::{Fetcher, Options};
use suite_platform::Env;
use suite_ui::{App, ThemeChoice, headless};

fn save(window: &slint::Window, dir: &Path, name: &str) {
    let shot = window.take_snapshot().expect("snapshot");
    let png = suite_ui::encode_png(shot.width(), shot.height(), shot.as_bytes()).expect("png");
    let path = dir.join(format!("{name}.png"));
    std::fs::write(&path, png).expect("write");
    println!("{}", path.display());
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().ok_or("usage: builtin <dir> [linux|macos|windows]")?;
    let dir = Path::new(&dir);
    let systems = match args.next().as_deref() {
        Some("linux") => vec![Os::Linux],
        Some("macos") => vec![Os::Macos],
        Some("windows") => vec![Os::Windows],
        Some(other) => return Err(format!("unknown system {other:?}").into()),
        None => vec![Os::Linux, Os::Macos, Os::Windows],
    };
    std::fs::create_dir_all(dir)?;
    let windows = headless::Headless::install()?;
    for os in systems {
        let root = tempfile::tempdir()?;
        let env = Env::rooted_as(&std::fs::canonicalize(root.path())?, os)?;
        // Nothing listens on the discard port: the fetch fails at once.
        let nowhere = CatalogSource::Url("http://127.0.0.1:9/catalog.json".into());
        let fetcher =
            Fetcher::new(Options { retries: 0, backoff: Duration::from_millis(1), ..Options::default() });
        let core =
            Core::new(env, fetcher, nowhere).with_builtin(BUILTIN_CATALOG).with_facts(HostFacts::example(os));
        let catalog = core.builtin_catalog().ok_or("the built-in list doesn't parse")?;
        let app = App::new(Arc::new(core), None)?;
        app.set_look(ThemeChoice::Light, Some("en"));
        let window = app.window().window();
        window.set_size(LogicalSize::new(1120.0, 860.0));
        app.show()?;
        headless::spin(&windows, Duration::from_millis(1500));
        save(window, dir, &format!("home-{os}"));
        for a in catalog.apps.iter().filter(|a| !a.releases.is_empty()) {
            app.open_page(&a.id);
            headless::spin(&windows, Duration::from_millis(300));
            save(window, dir, &format!("page-{}-{os}", a.id));
        }
        // The install window, on an app a store or a website hands out here (nothing is
        // downloaded): what it offers besides.
        let handed_out = catalog.apps.iter().find(|a| {
            suite_catalog::best_installable(
                a,
                suite_catalog::Channel::Stable,
                &HostFacts::example(os),
                suite_catalog::Prefs::default(),
            )
            .is_ok_and(|(_, asset)| asset.store.is_some())
        });
        if let Some(a) = handed_out {
            app.install_window(&a.id, None)?;
            let compact = app.compact_window().expect("install window");
            compact.window().set_size(LogicalSize::new(460.0, 520.0));
            headless::spin(&windows, Duration::from_millis(1500));
            save(compact.window(), dir, &format!("install-window-{}-{os}", a.id));
        }
        window.hide()?;
    }
    Ok(())
}
