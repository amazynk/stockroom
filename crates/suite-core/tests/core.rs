//! Loading the catalog, the job queue, policies and the background run, against the fake
//! server.

// Test code: a failed unwrap is a failed test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{sync::Arc, time::Duration};

use suite_catalog::{AssetKind, CatalogError, Os};
use suite_core::{
    CatalogSource, Core, CoreError, CoreEvent, ErrorKind, Job, JobOutcome, Level, ListOrigin, Queue,
};
use suite_fetch::{Fetcher, Options};
use suite_install::Policy;
use suite_platform::{Env, Metered, network::FixedNetwork};
use suite_testkit::{Fixture, fake_app};
use tempfile::TempDir;

fn core(root: &TempDir, fx: &Fixture) -> Core {
    let env = Env::for_tests(root.path(), Os::Linux);
    core_with(env, fx)
}

fn core_with(env: Env, fx: &Fixture) -> Core {
    let fetcher = Fetcher::new(Options { backoff: Duration::from_millis(5), ..Options::default() });
    Core::new(env, fetcher, CatalogSource::Url(fx.catalog_url())).with_facts(suite_testkit::facts(Os::Linux))
}

fn fixture() -> Fixture {
    let mut fx = Fixture::new();
    fx.add_release("demo", "1.0.0", AssetKind::TarGz, fake_app("demo", "1.0.0"));
    fx.add_release("other", "0.5.0", AssetKind::TarGz, fake_app("other", "0.5.0"));
    fx.publish();
    fx
}

#[test]
fn a_list_that_breaks_the_rules_is_refused() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let c = core(&root, &fx);
    assert_eq!(c.load_catalog().unwrap().apps.len(), 2);
    let json = String::from_utf8(fx.catalog.to_json_pretty().unwrap()).unwrap();
    fx.server.put("/catalog.json", json.replace(r#""id": "demo""#, r#""id": "../evil""#));
    assert!(matches!(c.load_catalog(), Err(CoreError::Catalog(CatalogError::Invalid(_)))));
    fx.server.put("/catalog.json", b"{".to_vec());
    assert!(matches!(c.load_catalog(), Err(CoreError::Catalog(CatalogError::Malformed(_)))));
}

#[test]
fn an_older_list_never_replaces_a_newer_one() {
    let mut fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let c = core(&root, &fx);
    let old_json = fx.catalog.to_json_pretty().unwrap();
    fx.publish();
    c.load_catalog().unwrap();
    assert_eq!(c.state().unwrap().catalog.last_version, Some(fx.catalog.version));
    // A stale copy on the way serves the list from before.
    fx.server.put("/catalog.json", old_json);
    assert!(matches!(c.load_catalog(), Err(CoreError::Catalog(CatalogError::Older { .. }))));
}

#[test]
fn falls_back_to_the_saved_or_built_in_list() {
    let mut fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let builtin = fx.catalog.to_json_pretty().unwrap();
    fx.server.remove("/catalog.json");

    // A catalog used as given (no built-in list) fails when it can't be fetched.
    assert!(matches!(core(&root, &fx).catalog(), Err(CoreError::Fetch(_))));
    assert!(core(&root, &fx).offline_catalog().is_none());

    // With one, the list the build came with stands in, and says why.
    let c = core(&root, &fx).with_builtin(builtin.clone());
    let list = c.catalog().unwrap();
    assert_eq!(list.origin, ListOrigin::BuiltIn);
    assert_eq!(list.problem.as_ref().map(|p| p.0), Some(ErrorKind::Network));
    assert_eq!(list.catalog.version, fx.catalog.version);
    // It installs: the download is checked against the list's checksum as always.
    c.install(&list.catalog, "demo", None, &mut |_| {}).unwrap();
    // The built-in list is not a server answer: it leaves no replay mark.
    assert_eq!(c.state().unwrap().catalog.last_version, None);

    // The server is back with a newer list, which is used and saved.
    fx.add_release("demo", "1.1.0", AssetKind::TarGz, fake_app("demo", "1.1.0"));
    fx.publish();
    let list = c.catalog().unwrap();
    assert_eq!((list.origin, list.problem.is_none()), (ListOrigin::Server, true));
    // Gone again: the saved copy is newer than the built-in list.
    fx.server.remove("/catalog.json");
    let list = c.catalog().unwrap();
    assert_eq!((list.origin, list.catalog.version), (ListOrigin::Saved, fx.catalog.version));
    assert!(c.update(&list.catalog, "demo", &mut |_| {}).unwrap().is_some());

    // A list that breaks the rules is a reason to fall back too, never to use it.
    let mut broken = fx.catalog.clone();
    broken.version += 1;
    broken.apps[0].pitch = "broken".into();
    broken.apps[0].id = "../evil".into();
    fx.server.put("/catalog.json", broken.to_json_pretty().unwrap());
    let list = c.catalog().unwrap();
    assert_eq!(list.origin, ListOrigin::Saved);
    assert_eq!(list.problem.as_ref().map(|p| p.0), Some(ErrorKind::Catalog));
    assert_ne!(list.catalog.apps[0].pitch, "broken");

    // A later build brings a newer built-in list than the copy saved here.
    let mut newer = fx.catalog.clone();
    newer.version += 10;
    let later = core(&root, &fx).with_builtin(newer.to_json_pretty().unwrap());
    let (catalog, origin) = later.offline_catalog().unwrap();
    assert_eq!((origin, catalog.version), (ListOrigin::BuiltIn, newer.version));
    // A broken built-in list is skipped, not fatal.
    let broken = core(&root, &fx).with_builtin(b"{".to_vec());
    assert_eq!(broken.offline_catalog().unwrap().1, ListOrigin::Saved);
}

#[test]
fn the_saved_list_is_checked_again_when_read() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let c = core(&root, &fx);
    // Saved a while ago: still the last known list.
    let mut old = fx.catalog.clone();
    old.published -= time::Duration::days(60);
    let json = old.to_json_pretty().unwrap();
    let data = &c.env().paths.data;
    std::fs::create_dir_all(data).unwrap();
    std::fs::write(data.join("catalog.json"), &json).unwrap();
    assert_eq!(c.cached_catalog().unwrap().unwrap(), old);
    // A saved copy that no longer reads is not used.
    std::fs::write(data.join("catalog.json"), &json[..json.len() / 2]).unwrap();
    assert!(matches!(c.cached_catalog(), Err(CoreError::Catalog(CatalogError::Malformed(_)))));
    assert!(c.offline_catalog().is_none());
}

#[test]
fn icons_come_from_the_server_or_from_the_build() {
    let mut fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let png = b"\x89PNG\r\n\x1a\n drawn elsewhere".to_vec();
    fx.server.put("/icons/demo.png", png.clone());
    let icon =
        |url: String, bytes: &[u8]| Some(suite_catalog::Icon { url, sha256: suite_testkit::sha256(bytes) });
    fx.catalog.apps[0].icon = icon(fx.server.url("/icons/demo.png"), &png);
    // An app this build has an icon for, named by the catalog with the same checksum...
    let (_, carried) = suite_core::BUILTIN_ICONS.iter().find(|(id, _)| *id == "specular").unwrap();
    let mut same = suite_testkit::app_entry("specular");
    same.icon = icon(fx.server.url("/icons/specular.png"), carried);
    // ...and with a newer drawing that the server doesn't have (yet).
    let mut newer = suite_testkit::app_entry("specular");
    newer.icon = icon(fx.server.url("/icons/specular-new.png"), b"newer");
    let c = core(&root, &fx);

    // Downloaded once, checked, kept.
    let demo = &fx.catalog.apps[0];
    let file = c.icon_file(demo).unwrap().unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), png);
    c.icon_file(demo).unwrap();
    assert_eq!(fx.server.hits("/icons/demo.png"), 1);
    assert_eq!(c.icon_file(&fx.catalog.apps[1]).unwrap(), None, "the catalog names none");

    // The carried icon is the one the catalog means: nothing is downloaded.
    let file = c.icon_file(&same).unwrap().unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), *carried);
    assert_eq!(fx.server.hits("/icons/specular.png"), 0);
    // It stands in for one that can't be had, without being kept as that one.
    let stand_in = c.icon_file(&newer).unwrap().unwrap();
    assert_eq!(std::fs::read(&stand_in).unwrap(), *carried);
    fx.server.put("/icons/specular-new.png", b"newer".to_vec());
    assert_eq!(std::fs::read(c.icon_file(&newer).unwrap().unwrap()).unwrap(), b"newer");

    // A download that isn't the promised picture is refused.
    fx.server.put("/icons/demo.png", b"swapped".to_vec());
    let other_root = tempfile::tempdir().unwrap();
    let r = core(&other_root, &fx).icon_file(demo);
    assert!(matches!(r, Err(CoreError::Fetch(suite_fetch::FetchError::Checksum { .. }))), "{r:?}");
}

#[test]
fn the_built_in_list_is_usable() {
    let c = suite_catalog::Catalog::from_json(suite_core::BUILTIN_CATALOG).unwrap();
    let specular = c.app("specular").expect("Specular is in the built-in list");
    assert!(!specular.releases.is_empty(), "with something to install");
    for os in [Os::Linux, Os::Macos, Os::Windows] {
        let facts = suite_catalog::HostFacts::example(os);
        let found = c.apps.iter().any(|a| {
            suite_catalog::best_installable(
                a,
                suite_catalog::Channel::Stable,
                &facts,
                suite_catalog::Prefs::default(),
            )
            .is_ok()
        });
        assert!(found, "nothing in the built-in list installs on {os}");
    }
    // The build carries the icon of every app in it that has one.
    for app in c.apps.iter().filter(|a| a.icon.is_some()) {
        assert!(suite_core::BUILTIN_ICONS.iter().any(|(id, _)| *id == app.id), "no icon for {}", app.id);
    }
}

#[test]
fn queue_reports_progress_and_results() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let c = Arc::new(core(&root, &fx));
    let catalog = Arc::new(c.load_catalog().unwrap());
    let mut q = Queue::start(c.clone(), catalog);
    let a = q.submit(Job::Install { app: "demo".into(), channel: None });
    let b = q.submit(Job::Install { app: "nope".into(), channel: None });
    let mut seen = Vec::new();
    let end = q.wait(a, &mut |e| seen.push(e.clone())).unwrap();
    assert!(matches!(end, CoreEvent::Finished { outcome: JobOutcome::Installed(_), .. }), "{end:?}");
    assert!(seen.iter().any(|e| matches!(e, CoreEvent::Progress { done, total, .. } if done == total)));
    let end = q.wait(b, &mut |_| {}).unwrap();
    assert!(matches!(&end, CoreEvent::Failed { error, .. } if error.contains("nope")), "{end:?}");
    let up = q.submit(Job::Update { app: "demo".into() });
    assert!(matches!(
        q.wait(up, &mut |_| {}),
        Some(CoreEvent::Finished { outcome: JobOutcome::UpToDate, .. })
    ));
    let rm = q.submit(Job::Remove { app: "demo".into(), purge: false });
    assert!(matches!(
        q.wait(rm, &mut |_| {}),
        Some(CoreEvent::Finished { outcome: JobOutcome::Removed { .. }, .. })
    ));
    q.finish();
    assert!(c.state().unwrap().apps.is_empty());
}

#[test]
fn background_update_follows_policies() {
    let mut fx = fixture();
    fx.add_release("third", "1.0.0", AssetKind::TarGz, fake_app("third", "1.0.0"));
    fx.publish();
    let root = tempfile::tempdir().unwrap();
    let c = core(&root, &fx);
    let cat = c.load_catalog().unwrap();
    for app in ["demo", "other", "third"] {
        c.install(&cat, app, None, &mut |_| {}).unwrap();
    }
    c.set_policy("other", Policy::Ask).unwrap();
    c.set_policy("third", Policy::Never).unwrap();
    assert!(c.set_policy("missing", Policy::Ask).is_err());

    fx.add_release("demo", "1.1.0", AssetKind::TarGz, fake_app("demo", "1.1.0"));
    fx.add_release("other", "0.6.0", AssetKind::TarGz, fake_app("other", "0.6.0"));
    fx.add_release("third", "1.1.0", AssetKind::TarGz, fake_app("third", "1.1.0"));
    fx.publish();

    let r = c.background_update(false).unwrap();
    assert_eq!(r.skipped, None);
    assert_eq!(r.applied.len(), 1);
    assert_eq!(r.applied[0].app, "demo");
    assert_eq!(r.waiting, [("other".to_owned(), "0.6.0".parse().unwrap())]);
    let st = c.state().unwrap();
    assert_eq!(st.apps["demo"].current.to_string(), "1.1.0");
    assert_eq!(st.apps["other"].current.to_string(), "0.5.0");
    assert_eq!(st.apps["third"].current.to_string(), "1.0.0");

    // At most daily.
    assert_eq!(c.background_update(false).unwrap().skipped, Some("checked recently"));
    assert!(c.background_update(true).unwrap().skipped.is_none());

    let rows = c.rows(&cat).unwrap();
    assert_eq!(rows.len(), 3);
}

#[test]
fn background_update_waits_on_metered_networks() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let mut env = Env::for_tests(root.path(), Os::Linux);
    env.network = Arc::new(FixedNetwork(Metered::Yes));
    let c = core_with(env, &fx);
    assert_eq!(c.background_update(false).unwrap().skipped, Some("metered connection"));
    assert_eq!(fx.server.hits("/catalog.json"), 0);
}

#[test]
fn doctor_reports() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let checks = core(&root, &fx).doctor();
    let catalog = checks.iter().find(|c| c.name == "catalog").unwrap();
    assert_eq!(catalog.level, Level::Ok, "{catalog:?}");
    let sched = checks.iter().find(|c| c.name == "scheduler").unwrap();
    assert_eq!(sched.level, Level::Warn);
    fx.server.remove("/catalog.json");
    let checks = core(&root, &fx).doctor();
    assert_eq!(checks.iter().find(|c| c.name == "catalog").unwrap().level, Level::Fail);
}

#[test]
fn history_cache_and_launch() {
    let mut fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let c = core(&root, &fx);
    assert!(c.cached_catalog().unwrap().is_none(), "nothing cached before the first load");
    let catalog = c.load_catalog().unwrap();
    // The verified copy is kept and checked again without the network.
    fx.server.remove("/catalog.json");
    assert_eq!(c.cached_catalog().unwrap().unwrap(), catalog);

    c.install(&catalog, "demo", None, &mut |_| {}).unwrap();
    fx.add_release("demo", "1.1.0", AssetKind::TarGz, fake_app("demo", "1.1.0"));
    fx.publish();
    let catalog = c.load_catalog().unwrap();
    c.update(&catalog, "demo", &mut |_| {}).unwrap();
    c.rollback("demo").unwrap();
    c.remove(Some(&catalog), "demo", true).unwrap();
    let kinds: Vec<_> = c.state().unwrap().history.into_iter().map(|h| h.what).collect();
    let v = |s: &str| s.parse::<semver::Version>().unwrap();
    assert_eq!(
        kinds,
        [
            suite_core::ChangeKind::Installed { version: v("1.0.0") },
            suite_core::ChangeKind::Updated { from: v("1.0.0"), to: v("1.1.0") },
            suite_core::ChangeKind::RolledBack { from: v("1.1.0"), to: v("1.0.0") },
            suite_core::ChangeKind::Removed { version: v("1.0.0"), purged: true },
        ]
    );

    // Opening goes through the runner (never the real program in tests).
    c.install(&catalog, "other", None, &mut |_| {}).unwrap();
    let (program, args) = c.installer().launch_command("other").unwrap();
    assert!(program.ends_with("other/0.5.0/other"), "{program}");
    assert_eq!(args.len(), 0);
    assert!(c.open_app("other").is_err(), "the fake runner knows no programs");
    assert!(matches!(c.open_app("demo"), Err(CoreError::Install(_))));
    assert!(c.open_url("http://example.com").is_err(), "plain HTTP is refused");
}

#[test]
fn errors_have_kinds_for_plain_messages() {
    use suite_core::ErrorKind;
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let c = Arc::new(core(&root, &fx));
    let catalog = Arc::new(c.load_catalog().unwrap());
    // A download that doesn't match the catalog.
    let mut tampered = fake_app("demo", "1.0.0");
    let last = tampered.len() - 1;
    tampered[last] ^= 0xFF;
    fx.server.put("/assets/demo-1.0.0.tar.gz", tampered);
    let mut q = Queue::start(c.clone(), catalog);
    let id = q.submit(Job::Install { app: "demo".into(), channel: None });
    let end = q.wait(id, &mut |_| {}).unwrap();
    assert!(matches!(end, CoreEvent::Failed { kind: ErrorKind::Integrity, .. }), "{end:?}");
    let id = q.submit(Job::Rollback { app: "other".into() });
    let end = q.wait(id, &mut |_| {}).unwrap();
    assert!(matches!(end, CoreEvent::Failed { kind: ErrorKind::Other, .. }), "{end:?}");
    // The event stream can move to another thread (a UI's pump).
    let events = q.take_events();
    let id = q.submit(Job::Install { app: "nope".into(), channel: None });
    let failed = events.iter().find(|e| e.id() == id && e.is_terminal()).unwrap();
    assert!(matches!(failed, CoreEvent::Failed { kind: ErrorKind::Other, .. }), "{failed:?}");
    q.finish();
    let pinned = CoreError::Install(suite_install::InstallError::PinnedChanged { app: "zynk".into() });
    assert_eq!(pinned.kind(), ErrorKind::NeedsRefresh);
    assert_eq!(CoreError::Catalog(CatalogError::Older { got: 1, seen: 2 }).kind(), ErrorKind::Catalog);
}

#[test]
fn settings_go_through_the_scheduler_and_state() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let c = core(&root, &fx);
    let exe = root.path().join("stockroom");
    let kind = c.set_auto_update(true, &exe).unwrap();
    assert!(kind.is_some());
    assert!(suite_platform::Scheduler::new(c.env()).status().enabled);
    assert_eq!(c.set_auto_update(false, &exe).unwrap(), None);
    assert!(!suite_platform::Scheduler::new(c.env()).status().enabled);
    assert!(!c.state().unwrap().settings.auto_update);
    c.set_skip_metered(false).unwrap();
    assert!(!c.state().unwrap().settings.skip_metered);
    // For everyone: macOS only (the copy in /Applications).
    assert_eq!(c.set_scope(suite_core::Scope::AllUsers).is_ok(), Core::all_users_supported());
    c.set_scope(suite_core::Scope::User).unwrap();
    assert!(matches!(c.set_channel("demo", suite_catalog::Channel::Beta), Err(CoreError::NotInstalled(_))));
}

#[test]
fn uninstalling_the_suite_leaves_the_apps() {
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let c = core(&root, &fx);
    let catalog = c.load_catalog().unwrap();
    c.install(&catalog, "demo", None, &mut |_| {}).unwrap();
    // A copy of Stockroom run from a download: it keeps a copy and registers that for the daily check.
    let download = c.env().paths.home.join("Downloads/demo-setup");
    std::fs::create_dir_all(download.parent().unwrap()).unwrap();
    std::fs::write(&download, "the stockroom").unwrap();
    let kept = suite_install::settle::settle(c.env(), &download).unwrap();
    assert_ne!(kept, download);
    c.set_auto_update(true, &kept).unwrap();
    assert!(suite_platform::Scheduler::new(c.env()).status().enabled);

    let removed = c.uninstall_suite().unwrap();
    assert!(removed.iter().any(|p| kept.starts_with(p)), "{removed:?}");
    assert!(!kept.exists());
    assert!(!suite_platform::Scheduler::new(c.env()).status().enabled);
    assert!(c.cached_catalog().unwrap().is_none(), "the saved list went too");
    // The app is untouched and still known.
    let state = c.state().unwrap();
    assert!(state.apps.contains_key("demo") && !state.settings.auto_update);
    assert_eq!(c.installer().launch_command("demo").unwrap().1.len(), 0);
}

#[test]
fn other_installs_open_through_their_source() {
    use suite_detect::{Found, Source};
    let mut app = suite_testkit::app_entry("zynk");
    let flatpak = Found { source: Source::Flatpak { user: true }, version: None, location: None };
    assert!(suite_core::can_open_found(&app, &flatpak));
    let store = Found { source: Source::MicrosoftStore, version: None, location: None };
    assert!(!suite_core::can_open_found(&app, &store), "no AppUserModelID to start it by");
    app.ids.windows_aumid = Some("Zynk.Zynk_abc!App".into());
    assert!(suite_core::can_open_found(&app, &store));
    let msi = Found { source: Source::WindowsInstaller { machine: true }, version: None, location: None };
    assert!(!suite_core::can_open_found(&app, &msi));
    let bundle =
        Found { source: Source::MacApp, version: None, location: Some("/Applications/Zynk.app".into()) };
    assert!(suite_core::can_open_found(&app, &bundle));
    let fx = fixture();
    let root = tempfile::tempdir().unwrap();
    let c = core(&root, &fx);
    // The fake runner answers nothing: the attempt is recorded and fails cleanly.
    assert!(c.open_found(&app, &flatpak).is_err());
    assert!(c.open_found(&app, &msi).is_err());
}
