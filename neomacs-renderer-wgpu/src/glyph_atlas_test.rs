use super::{
    FontconfigSubpixelOrder, GlyphKey, SubpixelBin, key_uses_default_font_metrics,
    normalize_subpixel_mask,
};

#[test]
fn normalize_subpixel_mask_preserves_rgb_order() {
    let out = normalize_subpixel_mask(&[10, 20, 30], 1, FontconfigSubpixelOrder::Rgb);
    assert_eq!(out, vec![10, 20, 30, 30]);
}

#[test]
fn normalize_subpixel_mask_swaps_bgr_order() {
    let out = normalize_subpixel_mask(&[10, 20, 30], 1, FontconfigSubpixelOrder::Bgr);
    assert_eq!(out, vec![30, 20, 10, 30]);
}

#[test]
fn default_metrics_ignore_nondefault_face_zero_font_size() {
    let key = GlyphKey {
        charcode: 'F' as u32,
        face_id: 0,
        font_size_bits: 27.0_f32.to_bits(),
        x_bin: SubpixelBin::Zero,
        y_bin: SubpixelBin::Zero,
    };

    assert!(!key_uses_default_font_metrics(&key, 13.0));
}

#[test]
fn default_metrics_accept_unspecified_default_font_size() {
    let key = GlyphKey {
        charcode: 'F' as u32,
        face_id: 0,
        font_size_bits: 0.0_f32.to_bits(),
        x_bin: SubpixelBin::Zero,
        y_bin: SubpixelBin::Zero,
    };

    assert!(key_uses_default_font_metrics(&key, 13.0));
}

#[test]
fn default_metrics_accept_explicit_default_font_size() {
    let key = GlyphKey {
        charcode: 'F' as u32,
        face_id: 0,
        font_size_bits: 13.05_f32.to_bits(),
        x_bin: SubpixelBin::Zero,
        y_bin: SubpixelBin::Zero,
    };

    assert!(key_uses_default_font_metrics(&key, 13.0));
}
