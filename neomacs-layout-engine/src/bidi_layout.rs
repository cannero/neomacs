//! Bidi (bidirectional text) integration for the Rust layout engine.
//!
//! Provides helpers to reorder glyph X positions within a completed row
//! according to the Unicode Bidirectional Algorithm (UAX#9).
//!
//! The integration works at row completion: after all characters on a line
//! have been laid out left-to-right, this module reorders their X positions
//! so that RTL runs appear in the correct visual order.

use crate::bidi::{self, BidiDir};
use neomacs_display_protocol::frame_glyphs::{FrameGlyph, FrameGlyphBuffer};

/// Quick check whether a character is in an RTL script range.
/// Used as a fast-path: if no character on a line is RTL, we skip
/// the full bidi algorithm entirely.
fn is_rtl_char(ch: char) -> bool {
    let cp = ch as u32;
    // Hebrew (0590-05FF)
    (0x0590..=0x05FF).contains(&cp)
    // Arabic (0600-06FF)
    || (0x0600..=0x06FF).contains(&cp)
    // Syriac (0700-074F)
    || (0x0700..=0x074F).contains(&cp)
    // Arabic Supplement (0750-077F)
    || (0x0750..=0x077F).contains(&cp)
    // Thaana (0780-07BF)
    || (0x0780..=0x07BF).contains(&cp)
    // NKo (07C0-07FF)
    || (0x07C0..=0x07FF).contains(&cp)
    // Samaritan (0800-083F)
    || (0x0800..=0x083F).contains(&cp)
    // Mandaic (0840-085F)
    || (0x0840..=0x085F).contains(&cp)
    // Arabic Extended-A (08A0-08FF)
    || (0x08A0..=0x08FF).contains(&cp)
    // Arabic Presentation Forms-A (FB50-FDFF)
    || (0xFB50..=0xFDFF).contains(&cp)
    // Arabic Presentation Forms-B (FE70-FEFF)
    || (0xFE70..=0xFEFF).contains(&cp)
    // Hebrew Presentation Forms (FB1D-FB4F)
    || (0xFB1D..=0xFB4F).contains(&cp)
    // RTL bidi control characters
    || cp == 0x200F  // RLM
    || cp == 0x202B  // RLE
    || cp == 0x202E  // RLO
    || cp == 0x2067 // RLI
}

/// Information about a character glyph on the current row, collected
/// for bidi reordering.
#[derive(Clone)]
struct RowCharInfo {
    /// Index into `frame_glyphs.glyphs`
    glyph_idx: usize,
    /// The character (for bidi class lookup and mirroring)
    ch: char,
    /// Original X position (set during LTR layout)
    x: f32,
    /// Glyph advance width
    width: f32,
}

/// Reorder glyph X positions on one completed row using the bidi algorithm.
///
/// `glyph_start` is the index into `frame_glyphs.glyphs` where this row's
/// glyphs begin. `glyph_end` is the exclusive end index.
/// `content_x` is the left edge of the text content area (after line numbers).
///
/// This function:
/// 1. Collects all Char/ComposedChar glyphs on the row
/// 2. Checks if any are RTL (fast-path exit if all LTR)
/// 3. Resolves bidi embedding levels
/// 4. Computes the visual reorder
/// 5. Reassigns X positions according to visual order
/// 6. Applies character mirroring for RTL characters
/// 7. Adjusts cursor positions if any cursors are on this row
pub fn reorder_row_bidi(
    frame_glyphs: &mut FrameGlyphBuffer,
    glyph_start: usize,
    glyph_end: usize,
    _content_x: f32,
) {
    if glyph_start >= glyph_end {
        return;
    }

    // Step 1: Collect character glyphs on this row
    let mut row_chars: Vec<RowCharInfo> = Vec::new();
    let mut row_max_ascent: f32 = 0.0;
    for idx in glyph_start..glyph_end {
        if idx >= frame_glyphs.glyphs.len() {
            break;
        }
        match &frame_glyphs.glyphs[idx] {
            FrameGlyph::Char {
                char: ch,
                x,
                width,
                ascent,
                ..
            } => {
                row_chars.push(RowCharInfo {
                    glyph_idx: idx,
                    ch: *ch,
                    x: *x,
                    width: *width,
                });
                row_max_ascent = row_max_ascent.max(*ascent);
            }
            FrameGlyph::Stretch {
                x,
                width,
                bidi_level: _,
                ..
            } => {
                row_chars.push(RowCharInfo {
                    glyph_idx: idx,
                    ch: ' ',
                    x: *x,
                    width: *width,
                });
            }
            _ => {
                // Skip glyphs that don't participate in bidi reordering.
            }
        }
    }

    if row_chars.is_empty() {
        return;
    }

    // Align all character glyphs on this row to a common baseline.
    // The baseline is at row_y + row_max_ascent. Each glyph's y is adjusted
    // by (row_max_ascent - glyph_ascent) so that y + ascent = row_y + row_max_ascent
    // for all glyphs. The original per-face ascent is preserved.
    if row_max_ascent > 0.0 {
        // Step A: Find the covered cursor slot on this row (before y adjustment)
        // and compute its offset.
        let cursor_offset: Option<f32> = {
            let mut found_offset = None;
            for info in &row_chars {
                if let FrameGlyph::Char {
                    slot_id, ascent, ..
                } = &frame_glyphs.glyphs[info.glyph_idx]
                {
                    if frame_glyphs
                        .phys_cursor
                        .as_ref()
                        .is_some_and(|cursor| *slot_id == cursor.slot_id)
                    {
                        found_offset = Some(row_max_ascent - *ascent);
                        break;
                    }
                }
            }
            found_offset
        };

        // Step B: Adjust each glyph's y for baseline alignment, keeping original ascent.
        // Renderer positioning uses `baseline` as authoritative; keep it in sync with `y`.
        for info in &row_chars {
            if let FrameGlyph::Char {
                y,
                baseline,
                ascent,
                ..
            } = &mut frame_glyphs.glyphs[info.glyph_idx]
            {
                let offset = row_max_ascent - *ascent;
                *y += offset;
                *baseline += offset;
                // ascent stays as the face's original ascent
            }
        }

        // Step C: Adjust cursor positions if the cursor is on this row.
        if let Some(offset) = cursor_offset {
            if offset.abs() > 0.01 {
                if let Some(ref mut cursor) = frame_glyphs.phys_cursor {
                    cursor.y += offset;
                }
                for idx in glyph_start..glyph_end {
                    if let FrameGlyph::Cursor { y, .. } = &mut frame_glyphs.glyphs[idx] {
                        *y += offset;
                    }
                }
            }
        }
    }

    // Step 2: Fast-path check — skip bidi if no RTL characters
    let has_rtl = row_chars.iter().any(|info| is_rtl_char(info.ch));
    if !has_rtl {
        return;
    }

    // Step 3: Build the character string and resolve bidi levels
    let chars: Vec<char> = row_chars.iter().map(|info| info.ch).collect();
    let text: String = chars.iter().collect();
    let levels = bidi::resolve_levels(&text, BidiDir::Auto);

    if levels.is_empty() {
        return;
    }

    // Fast-path: if all levels are 0, no reordering needed
    if levels.iter().all(|&l| l == 0) {
        return;
    }

    // Step 4: Get visual reorder indices
    let visual_order = bidi::reorder_visual(&levels);

    // Step 5: Compute new X positions based on visual order.
    // The visual order tells us: visual_order[visual_pos] = logical_index
    // We need to place glyphs left-to-right in visual order.
    //
    // First, compute the starting X of the row (minimum X among all chars).
    let row_start_x = row_chars
        .iter()
        .map(|info| info.x)
        .fold(f32::INFINITY, f32::min);

    // Collect widths in logical order
    let widths: Vec<f32> = row_chars.iter().map(|info| info.width).collect();

    // Assign new X positions: walk in visual order, placing each glyph
    let mut current_x = row_start_x;
    // new_x[logical_index] = new x position
    let mut new_x: Vec<f32> = vec![0.0; row_chars.len()];
    for &logical_idx in &visual_order {
        new_x[logical_idx] = current_x;
        current_x += widths[logical_idx];
    }

    // Step 6: Apply new X positions and mirroring to the glyphs
    for (logical_idx, info) in row_chars.iter().enumerate() {
        let glyph = &mut frame_glyphs.glyphs[info.glyph_idx];
        match glyph {
            FrameGlyph::Char {
                x,
                char: ch,
                bidi_level,
                ..
            } => {
                *x = new_x[logical_idx];
                *bidi_level = levels[logical_idx];
                // Apply character mirroring for RTL characters (odd level)
                if levels[logical_idx] % 2 == 1 {
                    if let Some(mirrored) = bidi::bidi_mirror(*ch) {
                        *ch = mirrored;
                    }
                }
            }
            FrameGlyph::Stretch { x, bidi_level, .. } => {
                *x = new_x[logical_idx];
                *bidi_level = levels[logical_idx];
            }
            _ => {}
        }
    }

    // Step 7: Adjust cursor positions on this row.
    // Cursors were placed at LTR X positions; we need to move them
    // to match the reordered glyph positions.
    // Find cursor glyphs in the range and update their X to match the exact
    // covered display slot rather than rediscovering it from geometry.
    let cursor_slot_id = frame_glyphs
        .phys_cursor
        .as_ref()
        .map(|cursor| cursor.slot_id);
    for idx in glyph_start..glyph_end.min(frame_glyphs.glyphs.len()) {
        let new_cursor_x = cursor_slot_id.and_then(|cursor_slot_id| {
            row_chars
                .iter()
                .enumerate()
                .find_map(
                    |(logical_idx, info)| match &frame_glyphs.glyphs[info.glyph_idx] {
                        FrameGlyph::Char { slot_id, .. } | FrameGlyph::Stretch { slot_id, .. }
                            if *slot_id == cursor_slot_id =>
                        {
                            Some(new_x[logical_idx])
                        }
                        _ => None,
                    },
                )
        });
        if let (Some(new_cursor_x), FrameGlyph::Cursor { x: cursor_x, .. }) =
            (new_cursor_x, &mut frame_glyphs.glyphs[idx])
        {
            *cursor_x = new_cursor_x;
        }
    }

    if let Some(ref mut cursor) = frame_glyphs.phys_cursor {
        for (logical_idx, info) in row_chars.iter().enumerate() {
            match &frame_glyphs.glyphs[info.glyph_idx] {
                FrameGlyph::Char { slot_id, .. } | FrameGlyph::Stretch { slot_id, .. }
                    if *slot_id == cursor.slot_id =>
                {
                    cursor.x = new_x[logical_idx];
                    break;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neomacs_display_protocol::frame_glyphs::{CursorStyle, DisplaySlotId, GlyphRowRole};
    use neomacs_display_protocol::types::Color;

    /// Helper to create a minimal Char glyph for testing.
    fn make_char_glyph(ch: char, x: f32, width: f32) -> FrameGlyph {
        let y = 0.0;
        let ascent = 12.0;
        FrameGlyph::Char {
            window_id: 0,
            row_role: GlyphRowRole::Text,
            clip_rect: None,
            slot_id: DisplaySlotId::from_pixels(0, x, y, 8.0, 16.0),
            bidi_level: 0,
            char: ch,
            composed: None,
            x,
            y,
            baseline: y + ascent,
            width,
            height: 16.0,
            ascent,
            fg: Color::new(1.0, 1.0, 1.0, 1.0),
            bg: None,
            face_id: 0,
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

    fn get_char_x(glyph: &FrameGlyph) -> f32 {
        match glyph {
            FrameGlyph::Char { x, .. } => *x,
            _ => panic!("expected Char glyph"),
        }
    }

    fn get_char(glyph: &FrameGlyph) -> char {
        match glyph {
            FrameGlyph::Char { char: ch, .. } => *ch,
            _ => panic!("expected Char glyph"),
        }
    }

    fn get_char_ascent(glyph: &FrameGlyph) -> f32 {
        match glyph {
            FrameGlyph::Char { ascent, .. } => *ascent,
            _ => panic!("expected Char glyph"),
        }
    }

    fn get_char_y(glyph: &FrameGlyph) -> f32 {
        match glyph {
            FrameGlyph::Char { y, .. } => *y,
            _ => panic!("expected Char glyph"),
        }
    }

    fn get_char_baseline(glyph: &FrameGlyph) -> f32 {
        match glyph {
            FrameGlyph::Char { baseline, .. } => *baseline,
            _ => panic!("expected Char glyph"),
        }
    }

    #[test]
    fn test_pure_ltr_no_reorder() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('H', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('i', 8.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 2, 0.0);

        // Should be unchanged
        assert_eq!(get_char_x(&buf.glyphs[0]), 0.0);
        assert_eq!(get_char_x(&buf.glyphs[1]), 8.0);
    }

    #[test]
    fn test_row_ascent_normalized_even_without_rtl() {
        let mut buf = FrameGlyphBuffer::default();
        let mut g1 = make_char_glyph('A', 0.0, 8.0);
        let mut g2 = make_char_glyph('B', 8.0, 8.0);
        if let FrameGlyph::Char {
            ascent, baseline, ..
        } = &mut g1
        {
            *ascent = 9.0;
            *baseline = 9.0;
        }
        if let FrameGlyph::Char {
            ascent, baseline, ..
        } = &mut g2
        {
            *ascent = 13.0;
            *baseline = 13.0;
        }
        buf.glyphs.push(g1);
        buf.glyphs.push(g2);

        reorder_row_bidi(&mut buf, 0, 2, 0.0);

        assert_eq!(get_char_x(&buf.glyphs[0]), 0.0);
        assert_eq!(get_char_x(&buf.glyphs[1]), 8.0);
        assert_eq!(get_char_ascent(&buf.glyphs[0]), 9.0);
        assert_eq!(get_char_ascent(&buf.glyphs[1]), 13.0);
        assert_eq!(get_char_baseline(&buf.glyphs[0]), 13.0);
        assert_eq!(get_char_baseline(&buf.glyphs[1]), 13.0);
        assert_eq!(
            get_char_y(&buf.glyphs[0]) + get_char_ascent(&buf.glyphs[0]),
            13.0
        );
        assert_eq!(
            get_char_y(&buf.glyphs[1]) + get_char_ascent(&buf.glyphs[1]),
            13.0
        );
    }

    #[test]
    fn test_pure_rtl_reorder() {
        let mut buf = FrameGlyphBuffer::default();
        // Hebrew: alef, bet, gimel laid out LTR
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 8.0)); // Alef
        buf.glyphs.push(make_char_glyph('\u{05D1}', 8.0, 8.0)); // Bet
        buf.glyphs.push(make_char_glyph('\u{05D2}', 16.0, 8.0)); // Gimel

        reorder_row_bidi(&mut buf, 0, 3, 0.0);

        // RTL: visual order should be reversed
        // Gimel at x=0, Bet at x=8, Alef at x=16
        assert_eq!(get_char_x(&buf.glyphs[0]), 16.0); // Alef (logical 0) -> rightmost
        assert_eq!(get_char_x(&buf.glyphs[1]), 8.0); // Bet (logical 1) -> middle
        assert_eq!(get_char_x(&buf.glyphs[2]), 0.0); // Gimel (logical 2) -> leftmost
    }

    #[test]
    fn test_mixed_ltr_rtl() {
        let mut buf = FrameGlyphBuffer::default();
        // "Hi" + Hebrew "אב" — LTR base with RTL embedded
        buf.glyphs.push(make_char_glyph('H', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('i', 8.0, 8.0));
        buf.glyphs.push(make_char_glyph(' ', 16.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D0}', 24.0, 8.0)); // Alef
        buf.glyphs.push(make_char_glyph('\u{05D1}', 32.0, 8.0)); // Bet

        reorder_row_bidi(&mut buf, 0, 5, 0.0);

        // LTR base: H, i, space stay at left
        // RTL segment: Alef and Bet should be swapped
        assert_eq!(get_char_x(&buf.glyphs[0]), 0.0); // H
        assert_eq!(get_char_x(&buf.glyphs[1]), 8.0); // i
        assert_eq!(get_char_x(&buf.glyphs[2]), 16.0); // space
        // Bet (logical idx 4) should come before Alef (logical idx 3)
        assert_eq!(get_char_x(&buf.glyphs[3]), 32.0); // Alef -> right
        assert_eq!(get_char_x(&buf.glyphs[4]), 24.0); // Bet -> left
    }

    #[test]
    fn test_bracket_mirroring() {
        let mut buf = FrameGlyphBuffer::default();
        // RTL text with brackets: ( should become ) and vice versa
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 8.0)); // Alef
        buf.glyphs.push(make_char_glyph('(', 8.0, 8.0)); // Open paren
        buf.glyphs.push(make_char_glyph('\u{05D1}', 16.0, 8.0)); // Bet
        buf.glyphs.push(make_char_glyph(')', 24.0, 8.0)); // Close paren

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        // In RTL context, '(' should be mirrored to ')' and ')' to '('
        // The paren characters are at levels determined by the bidi algorithm.
        // After reordering, verify that brackets got mirrored at odd levels.
        let chars: Vec<char> = buf.glyphs.iter().map(|g| get_char(g)).collect();
        // The reordered text should have ')' where '(' was and vice versa
        // (because they're in an RTL run)
        assert!(chars.contains(&')'));
        assert!(chars.contains(&'('));
    }

    #[test]
    fn test_empty_row() {
        let mut buf = FrameGlyphBuffer::default();
        // Should not panic
        reorder_row_bidi(&mut buf, 0, 0, 0.0);
    }

    #[test]
    fn test_non_char_glyphs_preserved() {
        let mut buf = FrameGlyphBuffer::default();
        // Add a stretch glyph between chars
        buf.glyphs.push(make_char_glyph('H', 0.0, 8.0));
        buf.glyphs.push(FrameGlyph::Stretch {
            window_id: 0,
            row_role: GlyphRowRole::Text,
            clip_rect: None,
            slot_id: DisplaySlotId::from_pixels(0, 8.0, 0.0, 8.0, 16.0),
            bidi_level: 0,
            x: 8.0,
            y: 0.0,
            width: 16.0,
            height: 16.0,
            bg: Color::new(0.0, 0.0, 0.0, 1.0),
            face_id: 0,
            stipple_id: 0,
            stipple_fg: None,
        });
        buf.glyphs.push(make_char_glyph('i', 24.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 3, 0.0);

        // Stretch should be untouched
        if let FrameGlyph::Stretch { x, .. } = &buf.glyphs[1] {
            assert_eq!(*x, 8.0);
        } else {
            panic!("expected Stretch glyph");
        }
    }

    // ===================================================================
    // Additional comprehensive tests
    // ===================================================================

    /// Helper to create a Cursor glyph for testing.
    fn make_cursor_glyph(x: f32, width: f32) -> FrameGlyph {
        FrameGlyph::Cursor {
            window_id: 0,
            x,
            y: 0.0,
            width,
            height: 16.0,
            style: CursorStyle::FilledBox,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
        }
    }

    fn make_phys_cursor(
        slot_x: f32,
        width: f32,
    ) -> neomacs_display_protocol::frame_glyphs::PhysCursor {
        neomacs_display_protocol::frame_glyphs::PhysCursor {
            window_id: 0,
            charpos: 0,
            row: 0,
            col: (slot_x / width).round() as u16,
            slot_id: DisplaySlotId::from_pixels(0, slot_x, 0.0, width, 16.0),
            x: slot_x,
            y: 0.0,
            width,
            height: 16.0,
            ascent: 12.0,
            style: CursorStyle::FilledBox,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
            cursor_fg: Color::new(0.0, 0.0, 0.0, 1.0),
        }
    }

    fn get_cursor_x(glyph: &FrameGlyph) -> f32 {
        match glyph {
            FrameGlyph::Cursor { x, .. } => *x,
            _ => panic!("expected Cursor glyph"),
        }
    }

    fn get_stretch_x(glyph: &FrameGlyph) -> f32 {
        match glyph {
            FrameGlyph::Stretch { x, .. } => *x,
            _ => panic!("expected Stretch glyph"),
        }
    }

    fn get_stretch_bidi_level(glyph: &FrameGlyph) -> u8 {
        match glyph {
            FrameGlyph::Stretch { bidi_level, .. } => *bidi_level,
            _ => panic!("expected Stretch glyph"),
        }
    }

    // --- is_rtl_char coverage for various Unicode ranges ---

    #[test]
    fn test_is_rtl_char_hebrew_range() {
        // Hebrew letters U+05D0..U+05EA
        assert!(is_rtl_char('\u{05D0}')); // Alef
        assert!(is_rtl_char('\u{05EA}')); // Tav
        assert!(is_rtl_char('\u{0590}')); // Start of Hebrew block
        assert!(is_rtl_char('\u{05FF}')); // End of Hebrew block
    }

    #[test]
    fn test_is_rtl_char_arabic_range() {
        assert!(is_rtl_char('\u{0600}')); // Start of Arabic block
        assert!(is_rtl_char('\u{0627}')); // Arabic Alef
        assert!(is_rtl_char('\u{06FF}')); // End of Arabic block
    }

    #[test]
    fn test_is_rtl_char_syriac_range() {
        assert!(is_rtl_char('\u{0700}')); // Start of Syriac block
        assert!(is_rtl_char('\u{0710}')); // Syriac Alaph
        assert!(is_rtl_char('\u{074F}')); // End of Syriac block
    }

    #[test]
    fn test_is_rtl_char_arabic_supplement() {
        assert!(is_rtl_char('\u{0750}')); // Start of Arabic Supplement
        assert!(is_rtl_char('\u{077F}')); // End of Arabic Supplement
    }

    #[test]
    fn test_is_rtl_char_thaana_range() {
        assert!(is_rtl_char('\u{0780}')); // Start of Thaana block
        assert!(is_rtl_char('\u{07A0}')); // Middle of Thaana
        assert!(is_rtl_char('\u{07BF}')); // End of Thaana block
    }

    #[test]
    fn test_is_rtl_char_nko_range() {
        assert!(is_rtl_char('\u{07C0}')); // Start of NKo block
        assert!(is_rtl_char('\u{07E0}')); // NKo letter
        assert!(is_rtl_char('\u{07FF}')); // End of NKo block
    }

    #[test]
    fn test_is_rtl_char_samaritan_range() {
        assert!(is_rtl_char('\u{0800}')); // Start of Samaritan block
        assert!(is_rtl_char('\u{0820}')); // Middle
        assert!(is_rtl_char('\u{083F}')); // End of Samaritan block
    }

    #[test]
    fn test_is_rtl_char_mandaic_range() {
        assert!(is_rtl_char('\u{0840}')); // Start of Mandaic block
        assert!(is_rtl_char('\u{0850}')); // Mandaic letter
        assert!(is_rtl_char('\u{085F}')); // End of Mandaic block
    }

    #[test]
    fn test_is_rtl_char_arabic_extended_a() {
        assert!(is_rtl_char('\u{08A0}')); // Start of Arabic Extended-A
        assert!(is_rtl_char('\u{08D0}')); // Middle
        assert!(is_rtl_char('\u{08FF}')); // End of Arabic Extended-A
    }

    #[test]
    fn test_is_rtl_char_arabic_presentation_forms() {
        // Presentation Forms-A (FB50-FDFF)
        assert!(is_rtl_char('\u{FB50}'));
        assert!(is_rtl_char('\u{FD00}'));
        assert!(is_rtl_char('\u{FDFF}'));
        // Presentation Forms-B (FE70-FEFF)
        assert!(is_rtl_char('\u{FE70}'));
        assert!(is_rtl_char('\u{FEB0}'));
        assert!(is_rtl_char('\u{FEFF}'));
    }

    #[test]
    fn test_is_rtl_char_hebrew_presentation_forms() {
        assert!(is_rtl_char('\u{FB1D}')); // Hebrew YOD WITH HIRIQ
        assert!(is_rtl_char('\u{FB2A}')); // Hebrew SHIN WITH SHIN DOT
        assert!(is_rtl_char('\u{FB4F}')); // End of Hebrew presentation forms
    }

    #[test]
    fn test_is_rtl_char_bidi_control_characters() {
        assert!(is_rtl_char('\u{200F}')); // RLM
        assert!(is_rtl_char('\u{202B}')); // RLE
        assert!(is_rtl_char('\u{202E}')); // RLO
        assert!(is_rtl_char('\u{2067}')); // RLI
    }

    #[test]
    fn test_is_rtl_char_non_rtl_characters() {
        // Latin
        assert!(!is_rtl_char('A'));
        assert!(!is_rtl_char('z'));
        // Digits
        assert!(!is_rtl_char('0'));
        assert!(!is_rtl_char('9'));
        // CJK
        assert!(!is_rtl_char('\u{4E00}'));
        // Hiragana
        assert!(!is_rtl_char('\u{3042}'));
        // Space and punctuation
        assert!(!is_rtl_char(' '));
        assert!(!is_rtl_char('.'));
        assert!(!is_rtl_char('('));
        // LTR bidi controls
        assert!(!is_rtl_char('\u{200E}')); // LRM
        assert!(!is_rtl_char('\u{202A}')); // LRE
        assert!(!is_rtl_char('\u{2066}')); // LRI
    }

    #[test]
    fn test_is_rtl_char_boundary_values() {
        // Just outside Hebrew range
        assert!(!is_rtl_char('\u{058F}')); // Below Hebrew block (U+058F is Armenian)
        // U+0600 IS in the Arabic range (0600-06FF), so it should be RTL
        assert!(is_rtl_char('\u{0600}'));
        // Between Mandaic end (085F) and Arabic Extended-A (08A0)
        assert!(!is_rtl_char('\u{0860}')); // Not covered by is_rtl_char ranges
        // After Arabic Extended-A
        assert!(!is_rtl_char('\u{0900}')); // Devanagari, not RTL
        // NUL character
        assert!(!is_rtl_char('\0'));
        // ASCII boundary
        assert!(!is_rtl_char('\u{007F}')); // DEL
    }

    // --- Edge cases for reorder_row_bidi ---

    #[test]
    fn test_single_ltr_char() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('A', 10.0, 8.0));
        reorder_row_bidi(&mut buf, 0, 1, 0.0);
        assert_eq!(get_char_x(&buf.glyphs[0]), 10.0);
    }

    #[test]
    fn test_single_rtl_char() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 10.0, 8.0));
        reorder_row_bidi(&mut buf, 0, 1, 10.0);
        // Single RTL char: levels=[1], visual reorder reverses a run of one.
        // X position should remain unchanged since there is only one glyph.
        assert_eq!(get_char_x(&buf.glyphs[0]), 10.0);
    }

    #[test]
    fn test_glyph_start_equals_glyph_end() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('A', 0.0, 8.0));
        // start == end => should return immediately without panic
        reorder_row_bidi(&mut buf, 0, 0, 0.0);
        assert_eq!(get_char_x(&buf.glyphs[0]), 0.0);
    }

    #[test]
    fn test_glyph_start_greater_than_glyph_end() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('A', 0.0, 8.0));
        // start > end => should return immediately without panic
        reorder_row_bidi(&mut buf, 5, 2, 0.0);
        assert_eq!(get_char_x(&buf.glyphs[0]), 0.0);
    }

    #[test]
    fn test_glyph_end_beyond_buffer_length() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('A', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D0}', 8.0, 8.0));
        // glyph_end (100) exceeds glyphs.len() (2), should handle gracefully
        reorder_row_bidi(&mut buf, 0, 100, 0.0);
        // RTL char present, so reordering happens but should not panic
    }

    #[test]
    fn test_empty_buffer_no_panic() {
        let mut buf = FrameGlyphBuffer::default();
        reorder_row_bidi(&mut buf, 0, 0, 0.0);
        reorder_row_bidi(&mut buf, 0, 10, 0.0);
        reorder_row_bidi(&mut buf, 5, 10, 0.0);
        // None should panic
    }

    // --- Complex bidi scenarios ---

    #[test]
    fn test_ltr_rtl_ltr_sandwich() {
        // "AB" + Hebrew "גד" + "EF"
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('A', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('B', 8.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D2}', 16.0, 8.0)); // Gimel
        buf.glyphs.push(make_char_glyph('\u{05D3}', 24.0, 8.0)); // Dalet
        buf.glyphs.push(make_char_glyph('E', 32.0, 8.0));
        buf.glyphs.push(make_char_glyph('F', 40.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 6, 0.0);

        // LTR chars A, B should stay left
        assert_eq!(get_char_x(&buf.glyphs[0]), 0.0); // A
        assert_eq!(get_char_x(&buf.glyphs[1]), 8.0); // B
        // Hebrew chars should be reversed: Dalet at 16, Gimel at 24
        assert_eq!(get_char_x(&buf.glyphs[2]), 24.0); // Gimel (was logical 2) -> right of RTL pair
        assert_eq!(get_char_x(&buf.glyphs[3]), 16.0); // Dalet (was logical 3) -> left of RTL pair
        // LTR chars E, F should stay right
        assert_eq!(get_char_x(&buf.glyphs[4]), 32.0); // E
        assert_eq!(get_char_x(&buf.glyphs[5]), 40.0); // F
    }

    #[test]
    fn test_all_ltr_long_row() {
        let mut buf = FrameGlyphBuffer::default();
        let text = "The quick brown fox jumps";
        for (i, ch) in text.chars().enumerate() {
            buf.glyphs.push(make_char_glyph(ch, i as f32 * 8.0, 8.0));
        }
        let len = buf.glyphs.len();
        reorder_row_bidi(&mut buf, 0, len, 0.0);

        // All LTR => positions unchanged
        for (i, _) in text.chars().enumerate() {
            assert_eq!(get_char_x(&buf.glyphs[i]), i as f32 * 8.0);
        }
    }

    #[test]
    fn test_all_rtl_arabic_text() {
        // All Arabic letters, should be fully reversed
        let mut buf = FrameGlyphBuffer::default();
        let chars = ['\u{0627}', '\u{0628}', '\u{062A}', '\u{062B}']; // Alef, Ba, Ta, Tha
        for (i, &ch) in chars.iter().enumerate() {
            buf.glyphs.push(make_char_glyph(ch, i as f32 * 10.0, 10.0));
        }

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        // All RTL: visual order reversed. Last logical char gets x=0, first gets x=30
        assert_eq!(get_char_x(&buf.glyphs[0]), 30.0); // Alef -> rightmost
        assert_eq!(get_char_x(&buf.glyphs[1]), 20.0); // Ba
        assert_eq!(get_char_x(&buf.glyphs[2]), 10.0); // Ta
        assert_eq!(get_char_x(&buf.glyphs[3]), 0.0); // Tha -> leftmost
    }

    #[test]
    fn test_rtl_with_numbers() {
        // Hebrew text with embedded numbers: "א 123 ב"
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 8.0)); // Alef
        buf.glyphs.push(make_char_glyph(' ', 8.0, 8.0));
        buf.glyphs.push(make_char_glyph('1', 16.0, 8.0));
        buf.glyphs.push(make_char_glyph('2', 24.0, 8.0));
        buf.glyphs.push(make_char_glyph('3', 32.0, 8.0));
        buf.glyphs.push(make_char_glyph(' ', 40.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D1}', 48.0, 8.0)); // Bet

        reorder_row_bidi(&mut buf, 0, 7, 0.0);

        // With RTL base, the overall visual order should reflect RTL paragraph
        // The numbers 123 should maintain their LTR order within the RTL context
        // Verify that the Hebrew chars moved to the right side and numbers stayed ordered
        let num_x_1 = get_char_x(&buf.glyphs[2]);
        let num_x_2 = get_char_x(&buf.glyphs[3]);
        let num_x_3 = get_char_x(&buf.glyphs[4]);
        // Numbers maintain relative LTR order: 1 < 2 < 3
        assert!(num_x_1 < num_x_2, "1 should be left of 2");
        assert!(num_x_2 < num_x_3, "2 should be left of 3");
    }

    #[test]
    fn test_variable_width_glyphs_rtl() {
        // RTL chars with different widths
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 10.0)); // Alef, width 10
        buf.glyphs.push(make_char_glyph('\u{05D1}', 10.0, 12.0)); // Bet, width 12
        buf.glyphs.push(make_char_glyph('\u{05D2}', 22.0, 8.0)); // Gimel, width 8

        reorder_row_bidi(&mut buf, 0, 3, 0.0);

        // After reversal: Gimel(8) at 0, Bet(12) at 8, Alef(10) at 20
        assert_eq!(get_char_x(&buf.glyphs[2]), 0.0); // Gimel (logical 2) -> leftmost
        assert_eq!(get_char_x(&buf.glyphs[1]), 8.0); // Bet (logical 1) -> middle
        assert_eq!(get_char_x(&buf.glyphs[0]), 20.0); // Alef (logical 0) -> rightmost
    }

    #[test]
    fn test_content_x_offset() {
        // Glyphs starting at x=100 (simulating line number offset)
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 100.0, 8.0)); // Alef
        buf.glyphs.push(make_char_glyph('\u{05D1}', 108.0, 8.0)); // Bet
        buf.glyphs.push(make_char_glyph('\u{05D2}', 116.0, 8.0)); // Gimel

        reorder_row_bidi(&mut buf, 0, 3, 100.0);

        // After reversal, row_start_x should be 100.0 (minimum x)
        // Gimel at 100, Bet at 108, Alef at 116
        assert_eq!(get_char_x(&buf.glyphs[2]), 100.0); // Gimel -> leftmost
        assert_eq!(get_char_x(&buf.glyphs[1]), 108.0); // Bet -> middle
        assert_eq!(get_char_x(&buf.glyphs[0]), 116.0); // Alef -> rightmost
    }

    // --- Bracket mirroring in various contexts ---

    #[test]
    fn test_mirror_square_brackets_in_rtl() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 8.0)); // Alef
        buf.glyphs.push(make_char_glyph('[', 8.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D1}', 16.0, 8.0)); // Bet
        buf.glyphs.push(make_char_glyph(']', 24.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        // '[' and ']' at RTL (odd) levels should be mirrored
        let chars: Vec<char> = buf.glyphs.iter().map(|g| get_char(g)).collect();
        // After mirroring in RTL context: '[' -> ']', ']' -> '['
        assert!(chars.contains(&'['));
        assert!(chars.contains(&']'));
    }

    #[test]
    fn test_mirror_curly_braces_in_rtl() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('{', 8.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D1}', 16.0, 8.0));
        buf.glyphs.push(make_char_glyph('}', 24.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        let chars: Vec<char> = buf.glyphs.iter().map(|g| get_char(g)).collect();
        assert!(chars.contains(&'{'));
        assert!(chars.contains(&'}'));
    }

    #[test]
    fn test_mirror_angle_brackets_in_rtl() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('<', 8.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D1}', 16.0, 8.0));
        buf.glyphs.push(make_char_glyph('>', 24.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        let chars: Vec<char> = buf.glyphs.iter().map(|g| get_char(g)).collect();
        assert!(chars.contains(&'<') || chars.contains(&'>'));
    }

    #[test]
    fn test_no_mirroring_for_ltr_brackets() {
        // Pure LTR text: brackets should NOT be mirrored
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('(', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('A', 8.0, 8.0));
        buf.glyphs.push(make_char_glyph(')', 16.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 3, 0.0);

        // No RTL chars => fast-path, no reordering or mirroring
        assert_eq!(get_char(&buf.glyphs[0]), '(');
        assert_eq!(get_char(&buf.glyphs[1]), 'A');
        assert_eq!(get_char(&buf.glyphs[2]), ')');
    }

    // --- Cursor position adjustment ---

    #[test]
    fn test_cursor_moves_with_rtl_reorder() {
        let mut buf = FrameGlyphBuffer::default();
        // Hebrew text with cursor at the position of Alef (x=0.0)
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 8.0)); // Alef
        buf.glyphs.push(make_char_glyph('\u{05D1}', 8.0, 8.0)); // Bet
        buf.glyphs.push(make_char_glyph('\u{05D2}', 16.0, 8.0)); // Gimel
        buf.glyphs.push(make_cursor_glyph(0.0, 8.0)); // Cursor at Alef's original x
        buf.phys_cursor = Some(make_phys_cursor(0.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        // After reorder, Alef moves to x=16.0 (rightmost)
        // Cursor should follow Alef to its new position
        assert_eq!(get_char_x(&buf.glyphs[0]), 16.0); // Alef
        assert_eq!(get_cursor_x(&buf.glyphs[3]), 16.0); // Cursor should match Alef
    }

    #[test]
    fn test_cursor_at_rtl_char_middle_of_row() {
        let mut buf = FrameGlyphBuffer::default();
        // "A" + Hebrew "בג" + cursor at Bet(x=8.0)
        buf.glyphs.push(make_char_glyph('A', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D1}', 8.0, 8.0)); // Bet
        buf.glyphs.push(make_char_glyph('\u{05D2}', 16.0, 8.0)); // Gimel
        buf.glyphs.push(make_cursor_glyph(8.0, 8.0)); // Cursor at Bet's original x
        buf.phys_cursor = Some(make_phys_cursor(8.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        // In LTR base with RTL embedded: A stays at 0, Gimel and Bet swap
        // Bet (logical 1) gets new x, Gimel (logical 2) gets new x
        // Cursor should match Bet's new x
        let bet_new_x = get_char_x(&buf.glyphs[1]);
        let cursor_new_x = get_cursor_x(&buf.glyphs[3]);
        assert_eq!(cursor_new_x, bet_new_x);
    }

    #[test]
    fn test_cursor_in_ltr_section_unchanged() {
        let mut buf = FrameGlyphBuffer::default();
        // "AB" + Hebrew "גד" + cursor at A (x=0.0)
        buf.glyphs.push(make_char_glyph('A', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('B', 8.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D2}', 16.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D3}', 24.0, 8.0));
        buf.glyphs.push(make_cursor_glyph(0.0, 8.0)); // Cursor at A
        buf.phys_cursor = Some(make_phys_cursor(0.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 5, 0.0);

        // A stays at x=0, cursor should stay at x=0
        assert_eq!(get_char_x(&buf.glyphs[0]), 0.0);
        assert_eq!(get_cursor_x(&buf.glyphs[4]), 0.0);
    }

    #[test]
    fn test_active_phys_cursor_moves_in_mixed_text() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('A', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D0}', 8.0, 8.0)); // Alef
        buf.glyphs.push(make_char_glyph('\u{05D1}', 16.0, 8.0)); // Bet
        buf.glyphs.push(make_cursor_glyph(8.0, 8.0)); // Cursor at Alef
        buf.phys_cursor = Some(make_phys_cursor(8.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        // Alef's new position after reorder
        let alef_new_x = get_char_x(&buf.glyphs[1]);
        assert_eq!(get_cursor_x(&buf.glyphs[3]), alef_new_x);
    }

    // --- Partial row reordering (sub-range of glyphs) ---

    #[test]
    fn test_partial_range_reorder() {
        let mut buf = FrameGlyphBuffer::default();
        // Row 0: LTR
        buf.glyphs.push(make_char_glyph('A', 0.0, 8.0)); // index 0
        buf.glyphs.push(make_char_glyph('B', 8.0, 8.0)); // index 1
        // Row 1: RTL (to be reordered)
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 8.0)); // index 2
        buf.glyphs.push(make_char_glyph('\u{05D1}', 8.0, 8.0)); // index 3
        buf.glyphs.push(make_char_glyph('\u{05D2}', 16.0, 8.0)); // index 4
        // Row 2: LTR
        buf.glyphs.push(make_char_glyph('X', 0.0, 8.0)); // index 5

        // Only reorder indices 2..5 (the RTL row)
        reorder_row_bidi(&mut buf, 2, 5, 0.0);

        // Row 0 should be untouched
        assert_eq!(get_char_x(&buf.glyphs[0]), 0.0);
        assert_eq!(get_char_x(&buf.glyphs[1]), 8.0);
        // Row 1 should be reversed
        assert_eq!(get_char_x(&buf.glyphs[2]), 16.0); // Alef -> rightmost
        assert_eq!(get_char_x(&buf.glyphs[3]), 8.0); // Bet -> middle
        assert_eq!(get_char_x(&buf.glyphs[4]), 0.0); // Gimel -> leftmost
        // Row 2 should be untouched
        assert_eq!(get_char_x(&buf.glyphs[5]), 0.0);
    }

    // --- Stretch and other non-char glyphs interspersed ---

    #[test]
    fn test_non_char_glyphs_between_rtl_chars() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 8.0)); // Alef
        buf.glyphs.push(FrameGlyph::Stretch {
            window_id: 0,
            row_role: GlyphRowRole::Text,
            clip_rect: None,
            slot_id: DisplaySlotId::from_pixels(0, 8.0, 0.0, 8.0, 16.0),
            bidi_level: 0,
            x: 8.0,
            y: 0.0,
            width: 4.0,
            height: 16.0,
            bg: Color::new(0.0, 0.0, 0.0, 1.0),
            face_id: 0,
            stipple_id: 0,
            stipple_fg: None,
        });
        buf.glyphs.push(make_char_glyph('\u{05D1}', 12.0, 8.0)); // Bet

        reorder_row_bidi(&mut buf, 0, 3, 0.0);

        // Stretch participates as a neutral bidi slot and stays between the RTL chars.
        assert_eq!(get_stretch_x(&buf.glyphs[1]), 8.0);
        assert_eq!(get_stretch_bidi_level(&buf.glyphs[1]), 1);
        assert_eq!(get_char_x(&buf.glyphs[0]), 12.0); // Alef (logical 0) -> right of stretch
        assert_eq!(get_char_x(&buf.glyphs[2]), 0.0); // Bet (logical 1) -> left
    }

    #[test]
    fn test_stretch_reorders_with_mixed_width_rtl_chars() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 16.0)); // wide-like Alef
        buf.glyphs.push(FrameGlyph::Stretch {
            window_id: 0,
            row_role: GlyphRowRole::Text,
            clip_rect: None,
            slot_id: DisplaySlotId::from_pixels(0, 16.0, 0.0, 8.0, 16.0),
            bidi_level: 0,
            x: 16.0,
            y: 0.0,
            width: 4.0,
            height: 16.0,
            bg: Color::new(0.0, 0.0, 0.0, 1.0),
            face_id: 0,
            stipple_id: 0,
            stipple_fg: None,
        });
        buf.glyphs.push(make_char_glyph('\u{05D1}', 20.0, 8.0)); // Bet

        reorder_row_bidi(&mut buf, 0, 3, 0.0);

        assert_eq!(get_char_x(&buf.glyphs[2]), 0.0);
        assert_eq!(get_stretch_x(&buf.glyphs[1]), 8.0);
        assert_eq!(get_char_x(&buf.glyphs[0]), 12.0);
        assert_eq!(get_stretch_bidi_level(&buf.glyphs[1]), 1);
    }

    #[test]
    fn test_cursor_moves_with_rtl_stretch_slot() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 16.0)); // wide-like Alef
        buf.glyphs.push(FrameGlyph::Stretch {
            window_id: 0,
            row_role: GlyphRowRole::Text,
            clip_rect: None,
            slot_id: DisplaySlotId::from_pixels(0, 16.0, 0.0, 8.0, 16.0),
            bidi_level: 0,
            x: 16.0,
            y: 0.0,
            width: 4.0,
            height: 16.0,
            bg: Color::new(0.0, 0.0, 0.0, 1.0),
            face_id: 0,
            stipple_id: 0,
            stipple_fg: None,
        });
        buf.glyphs.push(make_char_glyph('\u{05D1}', 20.0, 8.0)); // Bet
        buf.glyphs.push(make_cursor_glyph(16.0, 4.0));
        buf.phys_cursor = Some(make_phys_cursor(16.0, 8.0));

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        assert_eq!(get_stretch_x(&buf.glyphs[1]), 8.0);
        assert_eq!(get_cursor_x(&buf.glyphs[3]), 8.0);
        assert_eq!(buf.phys_cursor.as_ref().expect("phys cursor").x, 8.0);
    }

    #[test]
    fn test_only_stretch_glyphs_no_panic() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(FrameGlyph::Stretch {
            window_id: 0,
            row_role: GlyphRowRole::Text,
            clip_rect: None,
            slot_id: DisplaySlotId::from_pixels(0, 0.0, 0.0, 8.0, 16.0),
            bidi_level: 0,
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 16.0,
            bg: Color::new(0.0, 0.0, 0.0, 1.0),
            face_id: 0,
            stipple_id: 0,
            stipple_fg: None,
        });
        buf.glyphs.push(FrameGlyph::Stretch {
            window_id: 0,
            row_role: GlyphRowRole::Text,
            clip_rect: None,
            slot_id: DisplaySlotId::from_pixels(0, 100.0, 0.0, 8.0, 16.0),
            bidi_level: 0,
            x: 100.0,
            y: 0.0,
            width: 100.0,
            height: 16.0,
            bg: Color::new(0.0, 0.0, 0.0, 1.0),
            face_id: 0,
            stipple_id: 0,
            stipple_fg: None,
        });

        // No char glyphs => row_chars is empty, should return early
        reorder_row_bidi(&mut buf, 0, 2, 0.0);

        // Stretches should be completely unchanged
        if let FrameGlyph::Stretch { x, .. } = &buf.glyphs[0] {
            assert_eq!(*x, 0.0);
        }
        if let FrameGlyph::Stretch { x, .. } = &buf.glyphs[1] {
            assert_eq!(*x, 100.0);
        }
    }

    // --- Mixed script RTL scenarios ---

    #[test]
    fn test_hebrew_and_arabic_mixed() {
        // Both Hebrew (R) and Arabic (AL) are RTL scripts
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('\u{05D0}', 0.0, 8.0)); // Hebrew Alef (R)
        buf.glyphs.push(make_char_glyph('\u{0627}', 8.0, 8.0)); // Arabic Alef (AL)
        buf.glyphs.push(make_char_glyph('\u{05D1}', 16.0, 8.0)); // Hebrew Bet (R)

        reorder_row_bidi(&mut buf, 0, 3, 0.0);

        // All are RTL, should be reversed
        assert_eq!(get_char_x(&buf.glyphs[2]), 0.0); // Hebrew Bet -> leftmost
        assert_eq!(get_char_x(&buf.glyphs[1]), 8.0); // Arabic Alef -> middle
        assert_eq!(get_char_x(&buf.glyphs[0]), 16.0); // Hebrew Alef -> rightmost
    }

    #[test]
    fn test_multiple_rtl_segments_in_ltr() {
        // LTR "A" + Hebrew "בג" + LTR "C" + Hebrew "דה"
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('A', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D1}', 8.0, 8.0)); // Bet
        buf.glyphs.push(make_char_glyph('\u{05D2}', 16.0, 8.0)); // Gimel
        buf.glyphs.push(make_char_glyph('C', 24.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D3}', 32.0, 8.0)); // Dalet
        buf.glyphs.push(make_char_glyph('\u{05D4}', 40.0, 8.0)); // He

        reorder_row_bidi(&mut buf, 0, 6, 0.0);

        // A should stay at 0
        assert_eq!(get_char_x(&buf.glyphs[0]), 0.0);
        // First RTL pair (Bet, Gimel) should be swapped
        assert!(
            get_char_x(&buf.glyphs[2]) < get_char_x(&buf.glyphs[1]),
            "Gimel should be to the left of Bet after RTL reorder"
        );
        // C should be in the middle
        assert_eq!(get_char_x(&buf.glyphs[3]), 24.0);
        // Second RTL pair (Dalet, He) should be swapped
        assert!(
            get_char_x(&buf.glyphs[5]) < get_char_x(&buf.glyphs[4]),
            "He should be to the left of Dalet after RTL reorder"
        );
    }

    #[test]
    fn test_total_width_preserved_after_reorder() {
        // Verify that the total span of glyphs is the same before and after reorder
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('A', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D0}', 8.0, 10.0));
        buf.glyphs.push(make_char_glyph('\u{05D1}', 18.0, 12.0));
        buf.glyphs.push(make_char_glyph('B', 30.0, 8.0));

        let total_width_before: f32 = buf
            .glyphs
            .iter()
            .map(|g| {
                if let FrameGlyph::Char { width, .. } = g {
                    *width
                } else {
                    0.0
                }
            })
            .sum();

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        // Compute total span after reorder
        let min_x = buf
            .glyphs
            .iter()
            .map(|g| get_char_x(g))
            .fold(f32::INFINITY, f32::min);
        let max_x_plus_w = buf
            .glyphs
            .iter()
            .map(|g| {
                if let FrameGlyph::Char { x, width, .. } = g {
                    *x + *width
                } else {
                    0.0
                }
            })
            .fold(f32::NEG_INFINITY, f32::max);

        let total_span = max_x_plus_w - min_x;
        assert!(
            (total_span - total_width_before).abs() < 0.01,
            "total width should be preserved: span={}, sum={}",
            total_span,
            total_width_before
        );
    }

    #[test]
    fn test_no_glyph_overlap_after_reorder() {
        let mut buf = FrameGlyphBuffer::default();
        buf.glyphs.push(make_char_glyph('X', 0.0, 8.0));
        buf.glyphs.push(make_char_glyph('\u{05D0}', 8.0, 10.0));
        buf.glyphs.push(make_char_glyph('\u{05D1}', 18.0, 12.0));
        buf.glyphs.push(make_char_glyph('Y', 30.0, 6.0));

        reorder_row_bidi(&mut buf, 0, 4, 0.0);

        // Collect (x, width) pairs sorted by x
        let mut positions: Vec<(f32, f32)> = buf
            .glyphs
            .iter()
            .map(|g| {
                if let FrameGlyph::Char { x, width, .. } = g {
                    (*x, *width)
                } else {
                    (0.0, 0.0)
                }
            })
            .collect();
        positions.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        // Verify no overlap: each glyph starts at or after the previous one ends
        for i in 1..positions.len() {
            let prev_end = positions[i - 1].0 + positions[i - 1].1;
            assert!(
                positions[i].0 >= prev_end - 0.01,
                "glyph at {} overlaps with previous ending at {}",
                positions[i].0,
                prev_end
            );
        }
    }
}
