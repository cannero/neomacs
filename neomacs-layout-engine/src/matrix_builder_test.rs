use super::*;
use neomacs_display_protocol::frame_glyphs::{CursorStyle, GlyphRowRole};
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
    builder.begin_window(1, 24, 80, Rect::new(0.0, 0.0, 640.0, 384.0), true);
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
    builder.begin_window(1, 3, 10, Rect::new(0.0, 0.0, 80.0, 48.0), true);

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
    builder.begin_window(1, 5, 20, Rect::new(0.0, 0.0, 160.0, 80.0), true);
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
    builder.begin_window(1, 5, 20, Rect::new(0.0, 0.0, 160.0, 80.0), true);
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
    builder.begin_window(1, 2, 10, Rect::new(0.0, 0.0, 80.0, 32.0), true);
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
    builder.begin_window(1, 2, 10, Rect::new(0.0, 0.0, 80.0, 32.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('x', 0, 0);
    builder.end_row();
    builder.end_window();

    builder.reset();

    let state = builder.finish(10, 2, 8.0, 16.0);
    assert!(state.window_matrices.is_empty());
}

#[test]
fn builder_captures_status_line_chars_directly() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 3, 80, Rect::new(0.0, 0.0, 640.0, 48.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('a', 0, 0);
    builder.end_row();
    builder.end_window();

    // Push status-line characters directly
    builder.begin_status_line_row(GlyphRowRole::ModeLine);
    builder.push_status_line_char('-', 5);
    builder.push_status_line_char('U', 5);
    builder.push_status_line_char(':', 5);

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
fn builder_status_line_empty_row_when_no_chars_pushed() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 2, 40, Rect::new(0.0, 0.0, 320.0, 32.0), true);
    builder.end_window();

    // Begin a status-line row but push no characters
    builder.begin_status_line_row(GlyphRowRole::ModeLine);

    let state = builder.finish(40, 2, 8.0, 16.0);
    let ml_row = &state.window_matrices[0].matrix.rows[2]; // appended row
    assert_eq!(ml_row.glyphs[GlyphArea::Text as usize].len(), 0);
}

#[test]
fn builder_status_line_no_window_is_noop() {
    let mut builder = GlyphMatrixBuilder::new();
    // No window started — should not panic
    assert!(!builder.begin_status_line_row(GlyphRowRole::ModeLine));
    builder.push_status_line_char('x', 0); // should be a no-op too
    let state = builder.finish(80, 24, 8.0, 16.0);
    assert!(state.window_matrices.is_empty());
}

#[test]
fn builder_left_margin_chars() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 3, 80, Rect::new(0.0, 0.0, 640.0, 48.0), true);
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
    builder.begin_window(1, 3, 80, Rect::new(0.0, 0.0, 640.0, 48.0), true);
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

/// Regression test for the face-id-collision bug that caused
/// both mode lines to render with mode-line-inactive colors
/// after `C-x 2`.
///
/// Reproduction: insert TWO faces into the builder's shared
/// `faces` HashMap at the SAME face_id. The first insertion (the
/// active mode-line) is overwritten by the second (the inactive
/// mode-line), and both window matrices that reference the id
/// then read the inactive colors.
///
/// Fix: `LayoutEngine::frame_face_id_counter` is frame-scoped
/// and NEVER resets per window, so sibling windows always get
/// distinct face ids and their entries never overwrite each
/// other. Mirrors GNU's single `face_cache->used` counter per
/// frame at `src/xfaces.c::init_frame_faces` / `realize_face`.
///
/// This test verifies the invariant at the builder level: when
/// the caller inserts two DIFFERENT faces at DIFFERENT ids, both
/// faces remain readable in the finished frame state. That is
/// the contract the face-id counter fix guarantees; without the
/// fix, the caller accidentally uses the SAME id and the second
/// insert wipes out the first.
#[test]
fn builder_preserves_distinct_mode_line_faces_across_sibling_windows() {
    use neomacs_display_protocol::face::Face;
    use neomacs_display_protocol::types::Color;

    let mut builder = GlyphMatrixBuilder::new();

    // Emulate the post-C-x-2 redisplay order: active mode-line
    // for the TOP (selected) window, then inactive mode-line for
    // the BOTTOM (non-selected) window. The `LayoutEngine`'s
    // `frame_face_id_counter` guarantees these receive DIFFERENT
    // ids; the builder must keep both in the `faces` HashMap.
    let mut active = Face::new(10);
    active.foreground = Color::rgb(0.0, 0.0, 0.0);
    active.background = Color::rgb(0.75, 0.75, 0.75);
    builder.insert_face(10, active.clone());

    let mut inactive = Face::new(11);
    inactive.foreground = Color::rgb(0.8, 0.8, 0.8);
    inactive.background = Color::rgb(0.30, 0.30, 0.30);
    builder.insert_face(11, inactive.clone());

    // Window 1 (top, selected): references the active face on
    // its mode-line row.
    builder.begin_window(1, 12, 80, Rect::new(0.0, 0.0, 640.0, 192.0), true);
    builder.begin_row(11, GlyphRowRole::ModeLine);
    builder.push_char('-', 10, 0);
    builder.end_row();
    builder.end_window();

    // Window 3 (bottom, not selected): references the inactive
    // face on its mode-line row. Before the fix, the engine
    // re-used face_id 10 here because `current_face_id` was a
    // per-window `let` binding that reset to 1 for every window.
    builder.begin_window(3, 12, 80, Rect::new(0.0, 192.0, 640.0, 192.0), false);
    builder.begin_row(11, GlyphRowRole::ModeLine);
    builder.push_char('-', 11, 0);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(80, 25, 8.0, 16.0);

    // Both faces must survive into the finished frame state. If
    // they were inserted at the same id, one would clobber the
    // other and this assertion would fail.
    let stored_active = state
        .faces
        .get(&10)
        .expect("face id 10 (active mode-line) must remain in the faces map");
    let stored_inactive = state
        .faces
        .get(&11)
        .expect("face id 11 (inactive mode-line) must remain in the faces map");

    assert_eq!(
        stored_active.background, active.background,
        "active mode-line background must not be overwritten by sibling window's face insertion"
    );
    assert_eq!(
        stored_inactive.background, inactive.background,
        "inactive mode-line background must remain distinct"
    );
    assert_ne!(
        stored_active.background, stored_inactive.background,
        "sibling mode lines must have different background colors"
    );
}
