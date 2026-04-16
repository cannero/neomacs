use super::*;
use crate::core::face::{Face, FaceAttributes};
use crate::core::types::Color;
use cosmic_text::{Family, Metrics, Style, SwashContent, SwashImage, Weight};

// ---------------------------------------------------------------
// Helper to construct a SwashImage for testing image_to_rgba
// ---------------------------------------------------------------
fn make_swash_image(content: SwashContent, width: u32, height: u32, data: Vec<u8>) -> SwashImage {
    SwashImage {
        content,
        placement: cosmic_text::Placement {
            left: 0,
            top: 0,
            width,
            height,
        },
        data,
        ..Default::default()
    }
}

// ---------------------------------------------------------------
// TextEngine construction and metrics
// ---------------------------------------------------------------

#[test]
fn test_text_engine_default_font_size() {
    let engine = TextEngine::new();
    assert_eq!(engine.default_font_size, 13.0);
    assert_eq!(engine.default_line_height, 17.0);
}

#[test]
fn test_text_engine_with_font_size() {
    let engine = TextEngine::with_font_size(16.0, 22.0);
    assert_eq!(engine.default_font_size, 16.0);
    assert_eq!(engine.default_line_height, 22.0);
}

#[test]
fn test_text_engine_metrics() {
    let engine = TextEngine::new();
    let metrics = engine.metrics();
    assert_eq!(metrics.font_size, 13.0);
    assert_eq!(metrics.line_height, 17.0);
}

#[test]
fn test_text_engine_metrics_custom() {
    let engine = TextEngine::with_font_size(24.0, 30.0);
    let metrics = engine.metrics();
    assert_eq!(metrics.font_size, 24.0);
    assert_eq!(metrics.line_height, 30.0);
}

#[test]
fn test_text_engine_default_trait() {
    let engine: TextEngine = Default::default();
    assert_eq!(engine.default_font_size, 13.0);
    assert_eq!(engine.default_line_height, 17.0);
}

// ---------------------------------------------------------------
// face_to_attrs: None face (default case)
// ---------------------------------------------------------------

#[test]
fn test_face_to_attrs_none_returns_monospace() {
    let mut engine = TextEngine::new();
    let attrs = engine.face_to_attrs(None);
    // When face is None, family should be Monospace
    assert_eq!(attrs.family, Family::Monospace);
}

#[test]
fn test_face_to_attrs_none_returns_white_color() {
    let mut engine = TextEngine::new();
    let attrs = engine.face_to_attrs(None);
    let expected = CosmicColor::rgba(255, 255, 255, 255);
    assert_eq!(attrs.color_opt, Some(expected));
}

// ---------------------------------------------------------------
// face_to_attrs: font family mapping
// ---------------------------------------------------------------

#[test]
fn test_face_to_attrs_monospace_family() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_family = "monospace".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::Monospace);
}

#[test]
fn test_face_to_attrs_mono_family() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_family = "mono".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::Monospace);
}

#[test]
fn test_face_to_attrs_serif_family() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_family = "serif".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::Serif);
}

#[test]
fn test_face_to_attrs_sans_serif_family() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_family = "sans-serif".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::SansSerif);
}

#[test]
fn test_face_to_attrs_sans_family() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_family = "sans".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::SansSerif);
}

#[test]
fn test_face_to_attrs_specific_family_uses_name() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_family = "DejaVu Sans Mono".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    // Specific font family names are passed through as Family::Name
    assert_eq!(attrs.family, Family::Name("DejaVu Sans Mono"));
}

#[test]
fn test_face_to_attrs_empty_family_defaults_to_monospace() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_family = String::new();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::Monospace);
}

#[test]
fn test_face_to_attrs_case_insensitive_family() {
    let mut engine = TextEngine::new();

    let mut face = Face::default();
    face.font_family = "MONOSPACE".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::Monospace);

    face.font_family = "Serif".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::Serif);

    face.font_family = "Sans-Serif".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::SansSerif);

    face.font_family = "MONO".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::Monospace);

    face.font_family = "SANS".to_string();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.family, Family::SansSerif);
}

// ---------------------------------------------------------------
// face_to_attrs: font weight
// ---------------------------------------------------------------

#[test]
fn test_face_to_attrs_weight_normal() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_weight = 400;
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.weight, Weight(400));
}

#[test]
fn test_face_to_attrs_weight_bold() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_weight = 700;
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.weight, Weight(700));
}

#[test]
fn test_face_to_attrs_weight_thin() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_weight = 100;
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.weight, Weight(100));
}

#[test]
fn test_face_to_attrs_weight_black() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_weight = 900;
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.weight, Weight(900));
}

// ---------------------------------------------------------------
// face_to_attrs: italic style
// ---------------------------------------------------------------

#[test]
fn test_face_to_attrs_italic() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.attributes |= FaceAttributes::ITALIC;
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.style, Style::Italic);
}

#[test]
fn test_face_to_attrs_not_italic() {
    let mut engine = TextEngine::new();
    let face = Face::default();
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.style, Style::Normal);
}

// ---------------------------------------------------------------
// face_to_attrs: foreground color
// ---------------------------------------------------------------

#[test]
fn test_face_to_attrs_foreground_color_white() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.foreground = Color::WHITE; // r=1.0, g=1.0, b=1.0, a=1.0
    let attrs = engine.face_to_attrs(Some(&face));
    let expected = CosmicColor::rgba(255, 255, 255, 255);
    assert_eq!(attrs.color_opt, Some(expected));
}

#[test]
fn test_face_to_attrs_foreground_color_red() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.foreground = Color::RED; // r=1.0, g=0.0, b=0.0, a=1.0
    let attrs = engine.face_to_attrs(Some(&face));
    let expected = CosmicColor::rgba(255, 0, 0, 255);
    assert_eq!(attrs.color_opt, Some(expected));
}

#[test]
fn test_face_to_attrs_foreground_color_green() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.foreground = Color::GREEN;
    let attrs = engine.face_to_attrs(Some(&face));
    let expected = CosmicColor::rgba(0, 255, 0, 255);
    assert_eq!(attrs.color_opt, Some(expected));
}

#[test]
fn test_face_to_attrs_foreground_color_blue() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.foreground = Color::BLUE;
    let attrs = engine.face_to_attrs(Some(&face));
    let expected = CosmicColor::rgba(0, 0, 255, 255);
    assert_eq!(attrs.color_opt, Some(expected));
}

#[test]
fn test_face_to_attrs_foreground_color_custom() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.foreground = Color::new(0.5, 0.25, 0.75, 0.8);
    let attrs = engine.face_to_attrs(Some(&face));
    let expected = CosmicColor::rgba(
        (0.5 * 255.0) as u8,  // 127
        (0.25 * 255.0) as u8, // 63
        (0.75 * 255.0) as u8, // 191
        (0.8 * 255.0) as u8,  // 204
    );
    assert_eq!(attrs.color_opt, Some(expected));
}

#[test]
fn test_face_to_attrs_foreground_color_black() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.foreground = Color::BLACK;
    let attrs = engine.face_to_attrs(Some(&face));
    let expected = CosmicColor::rgba(0, 0, 0, 255);
    assert_eq!(attrs.color_opt, Some(expected));
}

#[test]
fn test_face_to_attrs_foreground_transparent() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.foreground = Color::TRANSPARENT;
    let attrs = engine.face_to_attrs(Some(&face));
    let expected = CosmicColor::rgba(0, 0, 0, 0);
    assert_eq!(attrs.color_opt, Some(expected));
}

// ---------------------------------------------------------------
// face_to_attrs: combined attributes
// ---------------------------------------------------------------

#[test]
fn test_face_to_attrs_bold_italic_serif() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_family = "serif".to_string();
    face.font_weight = 700;
    face.attributes |= FaceAttributes::ITALIC;
    face.foreground = Color::RED;

    let attrs = engine.face_to_attrs(Some(&face));

    assert_eq!(attrs.family, Family::Serif);
    assert_eq!(attrs.weight, Weight(700));
    assert_eq!(attrs.style, Style::Italic);
    assert_eq!(attrs.color_opt, Some(CosmicColor::rgba(255, 0, 0, 255)));
}

// ---------------------------------------------------------------
// image_to_rgba: Mask content
// ---------------------------------------------------------------

#[test]
fn test_image_to_rgba_mask_no_face() {
    // 2x2 mask image with varying alpha
    let data = vec![0, 128, 255, 64];
    let image = make_swash_image(SwashContent::Mask, 2, 2, data);

    let pixels = image_to_rgba(&image, None);

    // Without a face, foreground defaults to white (255, 255, 255)
    assert_eq!(pixels.len(), 2 * 2 * 4);
    // Pixel 0: (255,255,255, 0)
    assert_eq!(pixels[0], 255); // r
    assert_eq!(pixels[1], 255); // g
    assert_eq!(pixels[2], 255); // b
    assert_eq!(pixels[3], 0); // a
    // Pixel 1: (255,255,255, 128)
    assert_eq!(pixels[4], 255);
    assert_eq!(pixels[5], 255);
    assert_eq!(pixels[6], 255);
    assert_eq!(pixels[7], 128);
    // Pixel 2: (255,255,255, 255)
    assert_eq!(pixels[8], 255);
    assert_eq!(pixels[9], 255);
    assert_eq!(pixels[10], 255);
    assert_eq!(pixels[11], 255);
    // Pixel 3: (255,255,255, 64)
    assert_eq!(pixels[12], 255);
    assert_eq!(pixels[13], 255);
    assert_eq!(pixels[14], 255);
    assert_eq!(pixels[15], 64);
}

#[test]
fn test_image_to_rgba_mask_with_face() {
    // 1x2 mask image
    let data = vec![200, 100];
    let image = make_swash_image(SwashContent::Mask, 1, 2, data);

    let mut face = Face::default();
    face.foreground = Color::new(1.0, 0.0, 0.0, 1.0); // Red

    let pixels = image_to_rgba(&image, Some(&face));

    assert_eq!(pixels.len(), 1 * 2 * 4);
    // Pixel 0: (255, 0, 0, 200)
    assert_eq!(pixels[0], 255);
    assert_eq!(pixels[1], 0);
    assert_eq!(pixels[2], 0);
    assert_eq!(pixels[3], 200);
    // Pixel 1: (255, 0, 0, 100)
    assert_eq!(pixels[4], 255);
    assert_eq!(pixels[5], 0);
    assert_eq!(pixels[6], 0);
    assert_eq!(pixels[7], 100);
}

#[test]
fn test_image_to_rgba_mask_half_color() {
    let data = vec![128];
    let image = make_swash_image(SwashContent::Mask, 1, 1, data);

    let mut face = Face::default();
    face.foreground = Color::new(0.5, 0.5, 0.5, 1.0);

    let pixels = image_to_rgba(&image, Some(&face));
    assert_eq!(pixels.len(), 4);
    // 0.5 * 255.0 = 127.5 -> 127 as u8
    assert_eq!(pixels[0], 127);
    assert_eq!(pixels[1], 127);
    assert_eq!(pixels[2], 127);
    assert_eq!(pixels[3], 128);
}

// ---------------------------------------------------------------
// image_to_rgba: Color content
// ---------------------------------------------------------------

#[test]
fn test_image_to_rgba_color_content() {
    // Color content: data is copied as-is (RGBA)
    let data = vec![
        10, 20, 30, 40, // pixel 0
        50, 60, 70, 80, // pixel 1
    ];
    let image = make_swash_image(SwashContent::Color, 2, 1, data.clone());

    let pixels = image_to_rgba(&image, None);
    assert_eq!(pixels, data);
}

#[test]
fn test_image_to_rgba_color_ignores_face() {
    // With Color content, face color should be irrelevant (data is copied as-is)
    let data = vec![10, 20, 30, 40];
    let image = make_swash_image(SwashContent::Color, 1, 1, data.clone());

    let mut face = Face::default();
    face.foreground = Color::RED;

    let pixels = image_to_rgba(&image, Some(&face));
    assert_eq!(pixels, data);
}

// ---------------------------------------------------------------
// image_to_rgba: SubpixelMask content
// ---------------------------------------------------------------

#[test]
fn test_image_to_rgba_subpixel_mask_no_face() {
    // SubpixelMask: 3 bytes per pixel (RGB subpixel), averaged for alpha
    let data = vec![
        60, 120, 180, // pixel 0: avg = (60+120+180)/3 = 120
        0, 0, 0, // pixel 1: avg = 0
    ];
    let image = make_swash_image(SwashContent::SubpixelMask, 2, 1, data);

    let pixels = image_to_rgba(&image, None);

    assert_eq!(pixels.len(), 2 * 1 * 4);
    // Pixel 0: white with alpha 120
    assert_eq!(pixels[0], 255);
    assert_eq!(pixels[1], 255);
    assert_eq!(pixels[2], 255);
    assert_eq!(pixels[3], 120);
    // Pixel 1: white with alpha 0
    assert_eq!(pixels[4], 255);
    assert_eq!(pixels[5], 255);
    assert_eq!(pixels[6], 255);
    assert_eq!(pixels[7], 0);
}

#[test]
fn test_image_to_rgba_subpixel_mask_with_face() {
    let data = vec![
        90, 90, 90, // avg = 90
    ];
    let image = make_swash_image(SwashContent::SubpixelMask, 1, 1, data);

    let mut face = Face::default();
    face.foreground = Color::new(0.0, 1.0, 0.0, 1.0); // Green

    let pixels = image_to_rgba(&image, Some(&face));
    assert_eq!(pixels.len(), 4);
    assert_eq!(pixels[0], 0); // r
    assert_eq!(pixels[1], 255); // g
    assert_eq!(pixels[2], 0); // b
    assert_eq!(pixels[3], 90); // alpha from average
}

#[test]
fn test_image_to_rgba_subpixel_mask_uneven_average() {
    // Test rounding: (100 + 200 + 50) / 3 = 116 (integer division)
    let data = vec![100, 200, 50];
    let image = make_swash_image(SwashContent::SubpixelMask, 1, 1, data);

    let pixels = image_to_rgba(&image, None);
    let avg = ((100u16 + 200 + 50) / 3) as u8; // 116
    assert_eq!(pixels[3], avg);
}

// ---------------------------------------------------------------
// image_to_rgba: edge cases
// ---------------------------------------------------------------

#[test]
fn test_image_to_rgba_mask_empty_data() {
    // 0x0 image (empty data)
    let image = make_swash_image(SwashContent::Mask, 0, 0, vec![]);
    let pixels = image_to_rgba(&image, None);
    assert!(pixels.is_empty());
}

#[test]
fn test_image_to_rgba_mask_single_pixel_full_alpha() {
    let data = vec![255];
    let image = make_swash_image(SwashContent::Mask, 1, 1, data);
    let pixels = image_to_rgba(&image, None);
    assert_eq!(pixels, vec![255, 255, 255, 255]);
}

#[test]
fn test_image_to_rgba_mask_single_pixel_zero_alpha() {
    let data = vec![0];
    let image = make_swash_image(SwashContent::Mask, 1, 1, data);
    let pixels = image_to_rgba(&image, None);
    assert_eq!(pixels, vec![255, 255, 255, 0]);
}

// ---------------------------------------------------------------
// RasterizedGlyph struct
// ---------------------------------------------------------------

#[test]
fn test_rasterized_glyph_fields() {
    let glyph = RasterizedGlyph {
        x: 10.5,
        y: 20.3,
        width: 8,
        height: 16,
        pixels: vec![1, 2, 3, 4],
    };
    assert_eq!(glyph.x, 10.5);
    assert_eq!(glyph.y, 20.3);
    assert_eq!(glyph.width, 8);
    assert_eq!(glyph.height, 16);
    assert_eq!(glyph.pixels.len(), 4);
}

#[test]
fn test_rasterized_glyph_debug() {
    let glyph = RasterizedGlyph {
        x: 0.0,
        y: 0.0,
        width: 1,
        height: 1,
        pixels: vec![0; 4],
    };
    let debug = format!("{:?}", glyph);
    assert!(debug.contains("RasterizedGlyph"));
}

// ---------------------------------------------------------------
// Rasterization integration tests (require FontSystem)
// ---------------------------------------------------------------

#[test]
fn test_rasterize_char_ascii_a() {
    let mut engine = TextEngine::new();
    let result = engine.rasterize_char('A', None);
    // ASCII 'A' should rasterize successfully with system fonts
    if let Some((w, h, pixels, _bx, _by)) = result {
        assert!(w > 0, "glyph width should be > 0");
        assert!(h > 0, "glyph height should be > 0");
        assert_eq!(
            pixels.len() as u32,
            w * h * 4,
            "pixel data should be w*h*4 RGBA bytes"
        );
    }
    // Note: on systems without fonts, result may be None; that's acceptable
}

#[test]
fn test_rasterize_char_with_face() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.foreground = Color::RED;
    let result = engine.rasterize_char('X', Some(&face));
    if let Some((w, h, pixels, _bx, _by)) = result {
        assert!(w > 0);
        assert!(h > 0);
        assert_eq!(pixels.len() as u32, w * h * 4);
        // At least some pixels should use the red foreground
        let has_red = pixels
            .chunks(4)
            .any(|rgba| rgba[0] > 0 && rgba[1] == 0 && rgba[2] == 0);
        if w > 0 && h > 0 {
            assert!(
                has_red,
                "rasterized glyph with red face should have red pixels"
            );
        }
    }
}

#[test]
fn test_rasterize_char_scaled() {
    let mut engine = TextEngine::new();
    let result_1x = engine.rasterize_char_scaled('M', None, 1.0);
    let result_2x = engine.rasterize_char_scaled('M', None, 2.0);

    if let (Some((w1, h1, _, _, _)), Some((w2, h2, _, _, _))) = (result_1x, result_2x) {
        // At 2x scale, dimensions should be larger
        assert!(
            w2 >= w1 && h2 >= h1,
            "2x scaled glyph ({}x{}) should be >= 1x glyph ({}x{})",
            w2,
            h2,
            w1,
            h1
        );
    }
}

#[test]
fn test_rasterize_text_hello() {
    let mut engine = TextEngine::new();
    let glyphs = engine.rasterize_text("Hello", None);
    // "Hello" has 5 characters, but ligatures/kerning may affect count.
    // At minimum we should get some glyphs on a system with fonts.
    // On headless systems without fonts, this may be empty.
    if !glyphs.is_empty() {
        // Glyphs should have increasing x positions (left-to-right)
        for i in 1..glyphs.len() {
            assert!(
                glyphs[i].x >= glyphs[i - 1].x,
                "glyph {} x={} should be >= glyph {} x={}",
                i,
                glyphs[i].x,
                i - 1,
                glyphs[i - 1].x
            );
        }
        // Each glyph should have valid dimensions and pixel data
        for glyph in &glyphs {
            assert!(glyph.width > 0);
            assert!(glyph.height > 0);
            assert_eq!(glyph.pixels.len() as u32, glyph.width * glyph.height * 4,);
        }
    }
}

#[test]
fn test_rasterize_text_empty_string() {
    let mut engine = TextEngine::new();
    let glyphs = engine.rasterize_text("", None);
    assert!(glyphs.is_empty(), "empty string should produce no glyphs");
}

#[test]
fn test_rasterize_char_space() {
    let mut engine = TextEngine::new();
    let result = engine.rasterize_char(' ', None);
    // Space character typically has zero-size glyph (no visible pixels)
    // rasterize_char skips zero-size images, so result is likely None
    // This is expected behavior, not a failure
    if let Some((w, h, _, _, _)) = result {
        // If a space does rasterize, it should be valid
        assert!(w > 0 || h > 0);
    }
}

// ---------------------------------------------------------------
// face_to_attrs: stress / boundary values
// ---------------------------------------------------------------

#[test]
fn test_face_to_attrs_weight_zero() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_weight = 0;
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.weight, Weight(0));
}

#[test]
fn test_face_to_attrs_weight_max_u16() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();
    face.font_weight = u16::MAX;
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.weight, Weight(u16::MAX));
}

#[test]
fn test_face_to_attrs_foreground_boundary_colors() {
    let mut engine = TextEngine::new();
    let mut face = Face::default();

    // Color with all channels at 0.0 (but alpha 0 too)
    face.foreground = Color::new(0.0, 0.0, 0.0, 0.0);
    let attrs = engine.face_to_attrs(Some(&face));
    assert_eq!(attrs.color_opt, Some(CosmicColor::rgba(0, 0, 0, 0)));

    // Color near 1.0 (truncation edge: 0.999 * 255 = 254.745 -> 254)
    face.foreground = Color::new(0.999, 0.999, 0.999, 0.999);
    let attrs = engine.face_to_attrs(Some(&face));
    let expected_channel = (0.999 * 255.0) as u8;
    assert_eq!(
        attrs.color_opt,
        Some(CosmicColor::rgba(
            expected_channel,
            expected_channel,
            expected_channel,
            expected_channel
        ))
    );
}

// ---------------------------------------------------------------
// image_to_rgba: larger images
// ---------------------------------------------------------------

#[test]
fn test_image_to_rgba_mask_larger_image() {
    // 4x4 mask image
    let data: Vec<u8> = (0..16).collect();
    let image = make_swash_image(SwashContent::Mask, 4, 4, data.clone());

    let mut face = Face::default();
    face.foreground = Color::new(0.0, 0.0, 1.0, 1.0); // Blue

    let pixels = image_to_rgba(&image, Some(&face));
    assert_eq!(pixels.len(), 4 * 4 * 4);

    for (i, alpha) in data.iter().enumerate() {
        let offset = i * 4;
        assert_eq!(pixels[offset], 0, "r at pixel {}", i); // r = 0
        assert_eq!(pixels[offset + 1], 0, "g at pixel {}", i); // g = 0
        assert_eq!(pixels[offset + 2], 255, "b at pixel {}", i); // b = 255
        assert_eq!(pixels[offset + 3], *alpha, "a at pixel {}", i);
    }
}

#[test]
fn test_image_to_rgba_color_larger_image() {
    // 3x2 Color image (each pixel is 4 bytes RGBA)
    let data: Vec<u8> = (0..24).collect();
    let image = make_swash_image(SwashContent::Color, 3, 2, data.clone());
    let pixels = image_to_rgba(&image, None);
    assert_eq!(pixels, data);
}
