//! Winit window icon utilities.
//!
//! Uses the project SVG icon (`assets/window-icon.svg`) and rasterizes it at
//! runtime for platform window APIs that require RGBA pixel buffers.

use winit::window::{Icon, Window};

const WINDOW_ICON_SVG: &[u8] = include_bytes!("../assets/window-icon.svg");
const WINDOW_ICON_SIZE: u32 = 256;

fn decode_svg_icon(data: &[u8], size: u32) -> Option<Icon> {
    let opts = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(data, &opts).ok()?;
    let svg_size = tree.size();
    let svg_w = svg_size.width();
    let svg_h = svg_size.height();
    if svg_w <= 0.0 || svg_h <= 0.0 || size == 0 {
        return None;
    }

    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let scale_x = size as f32 / svg_w;
    let scale_y = size as f32 / svg_h;
    let transform = resvg::tiny_skia::Transform::from_scale(scale_x, scale_y);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // tiny_skia emits premultiplied RGBA; un-premultiply for window icon APIs.
    let mut rgba = pixmap.take();
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as f32 / 255.0;
        if a > 0.0 && a < 1.0 {
            px[0] = (px[0] as f32 / a).min(255.0) as u8;
            px[1] = (px[1] as f32 / a).min(255.0) as u8;
            px[2] = (px[2] as f32 / a).min(255.0) as u8;
        }
    }

    Icon::from_rgba(rgba, size, size).ok()
}

fn load_window_icon() -> Option<Icon> {
    decode_svg_icon(WINDOW_ICON_SVG, WINDOW_ICON_SIZE)
}

/// Apply the Neomacs window icon to a winit window.
pub(crate) fn apply_window_icon(window: &Window) {
    if let Some(icon) = load_window_icon() {
        window.set_window_icon(Some(icon));
    } else {
        tracing::warn!("Failed to decode window icon SVG");
    }
}
