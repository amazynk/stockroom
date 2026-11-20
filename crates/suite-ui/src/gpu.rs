//! One wgpu device shared by every window, instead of Slint creating one per window.

use std::{
    pin::pin,
    sync::{OnceLock, mpsc},
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

use slint::wgpu_30::{WGPUConfiguration, wgpu};

static KEEP: OnceLock<(wgpu::Device, wgpu::Queue)> = OnceLock::new();

/// `STOCKROOM_RENDERER=software` draws every window in software, with no GPU setup at all.
pub(crate) const RENDERER_ENV: &str = "STOCKROOM_RENDERER";

/// A driver that hasn't made a device by then is taken as broken: windows draw in software.
const SETUP_LIMIT: Duration = Duration::from_secs(5);

/// GPU setup running on its own thread; it takes a few hundred milliseconds, mostly in the driver.
#[derive(Debug)]
pub struct Gpu(Option<mpsc::Receiver<Option<Shared>>>);

/// The parts of a [`WGPUConfiguration`], which itself can't cross threads.
struct Shared {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Gpu {
    /// Starts the setup; hand the result to [`crate::init`].
    pub fn prepare() -> Self {
        if forced_software() {
            return Self(None);
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let spawned = thread::Builder::new().name("suite-gpu".into()).spawn(move || {
            // Unread if setup took too long; the device is then dropped here, off the UI thread.
            let _ = tx.send(shared());
        });
        Self(spawned.is_ok().then_some(rx))
    }

    /// The shared device, or `None` to draw in software: none was asked for, none was found,
    /// or the driver hung.
    pub(crate) fn wait(self) -> Option<WGPUConfiguration> {
        let ready = match self.0?.recv_timeout(SETUP_LIMIT) {
            Ok(ready) => ready,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(after = ?SETUP_LIMIT, "GPU setup hung");
                None
            }
            // The thread panicked in the driver.
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        };
        let Shared { instance, adapter, device, queue } = ready?;
        // Slint drops its handles from a thread-local destructor at exit, where wgpu's own
        // thread-locals are already gone and dropping the queue aborts the process. Statics are
        // never dropped, so these references keep that last drop from happening.
        let _ = KEEP.set((device.clone(), queue.clone()));
        Some(WGPUConfiguration::Manual { instance, adapter, device, queue })
    }
}

/// `None` when no adapter is available; Slint then picks one per window itself.
fn shared() -> Option<Shared> {
    // Only the platform's own API (asking for all also initialises OpenGL, ~100 ms). OpenGL needs
    // the window's display when the instance is made, so it's left to Slint, which has it.
    let backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY) & wgpu::Backends::PRIMARY;
    if backends.is_empty() {
        return None;
    }
    device(backends)
}

fn device(backends: wgpu::Backends) -> Option<Shared> {
    let started = Instant::now();
    // No debug messenger unless asked for (`WGPU_DEBUG=1`, `WGPU_VALIDATION=1`): in debug builds
    // it logs every missing Vulkan driver on the system as an error.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: wgpu::InstanceFlags::empty().with_env(),
        backend_options: wgpu::BackendOptions::from_env_or_default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let made = started.elapsed();
    let adapter = ready(wgpu::util::initialize_adapter_from_env_or_default(&instance, None))?.ok()?;
    let found = started.elapsed();
    let limits = wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
    let descriptor = wgpu::DeviceDescriptor {
        label: Some("stockroom"),
        required_limits: limits,
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..wgpu::DeviceDescriptor::default()
    };
    let (device, queue) = ready(adapter.request_device(&descriptor))?.ok()?;
    tracing::debug!(
        target: "suite_ui::timing",
        backend = ?adapter.get_info().backend,
        instance = ?made,
        adapter = ?found.saturating_sub(made),
        device = ?started.elapsed().saturating_sub(found),
        "gpu ready"
    );
    Some(Shared { instance, adapter, device, queue })
}

/// Whether [`RENDERER_ENV`] asks for the software renderer.
pub(crate) fn forced_software() -> bool {
    std::env::var(RENDERER_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("software"))
}

/// Native wgpu resolves these futures immediately; anything else falls back.
fn ready<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(out) => Some(out),
        Poll::Pending => None,
    }
}
