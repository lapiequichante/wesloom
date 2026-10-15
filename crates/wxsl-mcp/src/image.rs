//! Screenshots as MCP image content: scaled down, PNG, base64.

/// Halve `rgba` (`width` × `height`) with a box filter until it is at most
/// `max_width` wide. Returns the pixels and their size.
pub(crate) fn fit(
    mut rgba: Vec<u8>,
    mut width: u32,
    mut height: u32,
    max_width: u32,
) -> (Vec<u8>, u32, u32) {
    while width > max_width.max(1) && width >= 2 && height >= 2 {
        let (half_w, half_h) = (width / 2, height / 2);
        let mut half = Vec::with_capacity((half_w * half_h * 4) as usize);
        for y in 0..half_h {
            for x in 0..half_w {
                for channel in 0..4 {
                    let at = |dx: u32, dy: u32| {
                        u32::from(
                            rgba[(((2 * y + dy) * width + 2 * x + dx) * 4 + channel) as usize],
                        )
                    };
                    half.push(((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8);
                }
            }
        }
        (rgba, width, height) = (half, half_w, half_h);
    }
    (rgba, width, height)
}

/// Encode tightly packed RGBA8 rows as a PNG.
pub(crate) fn png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    writer
        .write_image_data(rgba)
        .map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())?;
    Ok(bytes)
}

/// Standard base64, padded, as MCP's image content wants it.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (index, &byte)| {
            word | u32::from(byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(ALPHABET[(word >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (input, output) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), output);
        }
    }

    #[test]
    fn fitting_halves_until_narrow_enough_and_averages() {
        let rgba: Vec<u8> = [[0, 0, 0, 255], [255, 255, 255, 255]]
            .iter()
            .cycle()
            .take(8 * 4)
            .flatten()
            .copied()
            .collect();
        let (pixels, width, height) = fit(rgba, 8, 4, 3);
        assert_eq!((width, height), (2, 1));
        assert_eq!(&pixels[..4], &[128, 128, 128, 255]);
    }

    #[test]
    fn a_png_starts_with_its_signature() {
        let bytes = png(&[255; 16], 2, 2).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
