use super::*;
use neomacs_display_protocol::frame_glyphs::GlyphRowRole;
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::types::Rect;

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
