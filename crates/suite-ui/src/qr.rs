//! "Get it on your phone": a QR code of a store link as a Slint image (the pure-Rust
//! `qrcode` crate, no image library).

use qrcode::{Color, QrCode};
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Light modules around the code, as the QR standard asks.
const QUIET: usize = 4;
/// Pixels per module; the markup scales it with nearest-neighbour sampling.
const SCALE: usize = 4;

/// The code's modules, row by row (true: dark), with the quiet zone; `None` if the text
/// doesn't fit in a QR code.
pub(crate) fn modules(text: &str) -> Option<(usize, Vec<bool>)> {
    let code = QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let size = width + 2 * QUIET;
    let mut out = vec![false; size * size];
    for (i, c) in colors.iter().enumerate() {
        let (x, y) = (i % width + QUIET, i / width + QUIET);
        out[y * size + x] = *c == Color::Dark;
    }
    Some((size, out))
}

/// Dark on white, whatever the theme: phone cameras want contrast.
pub(crate) fn image(text: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let (size, dark) = modules(text)?;
    let px = u32::try_from(size * SCALE).ok()?;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(px, px);
    let pixels = buffer.make_mut_slice();
    for (i, p) in pixels.iter_mut().enumerate() {
        let (x, y) = (i % (size * SCALE) / SCALE, i / (size * SCALE) / SCALE);
        let v = if dark[y * size + x] { 0x10 } else { 0xFF };
        *p = Rgba8Pixel { r: v, g: v, b: v, a: 0xFF };
    }
    Some(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_have_finder_patterns_and_a_quiet_zone() {
        let (size, m) = modules("https://play.google.com/store/apps/details?id=it.zynk").unwrap();
        assert!(size >= 21 + 2 * QUIET, "{size}");
        // The quiet zone is light all round.
        assert!((0..size).all(|i| !m[i] && !m[(size - 1) * size + i] && !m[i * size]));
        // The top-left finder pattern: a dark 7×7 ring with a light ring inside.
        let at = |x: usize, y: usize| m[(y + QUIET) * size + x + QUIET];
        assert!((0..7).all(|i| at(i, 0) && at(0, i) && at(6, i) && at(i, 6)));
        assert!((1..6).all(|i| !at(i, 1) && !at(1, i)));
        assert!(at(3, 3));
        let img = image("https://example.com").unwrap();
        assert_eq!(img.width() as usize % SCALE, 0);
        assert_eq!(img.width(), img.height());
    }
}
