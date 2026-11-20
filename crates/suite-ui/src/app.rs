//! The window's controller: it drives `suite-core` from a worker thread and its job queue,
//! and shows the results. All decisions about apps stay in the core; what to show is in
//! `view` and `jobs`; this file only wires them to the markup.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    path::PathBuf,
    rc::{Rc, Weak},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use slint::{
    ComponentHandle, Image, ModelRc, PlatformError, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode,
    VecModel, winit_030::WinitWindowAccessor,
};
use suite_catalog::{Catalog, Channel, HostFacts, Os};
use suite_core::{Core, CoreEvent, ErrorKind, Job, Queue};
use suite_install::State;
use suite_platform::{ScheduleStatus, Scheduler, UserActivity};
use suite_ui_markup::ui;

use crate::{
    bind,
    compact::Compact,
    i18n::{self, tr},
    icons, instance,
    jobs::Jobs,
    look::{self, ThemeChoice},
    prefs::Prefs,
    qr, timing,
    view::{self, Act, Row},
    words,
};

/// How often running jobs' progress is shown; nothing ticks while nothing runs.
const TICK: Duration = Duration::from_millis(250);

/// What the worker and the queue's event pump send to the window.
#[derive(Debug)]
pub(crate) enum Msg {
    Catalog {
        catalog: Arc<Catalog>,
        fresh: bool,
    },
    CatalogFailed {
        kind: ErrorKind,
        detail: String,
    },
    Snapshot(Box<Snapshot>),
    Icon {
        app: String,
        pixels: SharedPixelBuffer<Rgba8Pixel>,
    },
    Core(CoreEvent),
    /// Something the user asked for failed (opening an app, a setting).
    Problem {
        what: Problem,
        detail: String,
    },
    Request(instance::Request),
}

/// What failed; worded on the UI thread, in the window's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Problem {
    Open,
    Setting,
}

/// Everything the window shows about the computer, read off the UI thread.
#[derive(Debug)]
pub(crate) struct Snapshot {
    pub rows: Vec<Row>,
    pub state: State,
    pub schedule: ScheduleStatus,
    pub facts: HostFacts,
    /// The rows were computed against a catalog (else only state and settings are new).
    pub has_rows: bool,
}

/// Messages waiting for the UI thread, and progress (kept apart: only the newest counts).
#[derive(Debug, Default)]
pub(crate) struct Inbox {
    messages: Mutex<VecDeque<Msg>>,
    progress: Mutex<HashMap<u64, (u64, u64)>>,
    scheduled: AtomicBool,
}

thread_local! {
    static CURRENT: RefCell<Weak<Inner>> = const { RefCell::new(Weak::new()) };
}

impl Inbox {
    pub(crate) fn post(self: &Arc<Self>, msg: Msg) {
        lock(&self.messages).push_back(msg);
        if !self.scheduled.swap(true, Ordering::AcqRel) {
            let inbox = Arc::clone(self);
            let sent = slint::invoke_from_event_loop(move || {
                inbox.scheduled.store(false, Ordering::Release);
                CURRENT.with_borrow(|w| {
                    if let Some(inner) = w.upgrade() {
                        inner.drain();
                    }
                });
            });
            if let Err(e) = sent {
                tracing::debug!("event loop gone: {e}");
            }
        }
    }

    fn progress(&self, id: u64, done: u64, total: u64) {
        lock(&self.progress).insert(id, (done, total));
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

type Work = Box<dyn FnOnce(&Core) + Send>;

/// One thread that talks to the core (state file, catalog, detection, scheduler), so the
/// window never waits on disk or network.
#[derive(Debug)]
struct Worker {
    tx: mpsc::Sender<Work>,
}

impl Worker {
    fn start(core: Arc<Core>) -> Self {
        let (tx, rx) = mpsc::channel::<Work>();
        let spawned = std::thread::Builder::new().name("suite-worker".into()).spawn(move || {
            for work in rx {
                work(&core);
            }
        });
        if let Err(e) = spawned {
            tracing::error!("no worker thread: {e}");
        }
        Self { tx }
    }

    fn run(&self, work: impl FnOnce(&Core) + Send + 'static) {
        let _ = self.tx.send(Box::new(work));
    }
}

/// The app window and everything behind it.
pub struct App {
    inner: Rc<Inner>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App").finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    me: Weak<Inner>,
    pub(crate) core: Arc<Core>,
    pub(crate) ui: ui::AppWindow,
    worker: Worker,
    pub(crate) inbox: Arc<Inbox>,
    catalog: RefCell<Option<Arc<Catalog>>>,
    /// A newer catalog waiting for the queue to be idle.
    queue: RefCell<Option<Queue>>,
    pending: RefCell<Vec<Job>>,
    rows: RefCell<Vec<Row>>,
    facts: RefCell<HostFacts>,
    state: RefCell<State>,
    schedule: RefCell<Option<ScheduleStatus>>,
    pub(crate) jobs: RefCell<Jobs>,
    page_app: RefCell<Option<String>>,
    /// Channels chosen on the page of apps not installed yet.
    channels: RefCell<HashMap<String, Channel>>,
    icons: RefCell<HashMap<String, Image>>,
    qr: RefCell<HashMap<String, Image>>,
    prefs: RefCell<Prefs>,
    prefs_path: Option<PathBuf>,
    clock: Instant,
    tick: Timer,
    /// Held while jobs run, so the system doesn't slow them when the window is out of sight.
    activity: RefCell<Option<UserActivity>>,
    family: Rc<VecModel<ui::CardData>>,
    others: Rc<VecModel<ui::CardData>>,
    updates: Rc<VecModel<ui::UpdateItem>>,
    jobs_model: Rc<VecModel<ui::JobData>>,
    log_model: Rc<VecModel<ui::LogData>>,
    page_models: bind::PageModels,
    install_all: RefCell<Vec<String>>,
    pub(crate) compact: RefCell<Option<Compact>>,
    /// The main window was shown (not just the compact one).
    shown: Cell<bool>,
    first: RefCell<Option<timing::FirstFrame>>,
    loading: Cell<bool>,
    instance: RefCell<Option<instance::Primary>>,
}

impl App {
    /// Builds the window (not shown yet). `prefs_file` keeps theme and language; `None`
    /// keeps them in memory (tests, demos).
    pub fn new(core: Arc<Core>, prefs_file: Option<PathBuf>) -> Result<Self, PlatformError> {
        let prefs = Prefs::load(prefs_file.as_deref());
        look::set(prefs.theme);
        let language = i18n::resolve(prefs.language.as_deref());
        i18n::use_catalog(language);
        let ui = ui::AppWindow::new()?;
        i18n::select_markup(language);
        timing::step("window built");
        look::follow(&ui);
        let facts = HostFacts::example(Os::current());
        let inner = Rc::new_cyclic(|me| Inner {
            me: me.clone(),
            worker: Worker::start(Arc::clone(&core)),
            core,
            ui,
            inbox: Arc::default(),
            catalog: RefCell::default(),
            queue: RefCell::default(),
            pending: RefCell::default(),
            rows: RefCell::default(),
            facts: RefCell::new(facts),
            state: RefCell::default(),
            schedule: RefCell::default(),
            jobs: RefCell::default(),
            page_app: RefCell::default(),
            channels: RefCell::default(),
            icons: RefCell::default(),
            qr: RefCell::default(),
            prefs: RefCell::new(prefs),
            prefs_path: prefs_file,
            clock: Instant::now(),
            tick: Timer::default(),
            activity: RefCell::default(),
            family: Rc::default(),
            others: Rc::default(),
            updates: Rc::default(),
            jobs_model: Rc::default(),
            log_model: Rc::default(),
            page_models: bind::PageModels::default(),
            install_all: RefCell::default(),
            compact: RefCell::default(),
            shown: Cell::new(false),
            first: RefCell::default(),
            loading: Cell::new(true),
            instance: RefCell::default(),
        });
        CURRENT.with_borrow_mut(|c| *c = Rc::downgrade(&inner));
        inner.setup();
        Ok(Self { inner })
    }

    /// Shows the app window (or brings it to the front); the app list loads after its
    /// first frame.
    pub fn show(&self) -> Result<(), PlatformError> {
        self.inner.show_main()
    }

    /// Opens the compact install window for one app (`stockroom install <app> --ui`).
    pub fn install_window(&self, app: &str, channel: Option<Channel>) -> Result<(), PlatformError> {
        self.inner.open_compact(app, channel)
    }

    /// Starts loading the app list without a window on screen (tests).
    pub fn load(&self) {
        self.inner.load(true);
    }

    pub fn window(&self) -> &ui::AppWindow {
        &self.inner.ui
    }

    /// The compact window, if open.
    pub fn compact_window(&self) -> Option<ui::InstallWindow> {
        self.inner.compact.borrow().as_ref().map(|c| c.ui.clone_strong())
    }

    /// Feeds an event as if the queue had sent it (demos show work in progress this way).
    pub fn inject(&self, ev: CoreEvent) {
        self.inner.core_event(ev);
    }

    /// Feeds download progress as the queue would.
    pub fn inject_progress(&self, id: u64, done: u64, total: u64) {
        self.inner.inbox.progress(id, done, total);
        self.inner.ensure_tick();
    }

    /// 0 apps, 1 an app's page (`open_page`), 2 activity, 3 settings.
    pub fn show_page(&self, page: i32) {
        self.inner.show_page(page);
    }

    pub fn open_page(&self, app: &str) {
        self.inner.open_page(app);
    }

    /// Applies a theme and a language (a code from the bundled ones, or `None` for the
    /// system's) as Settings would.
    pub fn set_look(&self, theme: ThemeChoice, language: Option<&str>) {
        self.inner.set_theme(theme);
        let index = i18n::index_of(language);
        self.inner.set_language(i32::try_from(index).unwrap_or_default());
    }

    pub(crate) fn hold_instance(&self, primary: instance::Primary) {
        self.inner.instance.replace(Some(primary));
    }

    pub(crate) fn inbox(&self) -> Arc<Inbox> {
        Arc::clone(&self.inner.inbox)
    }

    /// Jobs queued or running.
    pub fn active_jobs(&self) -> usize {
        self.inner.jobs.borrow().active()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.inner.tick.stop();
        if let Some(q) = self.inner.queue.take() {
            // Lets a running job end cleanly rather than stopping mid-install.
            q.finish();
        }
    }
}

impl Inner {
    #[expect(clippy::too_many_lines, reason = "one place wires every callback")]
    fn setup(&self) {
        let ui = &self.ui;
        ui.set_product(suite_identity::NAME.into());
        ui.set_title_bar(crate::title_bar());
        ui.set_family(ModelRc::from(Rc::clone(&self.family)));
        ui.set_others(ModelRc::from(Rc::clone(&self.others)));
        ui.set_updates(ModelRc::from(Rc::clone(&self.updates)));
        ui.set_jobs(ModelRc::from(Rc::clone(&self.jobs_model)));
        ui.set_log(ModelRc::from(Rc::clone(&self.log_model)));
        ui.set_all_users_available(Core::all_users_supported());
        self.render_settings();

        macro_rules! on {
            ($set:ident, |$i:ident $(, $arg:ident: $t:ty)*| $body:expr) => {{
                let me = self.me.clone();
                ui.$set(move |$($arg: $t),*| {
                    if let Some($i) = me.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_show_page, |i, page: i32| i.show_page(page));
        on!(on_refresh, |i| i.load(false));
        on!(on_open_page, |i, id: slint::SharedString| i.open_page(&id));
        on!(on_act, |i, id: slint::SharedString| i.act(&id));
        on!(on_install_all_clicked, |i| {
            let ids = i.install_all.borrow().clone();
            for id in ids {
                let channel = i.channels.borrow().get(&id).copied();
                i.submit(Job::Install { app: id, channel });
            }
        });
        on!(on_update_all, |i| {
            let ids: Vec<String> = i
                .rows
                .borrow()
                .iter()
                .filter(|r| r.core.update_available().is_some())
                .map(|r| r.id().to_owned())
                .collect();
            for app in ids {
                i.submit(Job::Update { app });
            }
        });
        on!(on_open_url, |i, url: slint::SharedString| {
            let url = url.to_string();
            let inbox = Arc::clone(&i.inbox);
            i.worker.run(move |core| {
                if let Err(e) = core.open_url(&url) {
                    inbox.post(Msg::Problem { what: Problem::Open, detail: e.to_string() });
                }
            });
        });
        on!(on_open_other, |i, id: slint::SharedString, index: i32| i.open_other(&id, index));
        on!(on_set_channel, |i, id: slint::SharedString, index: i32| {
            let channel = if index == 1 { Channel::Beta } else { Channel::Stable };
            let managed = i.row(&id).is_some_and(|r| r.managed());
            if managed {
                let id = id.to_string();
                i.change(move |core| core.set_channel(&id, channel));
            } else {
                i.channels.borrow_mut().insert(id.to_string(), channel);
                i.render_page();
            }
        });
        on!(on_set_policy, |i, id: slint::SharedString, index: i32| {
            let id = id.to_string();
            i.change(move |core| core.set_policy(&id, view::policy_from(index)));
        });
        on!(on_rollback, |i, id: slint::SharedString| i.submit(Job::Rollback { app: id.to_string() }));
        on!(on_remove, |i, id: slint::SharedString, purge: bool| {
            i.submit(Job::Remove { app: id.to_string(), purge });
        });
        on!(on_open_app, |i, id: slint::SharedString| i.open_app(&id));
        on!(on_retry, |i, id: i32| {
            let id = u64::try_from(id).unwrap_or_default();
            let job = i.jobs.borrow().job(id).cloned();
            if let Some(job) = job {
                i.jobs.borrow_mut().dismiss(id);
                i.submit(job);
            }
        });
        on!(on_dismiss, |i, id: i32| {
            i.jobs.borrow_mut().dismiss(u64::try_from(id).unwrap_or_default());
            i.render();
        });
        on!(on_dismiss_notice, |i| {
            i.ui.set_notice("".into());
            i.ui.set_notice_detail("".into());
        });
        on!(on_set_auto_update, |i, on: bool| {
            i.change(move |core| core.set_auto_update(on, &core.settled_exe()).map(drop));
        });
        on!(on_set_skip_metered, |i, on: bool| i.change(move |core| core.set_skip_metered(on)));
        on!(on_set_scope, |i, index: i32| {
            let scope = if index == 1 { suite_core::Scope::AllUsers } else { suite_core::Scope::User };
            i.change(move |core| core.set_scope(scope));
        });
        on!(on_set_language, |i, index: i32| i.set_language(index));
        on!(on_set_theme, |i, index: i32| {
            let choice = ThemeChoice::ALL.get(usize::try_from(index).unwrap_or_default()).copied();
            i.set_theme(choice.unwrap_or_default());
        });

        let me = self.me.clone();
        ui.window().on_close_requested(move || {
            if let Some(i) = me.upgrade() {
                i.shown.set(false);
                // Jobs finish first; then Stockroom quits (see `maybe_quit`).
                slint::Timer::single_shot(Duration::ZERO, move || i.maybe_quit());
            }
            slint::CloseRequestResponse::HideWindow
        });
    }

    fn show_main(&self) -> Result<(), PlatformError> {
        let first_time = !self.shown.get() && self.first.borrow().is_none();
        if first_time {
            let first = timing::first_frame(self.ui.window(), "main");
            first.start();
            let me = self.me.clone();
            first.then(move || {
                if let Some(i) = me.upgrade() {
                    i.load(true);
                }
            });
            self.first.replace(Some(first));
        }
        self.shown.set(true);
        crate::show(&self.ui)?;
        self.ui.window().set_minimized(false);
        self.ui.window().with_winit_window(slint::winit_030::winit::window::Window::focus_window);
        Ok(())
    }

    /// Loads the app list: the cached copy first (unless `cached` is false), then a fresh one.
    pub(crate) fn load(&self, cached: bool) {
        self.loading.set(true);
        self.ui.set_loading(true);
        let inbox = Arc::clone(&self.inbox);
        let have = self.catalog.borrow().clone();
        self.worker.run(move |core| {
            timing::step("loading");
            match core.installer().recover() {
                Ok(fixed) => fixed.iter().for_each(|f| tracing::info!("repaired: {f}")),
                Err(e) => tracing::debug!("recovery: {e}"),
            }
            ensure_schedule(core);
            let mut shown = have;
            // The saved copy, or the list this build came with: something to show at once,
            // and to work with if the server can't be reached.
            if cached
                && shown.is_none()
                && let Some((c, origin)) = core.offline_catalog()
            {
                let c = Arc::new(c);
                inbox.post(Msg::Catalog { catalog: Arc::clone(&c), fresh: false });
                inbox.post(Msg::Snapshot(Box::new(snapshot(core, Some(&c)))));
                timing::step(match origin {
                    suite_core::ListOrigin::BuiltIn => "built-in catalog shown",
                    _ => "cached catalog shown",
                });
                shown = Some(c);
            }
            match core.load_catalog() {
                Ok(c) => {
                    let c = Arc::new(c);
                    inbox.post(Msg::Catalog { catalog: Arc::clone(&c), fresh: true });
                    inbox.post(Msg::Snapshot(Box::new(snapshot(core, Some(&c)))));
                    timing::step("fresh catalog shown");
                    icons_for(core, &c, &inbox);
                }
                Err(e) => {
                    tracing::warn!("catalog: {e}");
                    inbox.post(Msg::CatalogFailed { kind: e.kind(), detail: e.to_string() });
                    inbox.post(Msg::Snapshot(Box::new(snapshot(core, shown.as_deref()))));
                    if let Some(c) = &shown {
                        icons_for(core, c, &inbox);
                    }
                }
            }
        });
    }

    /// Re-reads state and rows (after a job or a setting changed).
    fn resnapshot(&self) {
        let inbox = Arc::clone(&self.inbox);
        let catalog = self.catalog.borrow().clone();
        self.worker.run(move |core| inbox.post(Msg::Snapshot(Box::new(snapshot(core, catalog.as_deref())))));
    }

    /// Runs a state change on the worker, then refreshes; failures show as a notice.
    fn change(&self, f: impl FnOnce(&Core) -> Result<(), suite_core::CoreError> + Send + 'static) {
        let inbox = Arc::clone(&self.inbox);
        let catalog = self.catalog.borrow().clone();
        self.worker.run(move |core| {
            if let Err(e) = f(core) {
                inbox.post(Msg::Problem { what: Problem::Setting, detail: e.to_string() });
            }
            inbox.post(Msg::Snapshot(Box::new(snapshot(core, catalog.as_deref()))));
        });
    }

    fn drain(&self) {
        loop {
            let next = lock(&self.inbox.messages).pop_front();
            let Some(msg) = next else { break };
            self.receive(msg);
        }
    }

    fn receive(&self, msg: Msg) {
        match msg {
            Msg::Catalog { catalog, fresh } => {
                if fresh {
                    self.loading.set(false);
                    self.ui.set_loading(false);
                    self.ui.set_problem("".into());
                    self.ui.set_problem_detail("".into());
                }
                self.catalog.replace(Some(Arc::clone(&catalog)));
                self.start_queue();
                if let Some(c) = self.compact.borrow_mut().as_mut()
                    && fresh
                {
                    c.catalog_ready();
                }
                self.submit_pending();
            }
            Msg::CatalogFailed { kind, detail } => {
                self.loading.set(false);
                self.ui.set_loading(false);
                let cached = self.catalog.borrow().is_some();
                self.ui.set_problem(words::catalog_problem(kind, cached).into());
                self.ui.set_problem_detail(detail.as_str().into());
                if let Some(c) = self.compact.borrow_mut().as_mut() {
                    if cached { c.catalog_ready() } else { c.failed(kind, &detail) }
                }
                self.submit_pending();
            }
            Msg::Snapshot(s) => {
                let Snapshot { rows, state, schedule, facts, has_rows } = *s;
                if has_rows {
                    self.rows.replace(rows);
                }
                self.state.replace(state);
                self.schedule.replace(Some(schedule));
                self.facts.replace(facts);
                self.render();
                self.render_settings();
            }
            Msg::Icon { app, pixels } => {
                self.icons.borrow_mut().insert(app, Image::from_rgba8(pixels));
                self.render();
            }
            Msg::Core(ev) => self.core_event(ev),
            Msg::Problem { what, detail } => {
                let text = match what {
                    Problem::Open => tr!("That couldn't be opened."),
                    Problem::Setting => tr!("That setting couldn't be changed."),
                };
                self.ui.set_notice(text.into());
                self.ui.set_notice_detail(detail.into());
            }
            Msg::Request(r) => self.request(&r),
        }
    }

    pub(crate) fn request(&self, r: &instance::Request) {
        let shown = match r {
            instance::Request::Show => self.show_main(),
            instance::Request::Install { app, channel } => self.open_compact(app, *channel),
        };
        if let Err(e) = shown {
            tracing::warn!("window: {e}");
        }
    }

    /// (Re)starts the queue on the newest catalog when it's idle.
    fn start_queue(&self) {
        if self.jobs.borrow().active() > 0 && self.queue.borrow().is_some() {
            return;
        }
        let Some(catalog) = self.catalog.borrow().clone() else { return };
        let mut queue = Queue::start(Arc::clone(&self.core), catalog);
        let events = queue.take_events();
        let inbox = Arc::clone(&self.inbox);
        let pump = std::thread::Builder::new().name("suite-events".into()).spawn(move || {
            for ev in events {
                match ev {
                    CoreEvent::Progress { id, done, total } => inbox.progress(id, done, total),
                    ev => inbox.post(Msg::Core(ev)),
                }
            }
        });
        if let Err(e) = pump {
            tracing::error!("no event thread: {e}");
        }
        if let Some(old) = self.queue.replace(Some(queue)) {
            old.finish();
        }
    }

    fn submit(&self, job: Job) {
        let id = self.queue.borrow_mut().as_mut().map(|q| q.submit(job.clone()));
        if id.is_none() {
            // Before the first catalog: queued here, submitted when it arrives.
            self.pending.borrow_mut().push(job);
            if !self.loading.get() {
                self.load(true);
            }
        }
    }

    fn submit_pending(&self) {
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        if self.queue.borrow().is_none() {
            if let Some(c) = self.compact.borrow_mut().as_mut() {
                c.no_catalog();
            }
            return;
        }
        for job in pending {
            self.submit(job);
        }
    }

    /// Submits `job` and returns its ID once the queue has one (the compact window).
    pub(crate) fn submit_now(&self, job: Job) -> Option<u64> {
        self.queue.borrow_mut().as_mut().map(|q| q.submit(job))
    }

    #[expect(clippy::needless_pass_by_value, reason = "events arrive owned from the inbox")]
    pub(crate) fn core_event(&self, ev: CoreEvent) {
        let now = self.clock.elapsed();
        let terminal = ev.is_terminal();
        let started = matches!(ev, CoreEvent::Started { .. });
        {
            let rows = self.rows.borrow();
            let name = |id: &str| {
                rows.iter().find(|r| r.id() == id).map_or_else(|| id.to_owned(), |r| r.name().to_owned())
            };
            self.jobs.borrow_mut().apply(&ev, now, &name);
        }
        let working = self.jobs.borrow().active() > 0;
        let mut activity = self.activity.borrow_mut();
        if working && activity.is_none() {
            *activity = Some(UserActivity::begin("Installing and updating apps"));
        } else if !working {
            *activity = None;
        }
        drop(activity);
        if let Some(c) = self.compact.borrow_mut().as_mut() {
            c.event(&ev, &self.jobs.borrow());
        }
        if started || matches!(ev, CoreEvent::Queued { .. }) {
            self.ensure_tick();
        }
        if terminal {
            self.resnapshot();
            if self.jobs.borrow().active() == 0 {
                // A catalog that arrived while jobs ran takes over now.
                self.start_queue();
                self.maybe_quit();
            }
        }
        self.render();
    }

    pub(crate) fn ensure_tick(&self) {
        if self.tick.running() {
            return;
        }
        let me = self.me.clone();
        self.tick.start(TimerMode::Repeated, TICK, move || {
            if let Some(i) = me.upgrade() {
                i.on_tick();
            }
        });
    }

    fn on_tick(&self) {
        let progress = std::mem::take(&mut *lock(&self.inbox.progress));
        let now = self.clock.elapsed();
        {
            let mut jobs = self.jobs.borrow_mut();
            for (id, (done, total)) in progress {
                jobs.progress(id, done, total);
            }
            jobs.tick(now);
        }
        if let Some(c) = self.compact.borrow_mut().as_mut() {
            c.tick(&self.jobs.borrow());
        }
        if self.jobs.borrow().active() == 0 {
            self.tick.stop();
        }
        self.render();
    }

    /// Quits once no window is open and no job is left.
    fn maybe_quit(&self) {
        let compact_open = self.compact.borrow().as_ref().is_some_and(Compact::visible);
        if !self.shown.get() && !compact_open && self.jobs.borrow().active() == 0 {
            let _ = slint::quit_event_loop();
        }
    }

    pub(crate) fn compact_closed(&self) {
        self.maybe_quit();
    }

    fn row(&self, id: &str) -> Option<Row> {
        self.rows.borrow().iter().find(|r| r.id() == id).cloned()
    }

    pub(crate) fn name_of(&self, id: &str) -> String {
        self.row(id).map_or_else(|| id.to_owned(), |r| r.name().to_owned())
    }

    /// The app's catalogue number ("AMZ-01"), or empty.
    pub(crate) fn part_of(&self, id: &str) -> String {
        self.row(id).and_then(|r| r.core.app.part.clone()).unwrap_or_default()
    }

    pub(crate) fn icon(&self, id: &str) -> Option<Image> {
        self.icons.borrow().get(id).cloned()
    }

    fn show_page(&self, page: i32) {
        if page != 1 {
            self.page_app.replace(None);
        }
        self.ui.set_page(page);
        if page == 1 {
            self.render_page();
        }
    }

    fn open_page(&self, id: &str) {
        self.page_app.replace(Some(id.to_owned()));
        self.render_page();
        self.ui.set_page(1);
    }

    /// From the install window: the app window, on one app's page.
    pub(crate) fn show_app(&self, id: &str) {
        self.open_page(id);
        if let Err(e) = self.show_main() {
            tracing::warn!("window: {e}");
        }
    }

    /// From the install window: the app window, on all the apps.
    pub(crate) fn show_all(&self) {
        self.show_page(0);
        if let Err(e) = self.show_main() {
            tracing::warn!("window: {e}");
        }
    }

    fn act(&self, id: &str) {
        let Some(row) = self.row(id) else { return };
        let busy = self.jobs.borrow().busy().contains_key(id);
        match row.act(busy) {
            Act::Install | Act::Store(_) => {
                let channel = self.channels.borrow().get(id).copied();
                self.submit(Job::Install { app: id.to_owned(), channel });
            }
            Act::Update => self.submit(Job::Update { app: id.to_owned() }),
            Act::Open => self.open_app(id),
            Act::None | Act::Busy => {}
        }
    }

    /// Starts an app Stockroom installed.
    pub(crate) fn launch(&self, id: &str) {
        let id = id.to_owned();
        let inbox = Arc::clone(&self.inbox);
        self.worker.run(move |core| {
            if let Err(e) = core.open_app(&id) {
                inbox.post(Msg::Problem { what: Problem::Open, detail: e.to_string() });
            }
        });
    }

    pub(crate) fn open_app(&self, id: &str) {
        let Some(row) = self.row(id) else { return };
        if row.managed() {
            self.launch(id);
        } else if let Some(i) =
            row.core.others.iter().position(|f| suite_core::can_open_found(&row.core.app, f))
        {
            self.open_other(id, i32::try_from(i).unwrap_or_default());
        }
    }

    fn open_other(&self, id: &str, index: i32) {
        let Some(row) = self.row(id) else { return };
        let Some(found) = usize::try_from(index).ok().and_then(|i| row.core.others.get(i)).cloned() else {
            return;
        };
        let app = row.core.app.clone();
        let inbox = Arc::clone(&self.inbox);
        self.worker.run(move |core| {
            if let Err(e) = core.open_found(&app, &found) {
                inbox.post(Msg::Problem { what: Problem::Open, detail: e.to_string() });
            }
        });
    }

    fn set_theme(&self, choice: ThemeChoice) {
        look::set(choice);
        self.prefs.borrow_mut().theme = choice;
        self.prefs.borrow().save(self.prefs_path.as_deref());
        self.render_settings();
    }

    fn set_language(&self, index: i32) {
        let code = usize::try_from(index)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| i18n::LANGUAGES.get(i))
            .map(|l| l.0.to_owned());
        i18n::select(i18n::resolve(code.as_deref()));
        self.prefs.borrow_mut().language = code;
        self.prefs.borrow().save(self.prefs_path.as_deref());
        self.render();
        self.render_settings();
        if let Some(c) = self.compact.borrow_mut().as_mut() {
            c.tick(&self.jobs.borrow());
        }
    }

    /// Everything on the apps, page and activity screens.
    pub(crate) fn render(&self) {
        let busy = self.jobs.borrow().busy();
        let rows = self.rows.borrow();
        let home = view::home(&rows, &busy);
        let icons = self.icons.borrow();
        bind::sync(&self.family, home.family.iter().map(|c| bind::card(c, icons.get(&c.id))).collect());
        bind::sync(&self.others, home.others.iter().map(|c| bind::card(c, icons.get(&c.id))).collect());
        bind::sync(
            &self.updates,
            home.updates
                .iter()
                .map(|(id, name, versions)| ui::UpdateItem {
                    id: id.into(),
                    name: name.into(),
                    versions: versions.into(),
                })
                .collect(),
        );
        self.ui.set_updates_title(home.updates_title.into());
        self.ui.set_install_all(home.install_all_text.into());
        self.ui.set_stock(home.stock.into());
        self.install_all.replace(home.install_all);
        if let Some(c) = self.compact.borrow().as_ref() {
            let more = view::more(&rows, &c.app, crate::compact::MORE);
            c.offer(more.iter().map(|m| bind::card(m, icons.get(&m.id))).collect());
        }
        drop((rows, icons));

        let jobs = self.jobs.borrow();
        bind::sync(&self.jobs_model, jobs.views().iter().map(bind::job).collect());
        self.ui.set_running(i32::try_from(jobs.active()).unwrap_or_default());
        drop(jobs);
        let now = time::OffsetDateTime::now_utc();
        bind::sync(
            &self.log_model,
            view::log(&self.state.borrow().history, now).iter().map(bind::log).collect(),
        );
        self.render_page();
    }

    fn render_page(&self) {
        let Some(id) = self.page_app.borrow().clone() else { return };
        let Some(row) = self.row(&id) else { return };
        let busy = self.jobs.borrow().busy();
        let channel = self.channels.borrow().get(&id).copied().unwrap_or_default();
        let page = view::page(&row, busy.get(&id), &self.facts.borrow(), channel);
        let icon = self.icon(&id);
        let qr_cache = &self.qr;
        let code = |url: &str| {
            qr_cache
                .borrow_mut()
                .entry(url.to_owned())
                .or_insert_with(|| qr::image(url).map_or_default(Image::from_rgba8))
                .clone()
        };
        let data = self.page_models.page(&page, icon.as_ref(), &code);
        if self.ui.get_app() != data {
            self.ui.set_app(data);
        }
    }

    fn render_settings(&self) {
        let state = self.state.borrow();
        let s = &state.settings;
        self.ui.set_auto_update(s.auto_update);
        self.ui.set_skip_metered(s.skip_metered);
        self.ui.set_scope(i32::from(s.scope == suite_core::Scope::AllUsers));
        let note = match self.schedule.borrow().as_ref() {
            Some(sched) if s.auto_update && sched.enabled => {
                tr!("Checks once a day in the background, using {}.", words::scheduler(sched.kind))
            }
            Some(sched) if s.auto_update => {
                tr!("On, but not registered with {} yet.", words::scheduler(sched.kind))
            }
            _ => tr!("Off: apps update only when you say so."),
        };
        self.ui.set_auto_update_note(note.into());
        let mut languages = vec![tr!("System")];
        languages.extend(i18n::LANGUAGES.iter().map(|l| l.1.to_owned()));
        self.ui.set_languages(bind::strings(languages));
        let lang = i18n::index_of(self.prefs.borrow().language.as_deref());
        self.ui.set_language(i32::try_from(lang).unwrap_or_default());
        self.ui.set_theme(look::current().index());
    }

    pub(crate) fn open_compact(&self, app: &str, channel: Option<Channel>) -> Result<(), PlatformError> {
        if let Some(c) = self.compact.borrow().as_ref()
            && c.app == app
        {
            return c.show();
        }
        let compact = Compact::new(self.me.clone(), app, channel)?;
        compact.show()?;
        self.compact.replace(Some(compact));
        if self.catalog.borrow().is_some() && !self.loading.get() {
            if let Some(c) = self.compact.borrow_mut().as_mut() {
                c.catalog_ready();
            }
        } else if !self.loading.get() || self.first.borrow().is_none() {
            self.load(true);
        }
        Ok(())
    }
}

/// Registers the daily check when automatic updates are on but nothing is registered yet
/// (the default is on, PLAN.md §5).
fn ensure_schedule(core: &Core) {
    let on = core.state().is_ok_and(|s| s.settings.auto_update);
    if on && !Scheduler::new(core.env()).status().enabled {
        let exe = core.settled_exe();
        if let Err(e) = core.set_auto_update(true, &exe) {
            tracing::warn!("daily check not registered: {e}");
        }
    }
}

/// Rows, state and settings, read on the worker.
pub(crate) fn snapshot(core: &Core, catalog: Option<&Catalog>) -> Snapshot {
    let state = core.state().unwrap_or_else(|e| {
        tracing::warn!("state: {e}");
        State::new()
    });
    let rows = catalog.map_or_default(|c| match core.rows(c) {
        Ok(rows) => rows
            .into_iter()
            .map(|r| {
                let beta = core.plan(c, &r.app.id, Channel::Beta).ok().map(|(_, rel, _)| rel.version.clone());
                Row { core: r, beta }
            })
            .collect(),
        Err(e) => {
            tracing::warn!("rows: {e}");
            Vec::new()
        }
    });
    Snapshot {
        rows,
        state,
        schedule: Scheduler::new(core.env()).status(),
        facts: core.facts().clone(),
        has_rows: catalog.is_some(),
    }
}

fn icons_for(core: &Core, catalog: &Catalog, inbox: &Arc<Inbox>) {
    for app in &catalog.apps {
        match core.icon_file(app) {
            Ok(Some(path)) => {
                if let Some(pixels) = icons::load(&path) {
                    inbox.post(Msg::Icon { app: app.id.clone(), pixels });
                } else {
                    tracing::debug!(app = %app.id, "icon isn't a PNG Stockroom can show");
                }
            }
            Ok(None) => {}
            Err(e) => tracing::debug!(app = %app.id, "icon: {e}"),
        }
    }
}
