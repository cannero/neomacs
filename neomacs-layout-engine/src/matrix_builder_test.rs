use super::*;
use neomacs_display_protocol::frame_glyphs::{CursorStyle, FrameGlyph, GlyphRowRole};
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::types::{Color, Rect};

#[test]
fn builder_starts_empty() {
    let builder = GlyphMatrixBuilder::new();
    let state = builder.finish(80, 24, 8.0, 16.0);
    assert!(state.window_matrices.is_empty());
}

#[test]
fn builder_tracks_single_window_single_row() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 24, 80, Rect::new(0.0, 0.0, 640.0, 384.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('H', 0, 0);
    builder.push_char('i', 0, 1);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(80, 24, 8.0, 16.0);
    assert_eq!(state.window_matrices.len(), 1);
    let matrix = &state.window_matrices[0].matrix;
    assert_eq!(matrix.nrows, 24);
    assert_eq!(matrix.ncols, 80);
    assert_eq!(matrix.rows[0].used(GlyphArea::Text), 2);

    let g0 = &matrix.rows[0].glyphs[GlyphArea::Text as usize][0];
    assert_eq!(g0.glyph_type, GlyphType::Char { ch: 'H' });
    assert_eq!(g0.face_id, 0);
    assert_eq!(g0.charpos, 0);

    let g1 = &matrix.rows[0].glyphs[GlyphArea::Text as usize][1];
    assert_eq!(g1.glyph_type, GlyphType::Char { ch: 'i' });
    assert_eq!(g1.charpos, 1);
}

#[test]
fn builder_tracks_multiple_rows() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 3, 10, Rect::new(0.0, 0.0, 80.0, 48.0));

    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('a', 0, 0);
    builder.end_row();

    builder.begin_row(1, GlyphRowRole::Text);
    builder.push_char('b', 0, 5);
    builder.push_char('c', 0, 6);
    builder.end_row();

    builder.end_window();

    let state = builder.finish(10, 3, 8.0, 16.0);
    let matrix = &state.window_matrices[0].matrix;
    assert_eq!(matrix.rows[0].used(GlyphArea::Text), 1);
    assert_eq!(matrix.rows[1].used(GlyphArea::Text), 2);
    assert_eq!(matrix.rows[2].used(GlyphArea::Text), 0);
}

#[test]
fn builder_tracks_wide_chars() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 5, 20, Rect::new(0.0, 0.0, 160.0, 80.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_wide_char('\u{4e16}', 0, 0);
    builder.push_char('x', 0, 3);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(20, 5, 8.0, 16.0);
    let glyphs = &state.window_matrices[0].matrix.rows[0].glyphs[GlyphArea::Text as usize];
    assert_eq!(glyphs.len(), 3);
    assert!(glyphs[0].wide);
    assert!(!glyphs[0].padding);
    assert!(glyphs[1].padding);
    assert!(!glyphs[2].wide);
    assert!(!glyphs[2].padding);
}

#[test]
fn builder_handles_stretch_glyphs() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 5, 20, Rect::new(0.0, 0.0, 160.0, 80.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('a', 0, 0);
    builder.push_stretch(4, 0);
    builder.push_char('b', 0, 5);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(20, 5, 8.0, 16.0);
    let glyphs = &state.window_matrices[0].matrix.rows[0].glyphs[GlyphArea::Text as usize];
    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs[1].glyph_type, GlyphType::Stretch { width_cols: 4 });
}

#[test]
fn builder_computes_row_hashes_on_finish() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 2, 10, Rect::new(0.0, 0.0, 80.0, 32.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('x', 0, 0);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(10, 2, 8.0, 16.0);
    let row = &state.window_matrices[0].matrix.rows[0];
    assert_ne!(row.hash, 0, "hash should be computed on finish");
}

#[test]
fn builder_resets_on_new_frame() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 2, 10, Rect::new(0.0, 0.0, 80.0, 32.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('x', 0, 0);
    builder.end_row();
    builder.end_window();

    builder.reset();

    let state = builder.finish(10, 2, 8.0, 16.0);
    assert!(state.window_matrices.is_empty());
}

/// Helper: construct a minimal FrameGlyph::Char for testing.
fn make_test_frame_glyph(ch: char, window_id: i64, role: GlyphRowRole, face_id: u32) -> FrameGlyph {
    FrameGlyph::Char {
        window_id,
        row_role: role,
        clip_rect: None,
        char: ch,
        composed: None,
        x: 0.0,
        y: 0.0,
        baseline: 0.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        fg: Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
        bg: None,
        face_id,
        font_weight: 400,
        italic: false,
        font_size: 14.0,
        underline: 0,
        underline_color: None,
        strike_through: 0,
        strike_through_color: None,
        overline: 0,
        overline_color: None,
        overstrike: false,
    }
}

#[test]
fn builder_captures_status_line_from_buffer() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 3, 80, Rect::new(0.0, 0.0, 640.0, 48.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('a', 0, 0);
    builder.end_row();
    builder.end_window();

    // Simulate FrameGlyphBuffer contents with mode-line chars
    let glyphs = vec![
        make_test_frame_glyph('a', 1, GlyphRowRole::Text, 0),
        make_test_frame_glyph('-', 1, GlyphRowRole::ModeLine, 5),
        make_test_frame_glyph('U', 1, GlyphRowRole::ModeLine, 5),
        make_test_frame_glyph(':', 1, GlyphRowRole::ModeLine, 5),
        // Different window — should be filtered out
        make_test_frame_glyph('X', 2, GlyphRowRole::ModeLine, 5),
        // Header line for same window — should not appear in mode-line row
        make_test_frame_glyph('H', 1, GlyphRowRole::HeaderLine, 6),
    ];

    builder.push_status_line_from_buffer(&glyphs, GlyphRowRole::ModeLine, 1);

    let state = builder.finish(80, 3, 8.0, 16.0);
    let matrix = &state.window_matrices[0].matrix;

    // Original 3 rows + 1 appended mode-line row
    assert_eq!(matrix.nrows, 4);
    assert_eq!(matrix.rows.len(), 4);

    let ml_row = &matrix.rows[3];
    assert_eq!(ml_row.role, GlyphRowRole::ModeLine);
    assert!(ml_row.enabled);
    assert!(ml_row.mode_line);

    let ml_glyphs = &ml_row.glyphs[GlyphArea::Text as usize];
    assert_eq!(ml_glyphs.len(), 3);
    assert_eq!(ml_glyphs[0].glyph_type, GlyphType::Char { ch: '-' });
    assert_eq!(ml_glyphs[1].glyph_type, GlyphType::Char { ch: 'U' });
    assert_eq!(ml_glyphs[2].glyph_type, GlyphType::Char { ch: ':' });
    assert_eq!(ml_glyphs[0].face_id, 5);
}

#[test]
fn builder_status_line_ignores_other_windows() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 2, 40, Rect::new(0.0, 0.0, 320.0, 32.0));
    builder.end_window();

    // Only window 2 chars — nothing should be captured for window 1
    let glyphs = vec![
        make_test_frame_glyph('Z', 2, GlyphRowRole::ModeLine, 3),
    ];

    builder.push_status_line_from_buffer(&glyphs, GlyphRowRole::ModeLine, 1);

    let state = builder.finish(40, 2, 8.0, 16.0);
    let ml_row = &state.window_matrices[0].matrix.rows[2]; // appended row
    assert_eq!(ml_row.glyphs[GlyphArea::Text as usize].len(), 0);
}

#[test]
fn builder_status_line_no_window_is_noop() {
    let mut builder = GlyphMatrixBuilder::new();
    // No window started — should not panic
    builder.push_status_line_from_buffer(&[], GlyphRowRole::ModeLine, 1);
    let state = builder.finish(80, 24, 8.0, 16.0);
    assert!(state.window_matrices.is_empty());
}

#[test]
fn builder_left_margin_chars() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 3, 80, Rect::new(0.0, 0.0, 640.0, 48.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_left_margin_stretch(2, 1);
    builder.push_left_margin_char('4', 1);
    builder.push_left_margin_char('2', 1);
    builder.push_char('H', 0, 0);
    builder.push_char('i', 0, 1);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(80, 3, 8.0, 16.0);
    let row = &state.window_matrices[0].matrix.rows[0];

    // Left margin should have stretch + 2 digit chars
    let lm = &row.glyphs[GlyphArea::LeftMargin as usize];
    assert_eq!(lm.len(), 3);
    assert_eq!(lm[0].glyph_type, GlyphType::Stretch { width_cols: 2 });
    assert_eq!(lm[1].glyph_type, GlyphType::Char { ch: '4' });
    assert_eq!(lm[2].glyph_type, GlyphType::Char { ch: '2' });

    // Text area should have the buffer chars
    let text = &row.glyphs[GlyphArea::Text as usize];
    assert_eq!(text.len(), 2);
    assert_eq!(text[0].glyph_type, GlyphType::Char { ch: 'H' });
}

#[test]
fn builder_set_cursor_at_row() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 3, 80, Rect::new(0.0, 0.0, 640.0, 48.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('a', 0, 0);
    builder.end_row();
    builder.begin_row(1, GlyphRowRole::Text);
    builder.push_char('b', 0, 5);
    builder.end_row();

    // Set cursor on row 1, column 0
    builder.set_cursor_at_row(1, 0, CursorStyle::FilledBox);
    builder.end_window();

    let state = builder.finish(80, 3, 8.0, 16.0);
    let matrix = &state.window_matrices[0].matrix;

    assert!(matrix.rows[0].cursor_col.is_none());
    assert_eq!(matrix.rows[1].cursor_col, Some(0));
    assert_eq!(matrix.rows[1].cursor_type, Some(CursorStyle::FilledBox));
}
