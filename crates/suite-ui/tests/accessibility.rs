//! The accessibility tree and keyboard reach of every screen, on Slint's headless testing
//! backend: each control has a role, a label and a default action, and Tab plus Enter
//! reaches it.
//!
//! Element queries need Slint's debug info, which normal builds leave out:
//! `SLINT_EMIT_DEBUG_INFO=1 cargo test -p suite-ui --test accessibility`.

#![allow(clippy::unwrap_used, reason = "test helpers")]

use std::{cell::RefCell, rc::Rc};

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery, ElementRoot};
use slint::{
    ComponentHandle, LogicalSize, ModelRc, SharedString, VecModel,
    platform::{Key, WindowEvent},
};
use suite_ui_markup::ui::{
    Action, AppWindow, CardData, Fact, InstallWindow, JobData, LogData, OtherInstall, PageData, PhoneCode,
    ReqLine, Tone, UpdateItem,
};

const INTERACTIVE: [AccessibleRole; 7] = [
    AccessibleRole::Button,
    AccessibleRole::RadioButton,
    AccessibleRole::Switch,
    AccessibleRole::Tab,
    AccessibleRole::ListItem,
    AccessibleRole::Checkbox,
    AccessibleRole::Spinbox,
];

type Log = Rc<RefCell<Vec<String>>>;

/// Without debug info the queries find nothing; say so instead of failing every build.
fn queryable() -> bool {
    let on = option_env!("SLINT_EMIT_DEBUG_INFO").is_some_and(|v| v == "1");
    if !on {
        eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to query the accessibility tree");
    }
    on
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn strings(items: &[&str]) -> ModelRc<SharedString> {
    model(items.iter().map(|s| SharedString::from(*s)).collect())
}

/// Every interactive element: (role, label, handle).
fn controls(root: &impl ElementRoot) -> Vec<(AccessibleRole, String, ElementHandle)> {
    let mut out = Vec::new();
    for role in INTERACTIVE {
        for e in ElementQuery::from_root(root).match_accessible_role(role).find_all() {
            out.push((role, e.accessible_label().unwrap_or_default().to_string(), e));
        }
    }
    out
}

fn assert_labelled(name: &str, root: &impl ElementRoot) -> usize {
    let found = controls(root);
    let unlabelled: Vec<_> =
        found.iter().filter(|c| c.1.trim().is_empty()).map(|c| format!("{:?}", c.0)).collect();
    assert!(unlabelled.is_empty(), "{name}: controls without a label: {unlabelled:?}");
    found.len()
}

fn press(window: &slint::Window, key: impl Into<SharedString>) {
    let text = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

/// What Enter does at each of the first `n` Tab stops, each from a fresh window.
fn stops<W: ComponentHandle>(make: impl Fn(&Log) -> W, n: usize) -> Vec<String> {
    (1..=n)
        .map(|k| {
            let log = Log::default();
            let ui = make(&log);
            ui.show().unwrap();
            for _ in 0..k {
                press(ui.window(), Key::Tab);
            }
            press(ui.window(), Key::Return);
            log.borrow().join("+")
        })
        .collect()
}

fn assert_reaches(name: &str, stops: &[String], wanted: &[&str]) {
    for w in wanted {
        assert!(
            stops.iter().any(|s| s.split('+').any(|a| a == *w)),
            "{name}: Tab never reaches {w}: {stops:?}"
        );
    }
}

fn card(id: &str, action: Action, text: &str) -> CardData {
    CardData {
        id: id.into(),
        part: "AMZ-09".into(),
        name: id.into(),
        pitch: "Does things well".into(),
        initial: "X".into(),
        status: "Installed · 1.0.0".into(),
        tone: Tone::Ok,
        action,
        action_text: text.into(),
        progress: -1.0,
        ..CardData::default()
    }
}

fn page_data() -> PageData {
    PageData {
        id: "specular".into(),
        name: "Specular".into(),
        pitch: "Photos".into(),
        description: "A photo viewer.".into(),
        initial: "S".into(),
        status: "Installed 1.1.0 · 1.2.0 is ready".into(),
        action: Action::Update,
        action_text: "Update".into(),
        progress: -1.0,
        facts: model(vec![Fact { label: "Installed".into(), value: "1.1.0".into() }]),
        reqs: model(vec![ReqLine { text: "A processor with AVX2".into(), ok: true }]),
        notes_url: "https://example.com/notes".into(),
        homepage: "https://example.com".into(),
        managed: true,
        can_beta: true,
        rollback: "Go back to 1.0.0".into(),
        has_data: true,
        others: model(vec![OtherInstall {
            text: "Installed by Flatpak".into(),
            note: "Its updates come from there.".into(),
            can_open: true,
        }]),
        phones: model(vec![PhoneCode {
            label: "Android".into(),
            url: "https://play.google.com/x".into(),
            code: slint::Image::default(),
        }]),
        ..PageData::default()
    }
}

fn job(id: i32, state: i32) -> JobData {
    JobData {
        id,
        app: format!("app{id}").into(),
        initial: "A".into(),
        title: format!("Installing app{id}").into(),
        stage: "Downloading".into(),
        fraction: 0.4,
        detail: "12 MB of 48 MB · 5 MB/s · 7 s left".into(),
        state,
        message: if state == 3 { "It failed.".into() } else { SharedString::new() },
        can_open: state == 2,
        can_retry: state == 3,
        ..JobData::default()
    }
}

fn app_window(log: &Log, page: i32) -> AppWindow {
    let ui = AppWindow::new().unwrap();
    // Tall enough that whole pages are on screen: queries skip clipped elements.
    ui.window().set_size(LogicalSize::new(1440.0, 2400.0));
    ui.set_page(page);
    ui.set_loading(false);
    ui.set_family(model(vec![
        card("specular", Action::Update, "Update"),
        card("harbor", Action::Open, "Open"),
        card("bindery", Action::Install, "Install"),
    ]));
    ui.set_others(model(vec![card("zynk", Action::Store, "Get it")]));
    ui.set_updates(model(vec![UpdateItem {
        id: "specular".into(),
        name: "Specular".into(),
        versions: "1.1 → 1.2".into(),
    }]));
    ui.set_updates_title("1 update is ready".into());
    ui.set_install_all("Install all (2)".into());
    ui.set_app(page_data());
    ui.set_jobs(model(vec![job(1, 1), job(2, 2), job(3, 3)]));
    ui.set_log(model(vec![LogData {
        when: "Today, 10:00".into(),
        text: "Installed Harbor 2.4.0".into(),
        tone: Tone::Normal,
    }]));
    ui.set_running(1);
    ui.set_languages(strings(&["System", "English", "Deutsch"]));
    ui.set_notice("That couldn't be opened.".into());
    let rec = |name: &'static str| {
        let log = Rc::clone(log);
        move || log.borrow_mut().push(name.into())
    };
    let rec_s = |name: &'static str| {
        let log = Rc::clone(log);
        move |s: SharedString| log.borrow_mut().push(format!("{name} {s}"))
    };
    let rec_i = |name: &'static str| {
        let log = Rc::clone(log);
        move |i: i32| log.borrow_mut().push(format!("{name} {i}"))
    };
    let rec_b = |name: &'static str| {
        let log = Rc::clone(log);
        move |b: bool| log.borrow_mut().push(format!("{name} {b}"))
    };
    let rec_pair = |name: &'static str| {
        let log = Rc::clone(log);
        move |s: SharedString, i: i32| log.borrow_mut().push(format!("{name} {s} {i}"))
    };
    ui.on_show_page(rec_i("page"));
    ui.on_refresh(rec("refresh"));
    ui.on_open_page(rec_s("open-page"));
    ui.on_act(rec_s("act"));
    ui.on_install_all_clicked(rec("install-all"));
    ui.on_update_all(rec("update-all"));
    ui.on_open_url(rec_s("url"));
    ui.on_open_other(rec_pair("open-other"));
    ui.on_set_channel(rec_pair("channel"));
    ui.on_set_policy(rec_pair("policy"));
    ui.on_rollback(rec_s("rollback"));
    let l = Rc::clone(log);
    ui.on_remove(move |id, purge| l.borrow_mut().push(format!("remove {id} {purge}")));
    ui.on_open_app(rec_s("open-app"));
    ui.on_retry(rec_i("retry"));
    ui.on_dismiss(rec_i("dismiss"));
    ui.on_dismiss_notice(rec("dismiss-notice"));
    ui.on_set_auto_update(rec_b("auto"));
    ui.on_set_skip_metered(rec_b("metered"));
    ui.on_set_scope(rec_i("scope"));
    ui.on_set_language(rec_i("language"));
    ui.on_set_theme(rec_i("theme"));
    ui
}

fn install_window(log: &Log, state: i32) -> InstallWindow {
    let ui = InstallWindow::new().unwrap();
    ui.window().set_size(LogicalSize::new(460.0, 400.0));
    ui.set_name("Specular".into());
    ui.set_heading("Installing Specular".into());
    ui.set_state(state);
    ui.set_message("It failed.".into());
    let l = Rc::clone(log);
    ui.on_open(move || l.borrow_mut().push("open".into()));
    let l = Rc::clone(log);
    ui.on_retry(move || l.borrow_mut().push("retry".into()));
    let l = Rc::clone(log);
    ui.on_dismiss(move || l.borrow_mut().push("dismiss".into()));
    // Two other apps offered under the install.
    ui.set_more(model(vec![card("dozed", Action::Store, "Get it"), card("bindery", Action::None, "")]));
    let l = Rc::clone(log);
    ui.on_show_app(move |id| l.borrow_mut().push(format!("show-app {id}")));
    let l = Rc::clone(log);
    ui.on_show_all(move || l.borrow_mut().push("show-all".into()));
    ui
}

#[test]
fn every_control_is_labelled() {
    i_slint_backend_testing::init_no_event_loop();
    if !queryable() {
        return;
    }
    for (page, name, at_least) in
        [(0, "apps", 12), (1, "app page", 12), (2, "activity", 6), (3, "settings", 12)]
    {
        let ui = app_window(&Log::default(), page);
        ui.show().unwrap();
        let n = assert_labelled(name, &ui);
        assert!(n >= at_least, "{name}: only {n} controls found");
    }
    let ui = app_window(&Log::default(), 1);
    ui.set_removing(true);
    ui.show().unwrap();
    assert_labelled("remove dialog", &ui);
    let boxes = ElementQuery::from_root(&ui).match_accessible_role(AccessibleRole::Checkbox).find_all();
    assert_eq!(boxes.len(), 1, "the purge choice");
    for state in 0..=3 {
        let w = install_window(&Log::default(), state);
        w.show().unwrap();
        assert_labelled("install window", &w);
    }
}

#[test]
fn apps_screen_is_operable_by_keyboard() {
    i_slint_backend_testing::init_no_event_loop();
    let stops = stops(|l| app_window(l, 0), 24);
    assert_reaches(
        "apps",
        &stops,
        &[
            "page 0",
            "page 2",
            "page 3",
            "refresh",
            "dismiss-notice",
            "update-all",
            "open-page specular",
            "install-all",
            "act specular",
            "open-page bindery",
            "act bindery",
            "act zynk",
        ],
    );
}

#[test]
fn app_page_is_operable_by_keyboard() {
    i_slint_backend_testing::init_no_event_loop();
    let stops = stops(|l| app_window(l, 1), 30);
    assert_reaches(
        "app page",
        &stops,
        &[
            "page 0",
            "act specular",
            "url https://example.com/notes",
            "url https://example.com",
            "open-other specular 0",
            "channel specular 1",
            "policy specular 2",
            "rollback specular",
        ],
    );
    // Remove asks first; the dialog is reachable and removes with the data when ticked.
    let log = Log::default();
    let ui = app_window(&log, 1);
    ui.set_removing(true);
    ui.show().unwrap();
    for _ in 0..60 {
        press(ui.window(), Key::Tab);
    }
    assert!(ui.get_removing(), "Tab can't close the dialog by itself");
    let confirm = stops_in_dialog();
    assert!(confirm.iter().any(|s| s == "remove specular false"), "{confirm:?}");
    // The purge box is found by its role, which needs the debug info.
    if queryable() {
        assert!(confirm.iter().any(|s| s == "remove specular true"), "{confirm:?}");
    }
}

/// Enter on each stop of the remove dialog after ticking (or not) the purge box.
fn stops_in_dialog() -> Vec<String> {
    let mut out = Vec::new();
    for tick in [false, true] {
        for k in 1..=40 {
            let log = Log::default();
            let ui = app_window(&log, 1);
            ui.set_notice("".into());
            ui.set_removing(true);
            ui.show().unwrap();
            if tick {
                // Find the checkbox by its label and tick it, as Space would.
                let boxes =
                    ElementQuery::from_root(&ui).match_accessible_role(AccessibleRole::Checkbox).find_all();
                if let Some(b) = boxes.first() {
                    b.invoke_accessible_default_action();
                }
            }
            for _ in 0..k {
                press(ui.window(), Key::Tab);
            }
            press(ui.window(), Key::Return);
            out.extend(log.borrow().iter().cloned());
        }
    }
    out
}

#[test]
fn activity_is_operable_by_keyboard() {
    i_slint_backend_testing::init_no_event_loop();
    let stops = stops(|l| app_window(l, 2), 16);
    assert_reaches("activity", &stops, &["open-app app2", "dismiss 2", "retry 3", "dismiss 3"]);
}

#[test]
fn settings_is_operable_by_keyboard() {
    i_slint_backend_testing::init_no_event_loop();
    let stops = stops(|l| app_window(l, 3), 20);
    assert_reaches(
        "settings",
        &stops,
        &["auto false", "metered false", "scope 0", "language 0", "language 2", "theme 1", "theme 2"],
    );
}

#[test]
fn install_window_is_operable_by_keyboard() {
    i_slint_backend_testing::init_no_event_loop();
    let more = ["show-all", "show-app dozed", "show-app bindery"];
    let reaches = |name: &str, state: i32, own: &[&str]| {
        let wanted: Vec<&str> = more.iter().chain(own).copied().collect();
        assert_reaches(name, &stops(|l| install_window(l, state), wanted.len() + 1), &wanted);
    };
    reaches("install window, done", 1, &["open", "dismiss"]);
    reaches("install window, failed", 2, &["retry", "dismiss"]);
    reaches("install window, working", 0, &["dismiss"]);
}

#[test]
fn progress_and_problems_are_announced() {
    i_slint_backend_testing::init_no_event_loop();
    if !queryable() {
        return;
    }
    let ui = app_window(&Log::default(), 2);
    ui.show().unwrap();
    let bars =
        ElementQuery::from_root(&ui).match_accessible_role(AccessibleRole::ProgressIndicator).find_all();
    assert!(!bars.is_empty());
    assert_eq!(bars[0].accessible_value().unwrap_or_default(), "40%");
    let texts: Vec<String> = ElementQuery::from_root(&ui)
        .match_accessible_role(AccessibleRole::Text)
        .find_all()
        .iter()
        .filter_map(|e| e.accessible_label().map(|l| l.to_string()))
        .collect();
    assert!(texts.iter().any(|t| t == "It failed."), "{texts:?}");
    assert!(texts.iter().any(|t| t == "That couldn't be opened."), "{texts:?}");
    // Cards say what each app is and where it stands.
    let ui = app_window(&Log::default(), 0);
    ui.show().unwrap();
    let cards: Vec<_> = ElementQuery::from_root(&ui)
        .match_accessible_role(AccessibleRole::Button)
        .find_all()
        .into_iter()
        .filter(|e| e.accessible_label().is_some_and(|l| l == "bindery"))
        .collect();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].accessible_description().unwrap_or_default(), "Does things well. Installed · 1.0.0");
}
