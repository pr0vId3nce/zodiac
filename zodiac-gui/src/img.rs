//! T_GFX_IMG pixel decode (roadmap 3.5) — the first place in zodiac that
//! actually decodes image bytes (the TUI re-emits them to the outer kitty
//! terminal verbatim). Formats mirror the kitty graphics protocol subset
//! the server mirrors: 100 = PNG, 24 = raw RGB, 32 = raw RGBA; raw data
//! may additionally be zlib-wrapped (o=z).

use std::io::Read as _;

/// Decode one mirrored image to tightly-packed RGBA8. `w`/`h` are the
/// header dimensions (may be 0 for PNG, where the file knows better).
/// Returns (width, height, rgba).
pub fn decode_rgba(
    format: u8,
    zlib: bool,
    w: u32,
    h: u32,
    data: &[u8],
) -> Option<(u32, u32, Vec<u8>)> {
    let inflated;
    let data = if zlib {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(data)
            .read_to_end(&mut out)
            .ok()?;
        inflated = out;
        &inflated[..]
    } else {
        data
    };
    match format {
        100 => decode_png(data),
        24 => {
            let n = (w as usize).checked_mul(h as usize)?;
            if data.len() < n * 3 || n == 0 {
                return None;
            }
            let mut rgba = Vec::with_capacity(n * 4);
            for px in data[..n * 3].chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            Some((w, h, rgba))
        }
        32 => {
            let n = (w as usize).checked_mul(h as usize)?;
            if data.len() < n * 4 || n == 0 {
                return None;
            }
            Some((w, h, data[..n * 4].to_vec()))
        }
        _ => None,
    }
}

fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(data);
    // Expand palette/1-2-4-bit/16-bit down to plain 8-bit channels.
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    let n = (w as usize).checked_mul(h as usize)?;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(n * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(n * 4);
            for &v in &buf {
                out.extend_from_slice(&[v, v, v, 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(n * 4);
            for px in buf.chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Indexed => return None, // normalize_to_color8 expands these
    };
    if rgba.len() != n * 4 {
        return None;
    }
    Some((w, h, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn raw_rgb_gets_opaque_alpha() {
        let data = [1u8, 2, 3, 4, 5, 6];
        let (w, h, rgba) = decode_rgba(24, false, 2, 1, &data).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn zlib_wrapped_raw_rgba_inflates() {
        let px = [9u8, 8, 7, 128];
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&px).unwrap();
        let z = enc.finish().unwrap();
        let (_, _, rgba) = decode_rgba(32, true, 1, 1, &z).unwrap();
        assert_eq!(rgba, px);
    }

    #[test]
    fn short_or_unknown_data_is_rejected() {
        assert!(decode_rgba(24, false, 4, 4, &[0u8; 3]).is_none());
        assert!(decode_rgba(7, false, 1, 1, &[0u8; 4]).is_none());
        assert!(decode_rgba(100, false, 0, 0, b"not a png").is_none());
    }
}
