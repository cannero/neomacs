use super::*;
use neomacs_display_protocol::frame_glyphs::{
    CursorStyle, DisplaySlotId, GlyphRowRole, PhysCursor,
};
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
fn builder_stores_row_metrics_window_relative() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 2, 10, Rect::new(5.0, 20.0, 80.0, 40.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.set_current_row_metrics(26.0, 18.0, 13.0);
    builder.push_char('x', 0, 0);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(10, 2, 8.0, 16.0);
    let row = &state.window_matrices[0].matrix.rows[0];
    assert_eq!(row.pixel_y, 6.0);
    assert_eq!(row.height_px, 18.0);
    assert_eq!(row.ascent_px, 13.0);
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
fn builder_installs_status_line_row_glyphs_wholesale() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 3, 80, Rect::new(0.0, 0.0, 640.0, 48.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('a', 0, 0);
    builder.end_row();
    builder.end_window();

    // Install a complete set of text-area glyphs (the post-
    // Step-3.6 replacement for the old per-glyph
    // push_status_line_char API).
    builder.begin_status_line_row(GlyphRowRole::ModeLine);
    let glyphs = vec![
        Glyph::char('-', 5, 0),
        Glyph::char('U', 5, 0),
        Glyph::char(':', 5, 0),
    ];
    builder.install_status_line_row_glyphs(glyphs);

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
    // install_status_line_row_glyphs is also a no-op when no
    // window is pushed.
    builder.install_status_line_row_glyphs(vec![Glyph::char('x', 0, 0)]);
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

#[test]
fn builder_preserves_phys_cursor() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 3, 80, Rect::new(0.0, 0.0, 640.0, 48.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('a', 0, 0);
    builder.end_row();
    builder.set_phys_cursor(PhysCursor {
        window_id: 1,
        charpos: 0,
        row: 0,
        col: 0,
        slot_id: DisplaySlotId {
            window_id: 1,
            row: 0,
            col: 0,
        },
        x: 0.0,
        y: 0.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: neomacs_display_protocol::types::Color::WHITE,
        cursor_fg: neomacs_display_protocol::types::Color::BLACK,
    });
    builder.end_window();

    let state = builder.finish(80, 3, 8.0, 16.0);
    let cursor = state.phys_cursor.as_ref().expect("phys cursor");
    assert_eq!(cursor.window_id, 1);
    assert_eq!(cursor.charpos, 0);
    assert_eq!(cursor.col, 0);
}

#[test]
fn builder_reorders_simple_rtl_row() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 1, 10, Rect::new(0.0, 0.0, 80.0, 16.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('א', 0, 0);
    builder.push_char('ב', 0, 1);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(10, 1, 8.0, 16.0);
    let glyphs = &state.window_matrices[0].matrix.rows[0].glyphs[GlyphArea::Text as usize];
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].glyph_type, GlyphType::Char { ch: 'ב' });
    assert_eq!(glyphs[1].glyph_type, GlyphType::Char { ch: 'א' });
    assert_eq!(glyphs[0].bidi_level, 1);
    assert_eq!(glyphs[1].bidi_level, 1);
}

#[test]
fn builder_keeps_stretch_fixed_while_reordering_rtl_chars() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 1, 10, Rect::new(0.0, 0.0, 80.0, 16.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('א', 0, 0);
    builder.push_stretch(3, 0);
    builder.push_char('ב', 0, 1);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(10, 1, 8.0, 16.0);
    let glyphs = &state.window_matrices[0].matrix.rows[0].glyphs[GlyphArea::Text as usize];
    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs[0].glyph_type, GlyphType::Char { ch: 'ב' });
    assert_eq!(glyphs[1].glyph_type, GlyphType::Stretch { width_cols: 3 });
    assert_eq!(glyphs[2].glyph_type, GlyphType::Char { ch: 'א' });
    assert_eq!(glyphs[0].bidi_level, 1);
    assert_eq!(glyphs[1].bidi_level, 1);
    assert_eq!(glyphs[2].bidi_level, 1);
}

#[test]
fn builder_reorders_wide_rtl_row() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 1, 10, Rect::new(0.0, 0.0, 80.0, 16.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_wide_char('א', 0, 0);
    builder.push_char('ב', 0, 1);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(10, 1, 8.0, 16.0);
    let glyphs = &state.window_matrices[0].matrix.rows[0].glyphs[GlyphArea::Text as usize];
    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs[0].glyph_type, GlyphType::Char { ch: 'ב' });
    assert_eq!(glyphs[1].glyph_type, GlyphType::Char { ch: 'א' });
    assert!(glyphs[1].wide);
    assert!(glyphs[2].padding);
    assert_eq!(glyphs[0].bidi_level, 1);
    assert_eq!(glyphs[1].bidi_level, 1);
    assert_eq!(glyphs[2].bidi_level, 1);
}

#[test]
fn builder_reorders_wide_rtl_row_across_stretch() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 1, 10, Rect::new(0.0, 0.0, 80.0, 16.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_wide_char('א', 0, 0);
    builder.push_stretch(2, 0);
    builder.push_char('ב', 0, 1);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(10, 1, 8.0, 16.0);
    let glyphs = &state.window_matrices[0].matrix.rows[0].glyphs[GlyphArea::Text as usize];
    assert_eq!(glyphs.len(), 4);
    assert_eq!(glyphs[0].glyph_type, GlyphType::Char { ch: 'ב' });
    assert_eq!(glyphs[1].glyph_type, GlyphType::Stretch { width_cols: 2 });
    assert_eq!(glyphs[2].glyph_type, GlyphType::Char { ch: 'א' });
    assert!(glyphs[2].wide);
    assert!(glyphs[3].padding);
    assert_eq!(glyphs[0].bidi_level, 1);
    assert_eq!(glyphs[1].bidi_level, 1);
    assert_eq!(glyphs[2].bidi_level, 1);
    assert_eq!(glyphs[3].bidi_level, 1);
}

#[test]
fn builder_remaps_phys_cursor_to_visual_bidi_column() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 1, 10, Rect::new(0.0, 0.0, 80.0, 16.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('א', 0, 0);
    builder.push_char('ב', 0, 1);
    builder.end_row();
    builder.set_phys_cursor(PhysCursor {
        window_id: 1,
        charpos: 0,
        row: 0,
        col: 0,
        slot_id: DisplaySlotId {
            window_id: 1,
            row: 0,
            col: 0,
        },
        x: 0.0,
        y: 0.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: neomacs_display_protocol::types::Color::WHITE,
        cursor_fg: neomacs_display_protocol::types::Color::BLACK,
    });
    builder.end_window();

    let state = builder.finish(10, 1, 8.0, 16.0);
    let cursor = state.phys_cursor.as_ref().expect("phys cursor");
    assert_eq!(cursor.col, 1);
    assert_eq!(cursor.slot_id.col, 1);
    assert_eq!(cursor.x, 8.0);

    let row = &state.window_matrices[0].matrix.rows[0];
    assert_eq!(row.cursor_col, Some(1));
    assert_eq!(row.cursor_type, Some(CursorStyle::FilledBox));
}

#[test]
fn builder_reorders_status_line_rtl_row() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 1, 10, Rect::new(0.0, 0.0, 80.0, 16.0), true);
    builder.end_window();

    builder.begin_status_line_row(GlyphRowRole::ModeLine);
    builder.install_status_line_row_glyphs(vec![Glyph::char('א', 5, 0), Glyph::char('ב', 5, 1)]);

    let state = builder.finish(10, 1, 8.0, 16.0);
    let glyphs = &state.window_matrices[0].matrix.rows[1].glyphs[GlyphArea::Text as usize];
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].glyph_type, GlyphType::Char { ch: 'ב' });
    assert_eq!(glyphs[1].glyph_type, GlyphType::Char { ch: 'א' });
    assert_eq!(glyphs[0].bidi_level, 1);
    assert_eq!(glyphs[1].bidi_level, 1);
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

/// Regression test for the missing TTY vertical border between
/// horizontally-split windows. After `C-x 3` in `neomacs -nw -Q`
/// the rasterized output had no `|` character between the two
/// halves; the left window's last text column ran straight into
/// the right window's first text column.
///
/// GNU Emacs's TTY frame matrix builder
/// (`src/dispnew.c::build_frame_matrix_from_leaf_window` lines
/// 2568-2697) overwrites the LAST glyph of every enabled row in
/// any non-rightmost window with a `|` character in the
/// `vertical-border` face:
///
///   if (!WINDOW_RIGHTMOST_P (w))
///     SET_GLYPH_FROM_CHAR (right_border_glyph, '|');
///   ...
///   struct glyph *border = window_row->glyphs[LAST_AREA] - 1;
///   SET_CHAR_GLYPH_FROM_GLYPH (f, *border, right_border_glyph);
///
/// `GlyphMatrixBuilder::overwrite_last_window_right_border` is
/// the neomacs analog: after the layout engine closes a window
/// matrix that is not the rightmost in the frame, it patches
/// every enabled row of that window's matrix to end with a
/// border glyph at column `ncols - 1`. The `vertical-border`
/// face on TTY inherits from `mode-line-inactive`
/// (`lisp/faces.el::vertical-border`).
#[test]
fn overwrite_last_window_right_border_pads_and_replaces_text() {
    let mut builder = GlyphMatrixBuilder::new();

    // Window with ncols=10, two rows of text. The first row has
    // exactly 10 glyphs (full width), the second has 5 glyphs
    // (short text — needs padding before the border).
    builder.begin_window(1, 2, 10, Rect::new(0.0, 0.0, 80.0, 32.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    for ch in "0123456789".chars() {
        builder.push_char(ch, 0, 0);
    }
    builder.end_row();
    builder.begin_row(1, GlyphRowRole::Text);
    for ch in "abcde".chars() {
        builder.push_char(ch, 0, 0);
    }
    builder.end_row();
    builder.end_window();

    // Border glyph: '|' with face_id 99.
    builder.overwrite_last_window_right_border('|', 99);

    let state = builder.finish(20, 5, 8.0, 16.0);
    assert_eq!(state.window_matrices.len(), 1);
    let matrix = &state.window_matrices[0].matrix;

    // Row 0: full-width text, truncated to 9 columns with the
    // border occupying the 10th column in RightMargin.
    let row0_text = &matrix.rows[0].glyphs[GlyphArea::Text as usize];
    let row0_right = &matrix.rows[0].glyphs[GlyphArea::RightMargin as usize];
    assert_eq!(
        row0_text.len(),
        9,
        "row 0 text area must leave one column for the border"
    );
    let row0_chars: String = row0_text
        .iter()
        .map(|g| match &g.glyph_type {
            GlyphType::Char { ch } => *ch,
            _ => '?',
        })
        .collect();
    assert_eq!(
        row0_chars, "012345678",
        "row 0 text must keep the first 9 glyphs"
    );
    assert_eq!(
        row0_right.len(),
        1,
        "row 0 must place the border in the right-margin area"
    );
    assert_eq!(row0_right[0].glyph_type, GlyphType::Char { ch: '|' });
    assert_eq!(row0_right[0].face_id, 99);

    // Row 1: short text, padded with spaces to reach 9 glyphs
    // then a '|' as the 10th. Original 'a'..'e' must remain.
    let row1_text = &matrix.rows[1].glyphs[GlyphArea::Text as usize];
    let row1_right = &matrix.rows[1].glyphs[GlyphArea::RightMargin as usize];
    assert_eq!(
        row1_text.len(),
        9,
        "row 1 text area must be padded to ncols - 1 glyphs"
    );
    let row1_chars: String = row1_text
        .iter()
        .map(|g| match &g.glyph_type {
            GlyphType::Char { ch } => *ch,
            _ => '?',
        })
        .collect();
    assert_eq!(
        row1_chars, "abcde    ",
        "row 1 must keep original text and pad with spaces before the border"
    );
    assert_eq!(row1_right.len(), 1);
    assert_eq!(row1_right[0].glyph_type, GlyphType::Char { ch: '|' });
    assert_eq!(row1_right[0].face_id, 99);
    // Padding spaces must also use the border face id (so the
    // tty backend renders them with mode-line-inactive bg, not
    // default bg).
    assert_eq!(row1_text[5].face_id, 99);
    assert_eq!(row1_text[8].face_id, 99);
}

/// Blank visible rows below buffer text still need a vertical border
/// in horizontally split TTY windows. GNU's final frame matrix shows
/// `|` in that last column even when no buffer text was drawn there.
#[test]
fn overwrite_last_window_right_border_paints_blank_rows() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 3, 5, Rect::new(0.0, 0.0, 40.0, 48.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('A', 0, 0);
    builder.end_row();
    // Row 1 is never begun, stays disabled.
    builder.begin_row(2, GlyphRowRole::Text);
    builder.push_char('Z', 0, 0);
    builder.end_row();
    builder.end_window();

    builder.overwrite_last_window_right_border('|', 7);

    let state = builder.finish(10, 3, 8.0, 16.0);
    let matrix = &state.window_matrices[0].matrix;

    // Row 0 enabled: padded + border.
    let row0 = &matrix.rows[0].glyphs[GlyphArea::Text as usize];
    let row0_right = &matrix.rows[0].glyphs[GlyphArea::RightMargin as usize];
    assert_eq!(
        row0.len(),
        4,
        "enabled row must leave one column for the border"
    );
    assert_eq!(row0_right.len(), 1);
    // Row 1 blank: padded + border so the split remains visible.
    let row1_text = &matrix.rows[1].glyphs[GlyphArea::Text as usize];
    let row1_right = &matrix.rows[1].glyphs[GlyphArea::RightMargin as usize];
    assert!(!row1_text.is_empty(), "blank visible row must be padded");
    let row1_chars: String = row1_text
        .iter()
        .map(|g| match &g.glyph_type {
            GlyphType::Char { ch } => *ch,
            _ => '?',
        })
        .collect();
    assert_eq!(row1_chars, "    ");
    assert_eq!(row1_right.len(), 1);
    assert_eq!(row1_right[0].glyph_type, GlyphType::Char { ch: '|' });
    // Row 2 enabled: padded + border.
    let row2 = &matrix.rows[2].glyphs[GlyphArea::Text as usize];
    let row2_right = &matrix.rows[2].glyphs[GlyphArea::RightMargin as usize];
    assert_eq!(row2.len(), 4);
    assert_eq!(row2_right.len(), 1);
}

#[test]
fn overwrite_current_window_row_last_glyph_marks_truncated_row() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 2, 5, Rect::new(0.0, 0.0, 40.0, 32.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    for ch in "ABCDE".chars() {
        builder.push_char(ch, 0, 0);
    }
    builder.end_row();
    builder.begin_row(1, GlyphRowRole::Text);
    builder.push_char('X', 0, 0);
    builder.end_row();

    builder.overwrite_current_window_row_last_glyph(0, '$', 13);
    builder.overwrite_current_window_row_last_glyph(1, '$', 13);
    builder.end_window();

    let state = builder.finish(10, 2, 8.0, 16.0);
    let matrix = &state.window_matrices[0].matrix;

    let row0_chars: String = matrix.rows[0].glyphs[GlyphArea::Text as usize]
        .iter()
        .map(|g| match &g.glyph_type {
            GlyphType::Char { ch } => *ch,
            _ => '?',
        })
        .collect();
    assert_eq!(row0_chars, "ABCD$");

    let row1_chars: String = matrix.rows[1].glyphs[GlyphArea::Text as usize]
        .iter()
        .map(|g| match &g.glyph_type {
            GlyphType::Char { ch } => *ch,
            _ => '?',
        })
        .collect();
    assert_eq!(row1_chars, "X   $");
}

#[test]
fn overwrite_last_window_right_border_preserves_truncation_marker() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 1, 5, Rect::new(0.0, 0.0, 40.0, 16.0), true);
    builder.begin_row(0, GlyphRowRole::Text);
    for ch in "ABCD$".chars() {
        builder.push_char(ch, 0, 0);
    }
    builder.end_row();
    builder.end_window();

    builder.overwrite_last_window_right_border('|', 21);

    let state = builder.finish(5, 1, 8.0, 16.0);
    let matrix = &state.window_matrices[0].matrix;
    let row0_text = &matrix.rows[0].glyphs[GlyphArea::Text as usize];
    let row0_right = &matrix.rows[0].glyphs[GlyphArea::RightMargin as usize];

    let row0_chars: String = row0_text
        .iter()
        .map(|g| match &g.glyph_type {
            GlyphType::Char { ch } => *ch,
            _ => '?',
        })
        .collect();
    assert_eq!(row0_chars, "ABC$");
    assert_eq!(row0_right.len(), 1);
    assert_eq!(row0_right[0].glyph_type, GlyphType::Char { ch: '|' });
}
