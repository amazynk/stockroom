//! Stockroom's app window (PLAN.md §6): the family's apps, each app's page, activity and
//! settings, in Slint with the family's look. It drives `suite-core` exactly as the CLI
//! does: the catalog, rows and settings on a worker thread, installs through the core's job
//! queue and its progress events.
//!
//! Engineering: the femtovg renderer on wgpu with a software fallback
//! (`STOCKROOM_RENDERER=software`), one `Theme` global for light and dark, bundled gettext
//! translations, frame timing, and no redraws while idle.

#![forbid(unsafe_code)]
#![recursion_limit = "256"]

mod app;
mod bind;
mod compact;
mod format;
#[cfg(feature = "gpu")]
mod gpu;
#[cfg(not(feature = "gpu"))]
#[path = "gpu_none.rs"]
mod gpu;
pub mod headless;
mod i18n;
mod icons;
mod instance;
mod jobs;
mod look;
mod meter;
mod prefs;
mod qr;
pub mod timing;
mod view;
mod words;

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

pub use app::App;
pub use gpu::Gpu;
pub use icons::encode as encode_png;
pub use look::ThemeChoice;
use suite_catalog::Channel;
use suite_core::Core;

#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("the window couldn't open: {0}")]
    Platform(#[from] slint::PlatformError),
    #[error("the window couldn't open: {0}")]
    EventLoop(#[from] slint::EventLoopError),
    #[error("single instance: {0}")]
    Instance(#[from] std::io::Error),
}

/// `stockroom install <app> --ui`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleApp {
    pub app: String,
    pub channel: Option<Channel>,
}

/// What the `stockroom` binary hands the window.
#[derive(Debug, Clone)]
pub struct UiOptions {
    pub core: Arc<Core>,
    /// Only the compact install window for this app.
    pub single_app: Option<SingleApp>,
    /// When the process started, for the startup timings.
    pub started: Instant,
}

/// Opens the window (or hands the request to Stockroom window already open) and returns
/// when it closes.
pub fn run(options: UiOptions) -> Result<(), UiError> {
    timing::launched(options.started);
    // GPU setup takes a while in the driver; it overlaps the rest of startup.
    let gpu = Gpu::prepare();
    let data = options.core.env().paths.data.clone();
    let request = match &options.single_app {
        None => instance::Request::Show,
        Some(s) => instance::Request::Install { app: s.app.clone(), channel: s.channel },
    };
    let inbox: Arc<Mutex<Option<Arc<app::Inbox>>>> = Arc::default();
    let to = Arc::clone(&inbox);
    let start = instance::start(&data, &request, move |r| {
        let inbox = to.lock().ok().and_then(|i| i.clone());
        if let Some(inbox) = inbox {
            inbox.post(app::Msg::Request(r));
        }
    })?;
    let instance::Start::Primary(primary) = start else {
        tracing::info!("handed to Stockroom window already open");
        return Ok(());
    };
    timing::step("single instance");
    init_with(gpu)?;
    let app = App::new(options.core, Some(prefs::path_in(&data)))?;
    app.hold_instance(primary);
    if let Ok(mut slot) = inbox.lock() {
        *slot = Some(app.inbox());
    }
    match &options.single_app {
        None => app.show()?,
        Some(s) => app.install_window(&s.app, s.channel)?,
    }
    slint::run_event_loop_until_quit()?;
    drop(app);
    Ok(())
}

/// Uses the wgpu renderer with one GPU device, or the software renderer where there is no
/// Vulkan, Metal or DX12 (or only a CPU adapter, or the driver hangs); call once before
/// creating a window. `STOCKROOM_RENDERER=software` forces the software renderer.
pub fn init() -> Result<(), slint::PlatformError> {
    init_with(Gpu::prepare())
}

/// Whether the windows draw under the system's title bar (macOS, on the real backend).
static UNDER_TITLE_BAR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The room to leave at the top of a window for the system's title bar, in logical pixels.
pub(crate) fn title_bar() -> f32 {
    if UNDER_TITLE_BAR.load(std::sync::atomic::Ordering::Relaxed) { 28.0 } else { 0.0 }
}

/// A backend selector for the real windows. On macOS the sheet runs up under the title bar
/// (no title, the three buttons over the paper), so the window is one piece of paper and
/// not a grey strip above one.
fn windows() -> slint::BackendSelector {
    let selector = slint::BackendSelector::new();
    #[cfg(target_os = "macos")]
    let selector = {
        use slint::winit_030::winit::platform::macos::WindowAttributesExtMacOS;
        UNDER_TITLE_BAR.store(true, std::sync::atomic::Ordering::Relaxed);
        selector.with_winit_window_attributes_hook(|attributes| {
            attributes
                .with_titlebar_transparent(true)
                .with_title_hidden(true)
                .with_fullsize_content_view(true)
        })
    };
    selector
}

/// [`init`] with GPU setup started earlier, so it overlaps other startup work.
pub fn init_with(gpu: Gpu) -> Result<(), slint::PlatformError> {
    #[cfg(feature = "gpu")]
    {
        let gpu = gpu.wait();
        timing::step("gpu device");
        if let Some(config) = gpu {
            let selected = windows().renderer_name("femtovg-wgpu".into()).require_wgpu_30(config).select();
            match selected {
                Ok(()) => return named(),
                Err(e) => tracing::warn!(error = %e, "GPU renderer unavailable; drawing in software"),
            }
        } else if gpu::forced_software() {
            tracing::info!("{}=software; drawing in software", gpu::RENDERER_ENV);
        } else {
            tracing::info!("no Vulkan, Metal or DX12 device; drawing in software");
        }
    }
    #[cfg(not(feature = "gpu"))]
    {
        let Gpu = gpu;
        tracing::info!(
            "built without GPU rendering ({} has no effect); drawing in software",
            gpu::RENDERER_ENV
        );
    }
    windows().renderer_name("software".into()).select()?;
    named()
}

/// Gives the windows Stockroom's app ID on Wayland and X11. It is the name of Stockroom's
/// desktop entry (`suite-install`'s `settle`), so docks and task switchers match the window
/// to the entry and show its icon. Elsewhere it does nothing.
fn named() -> Result<(), slint::PlatformError> {
    slint::set_xdg_app_id(suite_identity::APP_ID)
}

/// Shows a window and asks for its first frame: a window shown from work queued by another
/// thread isn't otherwise drawn, so Wayland never maps it.
pub(crate) fn show(window: &impl slint::ComponentHandle) -> Result<(), slint::PlatformError> {
    window.show()?;
    window.window().request_redraw();
    Ok(())
}
