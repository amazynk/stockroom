//! App icons from the catalog: PNG files the core downloaded and checked against the
//! catalog's SHA-256, decoded with the pure-Rust `png` crate into Slint images.

use std::path::Path;

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Icons larger than this are refused rather than decoded.
const MAX_SIDE: u32 = 1024;

pub(crate) fn decode(bytes: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width, reader.info().height);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let data = &buf[..frame.buffer_size()];
    let channels = match frame.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return None,
    };
    let mut out = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    for (px, src) in out.make_mut_slice().iter_mut().zip(data.chunks_exact(channels)) {
        *px = match *src {
            [g] => Rgba8Pixel { r: g, g, b: g, a: 255 },
            [g, a] => Rgba8Pixel { r: g, g, b: g, a },
            [r, g, b] => Rgba8Pixel { r, g, b, a: 255 },
            [r, g, b, a] => Rgba8Pixel { r, g, b, a },
            _ => return None,
        };
    }
    Some(out)
}

pub(crate) fn load(path: &Path) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    decode(&std::fs::read(path).ok()?)
}

/// Encodes RGBA pixels as a PNG (the demo's window snapshots).
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_refuses_junk() {
        let rgba: Vec<u8> = (0..4 * 3 * 2).map(|i| u8::try_from(i * 10).unwrap()).collect();
        let png = encode(3, 2, &rgba).unwrap();
        let img = decode(&png).unwrap();
        assert_eq!((img.width(), img.height()), (3, 2));
        assert_eq!(img.as_slice()[1], Rgba8Pixel { r: 40, g: 50, b: 60, a: 70 });
        assert!(decode(b"not a png").is_none());
        assert!(decode(&encode(2000, 1, &vec![0; 8000]).unwrap()).is_none());
    }
}
