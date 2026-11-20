//! Time from asking for a window to its first frame, logged under `suite_ui::timing`.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use slint::RenderingState;

static LAUNCH: OnceLock<Instant> = OnceLock::new();
static FIRST: AtomicBool = AtomicBool::new(true);

/// When the process started: the first frame of any window is also logged from here.
pub fn launched(at: Instant) {
    let _ = LAUNCH.set(at);
}

/// Logs a startup step, measured from [`launched`].
pub fn step(name: &str) {
    if let Some(at) = LAUNCH.get() {
        let ms = at.elapsed().as_secs_f64() * 1000.0;
        let cpu = thread_cpu_ms().map_or_else(String::new, |ms| ms.to_string());
        tracing::info!(target: "suite_ui::timing", step = name, ms = format!("{ms:.0}"), cpu, "since launch");
    }
}

/// CPU time this thread has used, which unlike wall time doesn't depend on machine load (Linux).
fn thread_cpu_ms() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    let ns: u64 = stat.split_whitespace().next()?.parse().ok()?;
    Some(ns / 1_000_000)
}

/// Arm with [`FirstFrame::start`] just before showing the window.
#[derive(Clone, Default)]
pub(crate) struct FirstFrame(Rc<Armed>);

#[derive(Default)]
struct Armed {
    since: Cell<Option<Instant>>,
    /// Runs once after the next first frame.
    then: RefCell<Option<Box<dyn FnOnce()>>>,
    /// The renderer can't report frames (the software one).
    blind: Cell<bool>,
}

impl std::fmt::Debug for FirstFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("FirstFrame").field(&self.0.since.get()).finish()
    }
}

/// Without frame reports, work queued with [`FirstFrame::then`] waits this long instead.
const BLIND_WAIT: Duration = Duration::from_millis(200);

impl FirstFrame {
    pub(crate) fn start(&self) {
        self.0.since.set(Some(Instant::now()));
    }

    /// Runs `f` on the event loop once the armed first frame is on screen, so it doesn't delay it.
    pub(crate) fn then(&self, f: impl FnOnce() + 'static) {
        if self.0.blind.get() || self.0.since.get().is_none() {
            slint::Timer::single_shot(BLIND_WAIT, f);
        } else {
            *self.0.then.borrow_mut() = Some(Box::new(f));
        }
    }
}

pub(crate) fn first_frame(window: &slint::Window, name: &'static str) -> FirstFrame {
    let armed = FirstFrame::default();
    let inner = Rc::clone(&armed.0);
    let rate = Cell::new((Instant::now(), 0_u32));
    let installed = window.set_rendering_notifier(move |state, _| {
        if !matches!(state, RenderingState::AfterRendering) {
            if let Some(at) = inner.since.get() {
                let ms = at.elapsed().as_secs_f64() * 1000.0;
                tracing::debug!(target: "suite_ui::timing", window = name, ?state, ms = format!("{ms:.0}"), "first frame");
            }
            return;
        }
        let (from, frames) = rate.get();
        if from.elapsed() >= Duration::from_secs(5) {
            let per_s = f64::from(frames + 1) / from.elapsed().as_secs_f64();
            tracing::debug!(target: "suite_ui::timing", window = name, per_s = format!("{per_s:.1}"), "frames");
            rate.set((Instant::now(), 0));
        } else {
            rate.set((from, frames + 1));
        }
        if let Some(at) = inner.since.take() {
            let ms = at.elapsed().as_secs_f64() * 1000.0;
            tracing::info!(target: "suite_ui::timing", window = name, ms = format!("{ms:.0}"), "first frame");
            if FIRST.swap(false, Ordering::Relaxed) {
                step("first frame");
            }
            if let Some(then) = inner.then.borrow_mut().take() {
                slint::Timer::single_shot(Duration::ZERO, then);
            }
        }
    });
    if let Err(e) = installed {
        tracing::debug!(window = name, error = ?e, "no frame timing");
        armed.0.blind.set(true);
    }
    armed
}
