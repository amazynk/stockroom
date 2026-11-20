//! A Slint platform with no display: windows draw into memory with the software renderer,
//! and the event loop runs timers and `invoke_from_event_loop` work. The tests and the
//! demo's window snapshots use it, so nothing ever appears on (or is read from) the screen.

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use slint::{
    PlatformError,
    platform::{
        EventLoopProxy, Platform, WindowAdapter,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    },
};

type Task = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Shared {
    tasks: Mutex<VecDeque<Task>>,
    quit: AtomicBool,
}

/// Install with [`Headless::install`] before creating any window; per thread.
pub struct Headless {
    shared: Arc<Shared>,
    windows: Windows,
}

impl std::fmt::Debug for Headless {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Headless").finish_non_exhaustive()
    }
}

/// The windows made so far, to draw, and how many frames they drew.
#[derive(Clone, Default)]
pub struct Windows(Rc<RefCell<Vec<Rc<MinimalSoftwareWindow>>>>, Rc<std::cell::Cell<usize>>);

impl std::fmt::Debug for Windows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Windows").field(&self.0.borrow().len()).field(&self.1.get()).finish()
    }
}

impl Windows {
    /// Draws every window that asked for a frame; true if any did.
    pub fn draw(&self) -> bool {
        let mut drew = false;
        for w in self.0.borrow().iter() {
            drew |= w.draw_if_needed(|renderer| {
                let size = w.size();
                let mut pixels = vec![
                    slint::platform::software_renderer::PremultipliedRgbaColor::default();
                    (size.width * size.height) as usize
                ];
                renderer.render(&mut pixels, size.width as usize);
            });
        }
        if drew {
            self.1.set(self.1.get() + 1);
        }
        drew
    }

    /// Frames drawn so far (any window).
    pub fn frames(&self) -> usize {
        self.1.get()
    }
}

struct Proxy(Arc<Shared>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.0.quit.store(true, Ordering::Release);
        Ok(())
    }

    fn invoke_from_event_loop(&self, event: Task) -> Result<(), slint::EventLoopError> {
        self.0.tasks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push_back(event);
        Ok(())
    }
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let w = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        w.set_size(slint::PhysicalSize::new(1120, 760));
        self.windows.0.borrow_mut().push(Rc::clone(&w));
        Ok(w as Rc<dyn WindowAdapter>)
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(Arc::clone(&self.shared))))
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        self.shared.quit.store(false, Ordering::Release);
        let windows = self.windows.clone();
        while !self.shared.quit.load(Ordering::Acquire) {
            pump(&self.shared);
            slint::platform::update_timers_and_animations();
            windows.draw();
            let wait = slint::platform::duration_until_next_timer_update()
                .unwrap_or(Duration::from_millis(10))
                .min(Duration::from_millis(10));
            std::thread::sleep(wait);
        }
        Ok(())
    }
}

fn pump(shared: &Shared) {
    loop {
        let task = shared.tasks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front();
        match task {
            Some(t) => t(),
            None => break,
        }
    }
}

impl Headless {
    /// Makes this thread's Slint platform a headless one and returns its windows.
    pub fn install() -> Result<Windows, slint::PlatformError> {
        let windows = Windows::default();
        let platform = Self { shared: Arc::default(), windows: windows.clone() };
        slint::platform::set_platform(Box::new(platform)).map_err(|e| PlatformError::Other(e.to_string()))?;
        Ok(windows)
    }
}

/// Runs timers and queued work for `for_how_long`, drawing as needed (tests drive time).
pub fn spin(windows: &Windows, for_how_long: Duration) {
    let until = std::time::Instant::now() + for_how_long;
    while std::time::Instant::now() < until {
        slint::platform::update_timers_and_animations();
        // `invoke_from_event_loop` work runs inside the loop, so run a short one.
        slint::Timer::single_shot(Duration::ZERO, || {
            let _ = slint::quit_event_loop();
        });
        let _ = slint::run_event_loop();
        windows.draw();
        std::thread::sleep(Duration::from_millis(5));
    }
}
