use super::*;
use neomacs_display_protocol::face::Face;

fn default_face() -> Face {
    Face::default()
}

#[test]
fn tty_char_advance_ascii_is_one_cell() {
    let mut be = TtyDisplayBackend::new();
    let f = default_face();
    assert_eq!(be.char_advance(&f, 'A'), 1.0);
    assert_eq!(be.char_advance(&f, ' '), 1.0);
    assert_eq!(be.char_advance(&f, '#'), 1.0);
}

#[test]
fn tty_char_advance_cjk_is_two_cells() {
    let mut be = TtyDisplayBackend::new();
    let f = default_face();
    // 中 = U+4E2D (CJK Unified Ideograph)
    assert_eq!(be.char_advance(&f, '中'), 2.0);
    // あ = U+3042 (Hiragana)
    assert_eq!(be.char_advance(&f, 'あ'), 2.0);
}

#[test]
fn tty_take_rows_drains() {
    let mut be = TtyDisplayBackend::new();
    // No rows yet.
    assert!(be.take_rows().is_empty());
}

#[test]
fn tty_trait_object_is_usable() {
    // Compile-time check: the trait is object-safe.
    let mut be: Box<dyn DisplayBackend> = Box::new(TtyDisplayBackend::new());
    let f = default_face();
    let _ = be.char_advance(&f, 'x');
    let _ = be.font_height(&f);
    let _ = be.font_width(&f);
}

// ----------- produce_glyph / finish_row -----------

use neomacs_display_protocol::frame_glyphs::GlyphRowRole;
use neomacs_display_protocol::glyph_matrix::GlyphType;

fn empty_row(role: GlyphRowRole) -> GlyphRow {
    let mut row = GlyphRow::new(role);
    row.mode_line = matches!(role, GlyphRowRole::ModeLine);
    row
}

#[test]
fn produce_char_glyph_accumulates_in_pending() {
    let mut be = TtyDisplayBackend::new();
    let f = default_face();
    be.produce_glyph(GlyphKind::Char('A'), &f, 0);
    be.produce_glyph(GlyphKind::Char('B'), &f, 1);
    be.produce_glyph(GlyphKind::Char('C'), &f, 2);
    assert_eq!(be.pending_glyphs().len(), 3);
    assert!(matches!(
        be.pending_glyphs()[0].glyph_type,
        GlyphType::Char { ch: 'A' }
    ));
    assert!(matches!(
        be.pending_glyphs()[1].glyph_type,
        GlyphType::Char { ch: 'B' }
    ));
}

#[test]
fn produce_char_glyph_uses_face_id() {
    let mut be = TtyDisplayBackend::new();
    let mut f = default_face();
    f.id = 42;
    be.produce_glyph(GlyphKind::Char('Z'), &f, 0);
    assert_eq!(be.pending_glyphs()[0].face_id, 42);
}

#[test]
fn produce_stretch_glyph_converts_pixels_to_cells() {
    let mut be = TtyDisplayBackend::new();
    let f = default_face();
    be.produce_glyph(
        GlyphKind::Stretch {
            width_px: 14.0,
            ascent: 1.0,
            descent: 0.0,
        },
        &f,
        0,
    );
    assert_eq!(be.pending_glyphs().len(), 1);
    match be.pending_glyphs()[0].glyph_type {
        GlyphType::Stretch { width_cols } => assert_eq!(width_cols, 14),
        _ => panic!("expected stretch glyph"),
    }
}

#[test]
fn finish_row_flushes_glyphs_into_text_area() {
    let mut be = TtyDisplayBackend::new();
    let f = default_face();
    be.produce_glyph(GlyphKind::Char('x'), &f, 0);
    be.produce_glyph(GlyphKind::Char('y'), &f, 1);
    be.produce_glyph(GlyphKind::Char('z'), &f, 2);
    be.finish_row(empty_row(GlyphRowRole::Text));
    // In-progress buffer drained.
    assert_eq!(be.pending_glyphs().len(), 0);
    let rows = be.take_rows();
    assert_eq!(rows.len(), 1);
    // The glyphs landed in the Text area (index 1).
    assert_eq!(rows[0].glyphs[1].len(), 3);
    assert_eq!(rows[0].glyphs[0].len(), 0); // left margin untouched
    assert_eq!(rows[0].glyphs[2].len(), 0); // right margin untouched
}

#[test]
fn finish_row_preserves_mode_line_flag() {
    let mut be = TtyDisplayBackend::new();
    be.finish_row(empty_row(GlyphRowRole::ModeLine));
    let rows = be.take_rows();
    assert!(rows[0].mode_line);
    assert!(matches!(rows[0].role, GlyphRowRole::ModeLine));
}

#[test]
fn display_text_plain_emits_chars_up_to_max_width() {
    let mut be = TtyDisplayBackend::new();
    let f = default_face();
    let advance = display_text_plain_via_backend(&mut be, "hello", &f, 1.0, 100.0);
    assert_eq!(advance, 5.0);
    assert_eq!(be.pending_glyphs().len(), 5);
}

#[test]
fn display_text_plain_stops_at_max_width() {
    let mut be = TtyDisplayBackend::new();
    let f = default_face();
    let advance = display_text_plain_via_backend(&mut be, "hello world", &f, 1.0, 5.0);
    assert_eq!(advance, 5.0);
    assert_eq!(be.pending_glyphs().len(), 5);
}

#[test]
fn display_text_plain_skips_newlines() {
    let mut be = TtyDisplayBackend::new();
    let f = default_face();
    display_text_plain_via_backend(&mut be, "a\nb\rc", &f, 1.0, 100.0);
    assert_eq!(be.pending_glyphs().len(), 3);
}

#[test]
fn display_text_plain_preserves_face_id() {
    let mut be = TtyDisplayBackend::new();
    let mut f = default_face();
    f.id = 7;
    display_text_plain_via_backend(&mut be, "ab", &f, 1.0, 100.0);
    assert_eq!(be.pending_glyphs()[0].face_id, 7);
    assert_eq!(be.pending_glyphs()[1].face_id, 7);
}

#[test]
fn multiple_rows_queue_in_order() {
    let mut be = TtyDisplayBackend::new();
    let f = default_face();
    be.produce_glyph(GlyphKind::Char('a'), &f, 0);
    be.finish_row(empty_row(GlyphRowRole::Text));
    be.produce_glyph(GlyphKind::Char('b'), &f, 0);
    be.finish_row(empty_row(GlyphRowRole::Text));
    let rows = be.take_rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].glyphs[1].len(), 1);
    assert_eq!(rows[1].glyphs[1].len(), 1);
}

// ----------- GuiDisplayBackend -----------

fn gui_face() -> Face {
    let mut f = Face::default();
    f.font_family = "monospace".to_string();
    f.font_size = 14.0;
    f.font_weight = 400;
    f
}

#[test]
fn gui_char_advance_returns_positive_pixel_width_for_ascii() {
    let mut svc = FontMetricsService::new();
    let mut be = GuiDisplayBackend::new(&mut svc);
    let f = gui_face();
    let w = be.char_advance(&f, 'M');
    assert!(
        w > 1.0,
        "GUI char_advance should be pixel-width (> cell-width), got {}",
        w
    );
}

#[test]
fn gui_font_height_is_positive_pixels() {
    let mut svc = FontMetricsService::new();
    let mut be = GuiDisplayBackend::new(&mut svc);
    let f = gui_face();
    assert!(be.font_height(&f) > 1.0);
}

#[test]
fn gui_font_width_is_positive_pixels() {
    let mut svc = FontMetricsService::new();
    let mut be = GuiDisplayBackend::new(&mut svc);
    let f = gui_face();
    assert!(be.font_width(&f) > 1.0);
}

#[test]
fn gui_produce_glyph_accumulates_like_tty() {
    let mut svc = FontMetricsService::new();
    let mut be = GuiDisplayBackend::new(&mut svc);
    let f = gui_face();
    be.produce_glyph(GlyphKind::Char('A'), &f, 0);
    be.produce_glyph(GlyphKind::Char('B'), &f, 1);
    assert_eq!(be.pending_glyphs().len(), 2);
}

#[test]
fn gui_finish_row_and_take_rows_work() {
    let mut svc = FontMetricsService::new();
    let mut be = GuiDisplayBackend::new(&mut svc);
    let f = gui_face();
    be.produce_glyph(GlyphKind::Char('x'), &f, 0);
    be.finish_row(empty_row(GlyphRowRole::Text));
    let rows = be.take_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].glyphs[1].len(), 1);
}

#[test]
fn gui_trait_object_is_usable() {
    let mut svc = FontMetricsService::new();
    let mut be: Box<dyn DisplayBackend> = Box::new(GuiDisplayBackend::new(&mut svc));
    let f = gui_face();
    let _ = be.char_advance(&f, 'x');
    let _ = be.font_height(&f);
    let _ = be.font_width(&f);
}

#[test]
fn gui_display_text_plain_via_backend_breaks_at_pixel_width() {
    // With a 14pt monospace face, the pixel advance per char is
    // significantly larger than 1.0; a max_width of 10 pixels
    // should break well before the string ends.
    let mut svc = FontMetricsService::new();
    let mut be = GuiDisplayBackend::new(&mut svc);
    let f = gui_face();
    let advance = display_text_plain_via_backend(&mut be, "xxxxxxxxxxxxxx", &f, 8.0, 10.0);
    assert!(
        advance <= 10.0,
        "advance {} should not exceed max_width 10.0",
        advance
    );
    assert!(
        be.pending_glyphs().len() < 14,
        "should break before emitting all 14 chars, got {}",
        be.pending_glyphs().len()
    );
}
