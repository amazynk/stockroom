//! Built without the `gpu` feature: no wgpu, every window draws with Slint's software
//! renderer. Same interface as `gpu.rs`.

/// `STOCKROOM_RENDERER=software` (always the case in this build).
pub(crate) const RENDERER_ENV: &str = "STOCKROOM_RENDERER";

/// Nothing to set up.
#[derive(Debug, Clone, Copy)]
pub struct Gpu;

impl Gpu {
    pub fn prepare() -> Self {
        Self
    }
}
