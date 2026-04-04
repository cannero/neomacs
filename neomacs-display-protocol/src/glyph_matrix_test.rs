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
