use super::*;

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
    matrix.rows[0]
        .glyphs[GlyphArea::Text as usize]
        .push(Glyph::char('x', 0, 0));
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
