//! Pure Rust XBM (X BitMap) decoder
//!
//! Parses XBM format image data and produces RGBA pixel buffers.
//! XBM is a monochrome bitmap format using C source code syntax.

/// Decode XBM image from in-memory data, returning (width, height, rgba_pixels).
///
/// `fg` and `bg` are RGBA colors for set (1) and unset (0) bits respectively.
pub fn decode_xbm_data(
    data: &[u8],
    fg: [u8; 4],
    bg: [u8; 4],
    max_width: u32,
    max_height: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    let text = std::str::from_utf8(data).ok()?;
    let (width, height, bits) = parse_xbm(text)?;
    let rgba = render_xbm(width, height, &bits, fg, bg);
    constrain_and_return(width, height, rgba, max_width, max_height)
}

/// Decode XBM image from a file path.
pub fn decode_xbm_file(
    path: &std::path::Path,
    fg: [u8; 4],
    bg: [u8; 4],
    max_width: u32,
    max_height: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    let data = std::fs::read(path).ok()?;
    decode_xbm_data(&data, fg, bg, max_width, max_height)
}

/// Query XBM dimensions without full decode (header only).
pub fn query_xbm_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(data).ok()?;
    let (w, h) = parse_xbm_dimensions(text)?;
    Some((w, h))
}

/// Parse `#define name_width N` and `#define name_height N` from XBM text.
fn parse_xbm_dimensions(text: &str) -> Option<(u32, u32)> {
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#define ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() == 2 {
                if parts[0].ends_with("_width") {
                    width = parts[1].parse().ok();
                } else if parts[0].ends_with("_height") {
                    height = parts[1].parse().ok();
                }
            }
        }
        if width.is_some() && height.is_some() {
            break;
        }
    }

    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    }
}

/// Parse full XBM: dimensions + bit data.
fn parse_xbm(text: &str) -> Option<(u32, u32, Vec<u8>)> {
    let (width, height) = parse_xbm_dimensions(text)?;

    // Find the data array: look for `{ ... };`
    let brace_start = text.find('{')?;
    let brace_end = text[brace_start..].find('}')? + brace_start;
    let data_str = &text[brace_start + 1..brace_end];

    // Parse hex/decimal values
    let mut bytes = Vec::new();
    for token in data_str.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if let Some(val) = parse_c_integer(token) {
            bytes.push(val as u8);
        }
    }

    // Validate: need at least (width+7)/8 * height bytes
    let bytes_per_line = ((width + 7) / 8) as usize;
    let expected = bytes_per_line * height as usize;
    if bytes.len() < expected {
        tracing::warn!(
            "XBM: expected {} bytes for {}x{}, got {}",
            expected,
            width,
            height,
            bytes.len()
        );
        return None;
    }

    Some((width, height, bytes))
}

/// Parse a C integer literal (hex 0xNN or decimal).
fn parse_c_integer(s: &str) -> Option<u32> {
    let s = s.trim();
    // Strip trailing comments or other junk
    let s = s.trim_end_matches(|c: char| !c.is_ascii_hexdigit() && c != 'x' && c != 'X');
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Render XBM bit data to RGBA pixels.
/// XBM format: 1 bit per pixel, LSB first within each byte.
/// Set bits (1) get `fg` color, unset bits (0) get `bg` color.
fn render_xbm(width: u32, height: u32, bits: &[u8], fg: [u8; 4], bg: [u8; 4]) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let bytes_per_line = (w + 7) / 8;
    let mut rgba = vec![0u8; w * h * 4];

    for y in 0..h {
        for x in 0..w {
            let byte_idx = y * bytes_per_line + x / 8;
            let bit_idx = x % 8;
            let bit_set = (bits[byte_idx] >> bit_idx) & 1 != 0;
            let color = if bit_set { &fg } else { &bg };
            let idx = (y * w + x) * 4;
            rgba[idx] = color[0];
            rgba[idx + 1] = color[1];
            rgba[idx + 2] = color[2];
            rgba[idx + 3] = color[3];
        }
    }

    rgba
}

/// Apply size constraints and return.
fn constrain_and_return(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    max_width: u32,
    max_height: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    let mw = if max_width > 0 { max_width } else { 4096 };
    let mh = if max_height > 0 { max_height } else { 4096 };
    if width > mw || height > mh {
        let img = image::RgbaImage::from_raw(width, height, rgba)?;
        let mut nw = width;
        let mut nh = height;
        if nw > mw {
            nh = (nh as f64 * mw as f64 / nw as f64) as u32;
            nw = mw;
        }
        if nh > mh {
            nw = (nw as f64 * mh as f64 / nh as f64) as u32;
            nh = mh;
        }
        nw = nw.max(1);
        nh = nh.max(1);
        let resized = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
        Some((nw, nh, resized.into_raw()))
    } else {
        Some((width, height, rgba))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_xbm() {
        // A simple 8x2 XBM
        let xbm = b"#define test_width 8\n\
                     #define test_height 2\n\
                     static unsigned char test_bits[] = {\n\
                       0xff, 0x00 };\n";
        let fg = [255, 255, 255, 255]; // white
        let bg = [0, 0, 0, 255]; // black
        let result = decode_xbm_data(xbm, fg, bg, 0, 0);
        assert!(result.is_some());
        let (w, h, rgba) = result.unwrap();
        assert_eq!(w, 8);
        assert_eq!(h, 2);
        assert_eq!(rgba.len(), 8 * 2 * 4);
        // First row: all bits set -> all white
        for x in 0..8 {
            assert_eq!(&rgba[x * 4..x * 4 + 4], &fg);
        }
        // Second row: no bits set -> all black
        for x in 0..8 {
            let idx = (8 + x) * 4;
            assert_eq!(&rgba[idx..idx + 4], &bg);
        }
    }

    #[test]
    fn test_query_dimensions() {
        let xbm = b"#define icon_width 16\n#define icon_height 32\nstatic ...";
        let dims = query_xbm_dimensions(xbm);
        assert_eq!(dims, Some((16, 32)));
    }

    #[test]
    fn test_lsb_first() {
        // 4x1 XBM, byte 0x05 = 0b00000101 -> LSB first: bits 0,2 set
        let xbm = b"#define t_width 4\n\
                     #define t_height 1\n\
                     static unsigned char t_bits[] = { 0x05 };\n";
        let fg = [255, 0, 0, 255]; // red
        let bg = [0, 0, 255, 255]; // blue
        let result = decode_xbm_data(xbm, fg, bg, 0, 0);
        assert!(result.is_some());
        let (w, h, rgba) = result.unwrap();
        assert_eq!(w, 4);
        assert_eq!(h, 1);
        // Pixel 0: bit 0 set -> fg (red)
        assert_eq!(&rgba[0..4], &fg);
        // Pixel 1: bit 1 unset -> bg (blue)
        assert_eq!(&rgba[4..8], &bg);
        // Pixel 2: bit 2 set -> fg (red)
        assert_eq!(&rgba[8..12], &fg);
        // Pixel 3: bit 3 unset -> bg (blue)
        assert_eq!(&rgba[12..16], &bg);
    }

    #[test]
    fn test_hex_parsing() {
        assert_eq!(parse_c_integer("0xff"), Some(255));
        assert_eq!(parse_c_integer("0xFF"), Some(255));
        assert_eq!(parse_c_integer("0x0a"), Some(10));
        assert_eq!(parse_c_integer("255"), Some(255));
        assert_eq!(parse_c_integer("0"), Some(0));
    }

    #[test]
    fn test_custom_colors() {
        // 2x1 XBM, byte 0x01 -> bit 0 set, bit 1 unset
        let xbm = b"#define t_width 2\n\
                     #define t_height 1\n\
                     static unsigned char t_bits[] = { 0x01 };\n";
        let fg = [0, 255, 0, 255]; // green foreground
        let bg = [128, 128, 128, 255]; // gray background
        let result = decode_xbm_data(xbm, fg, bg, 0, 0);
        assert!(result.is_some());
        let (_, _, rgba) = result.unwrap();
        assert_eq!(&rgba[0..4], &fg); // bit 0 set -> green
        assert_eq!(&rgba[4..8], &bg); // bit 1 unset -> gray
    }
}
