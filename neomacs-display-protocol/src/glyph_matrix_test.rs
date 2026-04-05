use super::*;
use crate::face::Face;

#[test]
fn empty_row_has_zero_hash() {
    let row = GlyphRow::new(GlyphRowRole::Text);
    assert_eq!(row.compute_hash(), 0);
}

#[test]
fn row_hash_changes_with_content() {
    let mut row = GlyphRow::new(GlyphRowRole::Text);
    let hash_empty = row.compute_hash();
    row.glyphs[GlyphArea::Text as usize].push(Glyph::char('a', 0, 0));
    let hash_a = row.compute_hash();
    assert_ne!(hash_empty, hash_a);
}

#[test]
fn row_hash_differs_for_different_chars() {
    let mut row_a = GlyphRow::new(GlyphRowRole::Text);
    row_a.glyphs[GlyphArea::Text as usize].push(Glyph::char('a', 0, 0));

    let mut row_b = GlyphRow::new(GlyphRowRole::Text);
    row_b.glyphs[GlyphArea::Text as usize].push(Glyph::char('b', 0, 0));

    assert_ne!(row_a.compute_hash(), row_b.compute_hash());
}

#[test]
fn row_hash_differs_for_different_faces() {
    let mut row_a = GlyphRow::new(GlyphRowRole::Text);
    row_a.glyphs[GlyphArea::Text as usize].push(Glyph::char('a', 0, 0));

    let mut row_b = GlyphRow::new(GlyphRowRole::Text);
    row_b.glyphs[GlyphArea::Text as usize].push(Glyph::char('a', 1, 0));

    assert_ne!(row_a.compute_hash(), row_b.compute_hash());
}

#[test]
fn identical_rows_have_same_hash() {
    let mut row_a = GlyphRow::new(GlyphRowRole::Text);
    row_a.glyphs[GlyphArea::Text as usize].push(Glyph::char('x', 5, 100));

    let mut row_b = GlyphRow::new(GlyphRowRole::Text);
    row_b.glyphs[GlyphArea::Text as usize].push(Glyph::char('x', 5, 100));

    assert_eq!(row_a.compute_hash(), row_b.compute_hash());
}

#[test]
fn row_equal_uses_hash_fast_path() {
    let mut row_a = GlyphRow::new(GlyphRowRole::Text);
    row_a.glyphs[GlyphArea::Text as usize].push(Glyph::char('a', 0, 0));
    row_a.hash = row_a.compute_hash();

    let mut row_b = GlyphRow::new(GlyphRowRole::Text);
    row_b.glyphs[GlyphArea::Text as usize].push(Glyph::char('b', 0, 0));
    row_b.hash = row_b.compute_hash();

    // Different hashes → rows are not equal (fast path, no cell comparison)
    assert!(!row_a.row_equal(&row_b));

    // Same content → equal
    let row_c = row_a.clone();
    assert!(row_a.row_equal(&row_c));
}

#[test]
fn new_row_has_empty_glyph_areas() {
    let row = GlyphRow::new(GlyphRowRole::ModeLine);
    assert!(row.glyphs[GlyphArea::LeftMargin as usize].is_empty());
    assert!(row.glyphs[GlyphArea::Text as usize].is_empty());
    assert!(row.glyphs[GlyphArea::RightMargin as usize].is_empty());
    assert_eq!(row.role, GlyphRowRole::ModeLine);
    assert!(row.enabled);
}

#[test]
fn matrix_new_has_correct_dimensions() {
    let matrix = GlyphMatrix::new(24, 80);
    assert_eq!(matrix.nrows, 24);
    assert_eq!(matrix.ncols, 80);
    assert_eq!(matrix.rows.len(), 24);
}

#[test]
fn matrix_rows_are_enabled_by_default() {
    let matrix = GlyphMatrix::new(3, 10);
    for row in &matrix.rows {
        assert!(row.enabled);
        assert_eq!(row.role, GlyphRowRole::Text);
    }
}

#[test]
fn matrix_clear_resets_all_rows() {
    let mut matrix = GlyphMatrix::new(2, 10);
    matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char('x', 0, 0));
    matrix.rows[0].hash = 12345;
    matrix.rows[0].cursor_col = Some(5);

    matrix.clear();

    assert!(matrix.rows[0].glyphs[GlyphArea::Text as usize].is_empty());
    assert_eq!(matrix.rows[0].hash, 0);
    assert_eq!(matrix.rows[0].cursor_col, None);
}

#[test]
fn matrix_resize_grows_and_shrinks() {
    let mut matrix = GlyphMatrix::new(10, 80);
    assert_eq!(matrix.rows.len(), 10);

    matrix.resize(20, 100);
    assert_eq!(matrix.nrows, 20);
    assert_eq!(matrix.ncols, 100);
    assert_eq!(matrix.rows.len(), 20);

    matrix.resize(5, 40);
    assert_eq!(matrix.nrows, 5);
    assert_eq!(matrix.ncols, 40);
    assert_eq!(matrix.rows.len(), 5);
}

#[test]
fn frame_display_state_new_has_correct_defaults() {
    let state = FrameDisplayState::new(80, 24, 8.0, 16.0);
    assert_eq!(state.frame_cols, 80);
    assert_eq!(state.frame_rows, 24);
    assert_eq!(state.char_width, 8.0);
    assert_eq!(state.char_height, 16.0);
    assert!(state.window_matrices.is_empty());
    assert!(state.faces.is_empty());
}

#[test]
fn frame_display_state_add_window_matrix() {
    let mut state = FrameDisplayState::new(80, 24, 8.0, 16.0);
    let matrix = GlyphMatrix::new(20, 80);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, 640.0, 320.0),
    });
    assert_eq!(state.window_matrices.len(), 1);
    assert_eq!(state.window_matrices[0].window_id, 1);
    assert_eq!(state.window_matrices[0].matrix.nrows, 20);
}

// ---------------------------------------------------------------------------
// FrameDisplayState::materialize() tests
// ---------------------------------------------------------------------------

/// Helper: build a FrameDisplayState with one window containing `text` on row 0.
fn state_with_text(text: &str) -> FrameDisplayState {
    let cols = text.len().max(1);
    let rows = 1;
    let char_w = 8.0f32;
    let char_h = 16.0f32;
    let mut state = FrameDisplayState::new(cols, rows, char_w, char_h);

    // Insert a default face (id 0)
    state.faces.insert(0, Face::new(0));

    let mut matrix = GlyphMatrix::new(1, cols);
    for (i, ch) in text.chars().enumerate() {
        matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 0, i));
    }

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, cols as f32 * char_w, char_h),
    });
    state
}

#[test]
fn materialize_produces_correct_glyph_count_from_grid() {
    let state = state_with_text("Hello");
    let buf = state.materialize();
    // 5 characters -> 5 FrameGlyph::Char entries
    assert_eq!(buf.glyphs.len(), 5);
    for g in &buf.glyphs {
        assert!(matches!(g, FrameGlyph::Char { .. }));
    }
}

#[test]
fn materialize_empty_grid_produces_no_glyphs() {
    let state = FrameDisplayState::new(80, 24, 8.0, 16.0);
    let buf = state.materialize();
    assert!(buf.glyphs.is_empty());
}

#[test]
fn materialize_includes_backgrounds() {
    let mut state = FrameDisplayState::new(80, 24, 8.0, 16.0);
    state.backgrounds.push(BackgroundItem {
        bounds: Rect::new(0.0, 0.0, 640.0, 384.0),
        color: Color::RED,
    });
    let buf = state.materialize();
    assert_eq!(buf.glyphs.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Background { bounds, color } => {
            assert_eq!(bounds.x, 0.0);
            assert_eq!(bounds.width, 640.0);
            assert_eq!(*color, Color::RED);
        }
        other => panic!("expected Background, got {:?}", other),
    }
}

#[test]
fn materialize_includes_borders() {
    let mut state = FrameDisplayState::new(80, 24, 8.0, 16.0);
    state.borders.push(BorderItem {
        window_id: 42,
        x: 100.0,
        y: 0.0,
        width: 1.0,
        height: 384.0,
        color: Color::WHITE,
    });
    let buf = state.materialize();
    assert_eq!(buf.glyphs.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Border {
            window_id,
            x,
            width,
            color,
            ..
        } => {
            assert_eq!(*window_id, 42);
            assert_eq!(*x, 100.0);
            assert_eq!(*width, 1.0);
            assert_eq!(*color, Color::WHITE);
        }
        other => panic!("expected Border, got {:?}", other),
    }
}

#[test]
fn materialize_includes_cursors() {
    let mut state = FrameDisplayState::new(80, 24, 8.0, 16.0);
    state.cursors.push(CursorItem {
        window_id: 7,
        x: 40.0,
        y: 0.0,
        width: 8.0,
        height: 16.0,
        style: CursorStyle::FilledBox,
        color: Color::GREEN,
    });
    let buf = state.materialize();
    assert_eq!(buf.glyphs.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Cursor {
            window_id,
            x,
            style,
            color,
            ..
        } => {
            assert_eq!(*window_id, 7);
            assert_eq!(*x, 40.0);
            assert_eq!(*style, CursorStyle::FilledBox);
            assert_eq!(*color, Color::GREEN);
        }
        other => panic!("expected Cursor, got {:?}", other),
    }
}

#[test]
fn materialize_includes_scroll_bars() {
    let mut state = FrameDisplayState::new(80, 24, 8.0, 16.0);
    state.scroll_bars.push(ScrollBarItem {
        horizontal: false,
        x: 632.0,
        y: 0.0,
        width: 8.0,
        height: 384.0,
        thumb_start: 10.0,
        thumb_size: 50.0,
        track_color: Color::BLACK,
        thumb_color: Color::WHITE,
    });
    let buf = state.materialize();
    assert_eq!(buf.glyphs.len(), 1);
    assert!(matches!(&buf.glyphs[0], FrameGlyph::ScrollBar { .. }));
}

#[test]
fn materialize_pixel_positions_from_grid() {
    let char_w = 10.0f32;
    let char_h = 20.0f32;
    let cols = 3;
    let rows = 2;
    let mut state = FrameDisplayState::new(cols, rows, char_w, char_h);
    state.faces.insert(0, Face::new(0));

    let mut matrix = GlyphMatrix::new(2, cols);
    // Row 0: "AB"
    matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char('A', 0, 0));
    matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char('B', 0, 1));
    // Row 1: "C"
    matrix.rows[1].glyphs[GlyphArea::Text as usize].push(Glyph::char('C', 0, 2));

    let win_x = 5.0f32;
    let win_y = 3.0f32;
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(win_x, win_y, cols as f32 * char_w, rows as f32 * char_h),
    });

    let buf = state.materialize();
    assert_eq!(buf.glyphs.len(), 3);

    // Glyph 'A' at (win_x + 0*char_w, win_y + 0*char_h)
    match &buf.glyphs[0] {
        FrameGlyph::Char {
            char: ch,
            x,
            y,
            width,
            height,
            ..
        } => {
            assert_eq!(*ch, 'A');
            assert_eq!(*x, win_x);
            assert_eq!(*y, win_y);
            assert_eq!(*width, char_w);
            assert_eq!(*height, char_h);
        }
        other => panic!("expected Char, got {:?}", other),
    }

    // Glyph 'B' at (win_x + 1*char_w, win_y + 0*char_h)
    match &buf.glyphs[1] {
        FrameGlyph::Char { char: ch, x, y, .. } => {
            assert_eq!(*ch, 'B');
            assert_eq!(*x, win_x + char_w);
            assert_eq!(*y, win_y);
        }
        other => panic!("expected Char, got {:?}", other),
    }

    // Glyph 'C' at (win_x + 0*char_w, win_y + 1*char_h)
    match &buf.glyphs[2] {
        FrameGlyph::Char { char: ch, x, y, .. } => {
            assert_eq!(*ch, 'C');
            assert_eq!(*x, win_x);
            assert_eq!(*y, win_y + char_h);
        }
        other => panic!("expected Char, got {:?}", other),
    }
}

#[test]
fn materialize_copies_metadata() {
    let mut state = FrameDisplayState::new(80, 24, 8.0, 16.0);
    state.frame_id = 123;
    state.parent_id = 456;
    state.parent_x = 10.0;
    state.parent_y = 20.0;
    state.z_order = 5;
    state.background = Color::BLUE;

    let mut face = Face::new(1);
    face.foreground = Color::RED;
    state.faces.insert(1, face);

    let buf = state.materialize();
    assert_eq!(buf.frame_id, 123);
    assert_eq!(buf.parent_id, 456);
    assert_eq!(buf.parent_x, 10.0);
    assert_eq!(buf.parent_y, 20.0);
    assert_eq!(buf.z_order, 5);
    assert_eq!(buf.background, Color::BLUE);
    assert!(buf.faces.contains_key(&1));
    assert_eq!(buf.faces[&1].foreground, Color::RED);
}

#[test]
fn materialize_disabled_rows_are_skipped() {
    let mut state = FrameDisplayState::new(3, 2, 8.0, 16.0);
    state.faces.insert(0, Face::new(0));

    let mut matrix = GlyphMatrix::new(2, 3);
    matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char('A', 0, 0));
    matrix.rows[1].glyphs[GlyphArea::Text as usize].push(Glyph::char('B', 0, 1));
    matrix.rows[1].enabled = false; // Disable row 1

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, 24.0, 32.0),
    });

    let buf = state.materialize();
    // Only row 0's glyph should be materialized
    assert_eq!(buf.glyphs.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Char { char: ch, .. } => assert_eq!(*ch, 'A'),
        other => panic!("expected Char, got {:?}", other),
    }
}

#[test]
fn materialize_padding_glyphs_are_skipped() {
    let mut state = FrameDisplayState::new(4, 1, 8.0, 16.0);
    state.faces.insert(0, Face::new(0));

    let mut matrix = GlyphMatrix::new(1, 4);
    // Wide char 'W' followed by padding
    let mut wide_glyph = Glyph::char('W', 0, 0);
    wide_glyph.wide = true;
    matrix.rows[0].glyphs[GlyphArea::Text as usize].push(wide_glyph);
    matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::padding_for(0, 0));
    matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char('x', 0, 1));

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, 32.0, 16.0),
    });

    let buf = state.materialize();
    // Should have 2 visible glyphs: wide 'W' and 'x'; padding is skipped
    assert_eq!(buf.glyphs.len(), 2);
    match &buf.glyphs[0] {
        FrameGlyph::Char {
            char: ch, width, ..
        } => {
            assert_eq!(*ch, 'W');
            assert_eq!(*width, 16.0); // 2 * char_w for wide
        }
        other => panic!("expected wide Char, got {:?}", other),
    }
    match &buf.glyphs[1] {
        FrameGlyph::Char { char: ch, x, .. } => {
            assert_eq!(*ch, 'x');
            // col = 2 (wide took 2 cols), so x = 2 * 8.0 = 16.0
            assert_eq!(*x, 16.0);
        }
        other => panic!("expected Char, got {:?}", other),
    }
}

#[test]
fn materialize_stretch_glyph() {
    let mut state = FrameDisplayState::new(10, 1, 8.0, 16.0);
    state.faces.insert(0, Face::new(0));

    let mut matrix = GlyphMatrix::new(1, 10);
    matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::stretch(4, 0));

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, 80.0, 16.0),
    });

    let buf = state.materialize();
    assert_eq!(buf.glyphs.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Stretch { width, height, .. } => {
            assert_eq!(*width, 4.0 * 8.0); // 4 cols * 8px
            assert_eq!(*height, 16.0);
        }
        other => panic!("expected Stretch, got {:?}", other),
    }
}

#[test]
fn materialize_new_fields_default_to_empty() {
    let state = FrameDisplayState::new(80, 24, 8.0, 16.0);
    assert!(state.backgrounds.is_empty());
    assert!(state.borders.is_empty());
    assert!(state.cursors.is_empty());
    assert!(state.images.is_empty());
    assert!(state.videos.is_empty());
    assert!(state.webkits.is_empty());
    assert!(state.scroll_bars.is_empty());
    assert!(state.cursor_inverse.is_none());
    assert!(state.stipple_patterns.is_empty());
    assert!(state.effect_hints.is_empty());
}

#[test]
fn materialize_mixed_grid_and_nongrid_items() {
    let mut state = state_with_text("Hi");

    // Add one background and one cursor
    state.backgrounds.push(BackgroundItem {
        bounds: Rect::new(0.0, 0.0, 16.0, 16.0),
        color: Color::BLACK,
    });
    state.cursors.push(CursorItem {
        window_id: 1,
        x: 0.0,
        y: 0.0,
        width: 8.0,
        height: 16.0,
        style: CursorStyle::FilledBox,
        color: Color::WHITE,
    });

    let buf = state.materialize();
    // 1 background + 2 chars + 1 cursor = 4
    assert_eq!(buf.glyphs.len(), 4);

    // Backgrounds come first
    assert!(matches!(&buf.glyphs[0], FrameGlyph::Background { .. }));
    // Then grid chars
    assert!(matches!(&buf.glyphs[1], FrameGlyph::Char { .. }));
    assert!(matches!(&buf.glyphs[2], FrameGlyph::Char { .. }));
    // Then cursors
    assert!(matches!(&buf.glyphs[3], FrameGlyph::Cursor { .. }));
}
