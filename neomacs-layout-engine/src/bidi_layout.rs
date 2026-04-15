//! Bidi (bidirectional text) integration for the Rust layout engine.
//!
//! Provides helpers to reorder glyph X positions within a completed row
//! according to the Unicode Bidirectional Algorithm (UAX#9).
//!
//! The integration works at row completion: after all characters on a line
//! have been laid out left-to-right, this module reorders their X positions
//! so that RTL runs appear in the correct visual order.

use crate::bidi::{self, BidiDir};
use neomacs_display_protocol::frame_glyphs::{DisplaySlotId, FrameGlyph, FrameGlyphBuffer};

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
        let slot_y_offsets: Vec<(DisplaySlotId, f32)> = row_chars
            .iter()
            .filter_map(|info| match &frame_glyphs.glyphs[info.glyph_idx] {
                FrameGlyph::Char {
                    slot_id, ascent, ..
                } => Some((*slot_id, row_max_ascent - *ascent)),
                _ => None,
            })
            .collect();

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

        // Step C: Adjust cursor positions if any cursor visual covers a slot
        // on this row. Decorative window cursors and the active phys cursor
        // must stay attached to the same display slot baseline.
        if let Some(cursor) = frame_glyphs.phys_cursor.as_mut()
            && let Some(offset) = slot_offset_for(cursor.slot_id, &slot_y_offsets)
            && offset.abs() > 0.01
        {
            cursor.y += offset;
        }
        for cursor in &mut frame_glyphs.window_cursors {
            if let Some(offset) = slot_offset_for(cursor.slot_id, &slot_y_offsets)
                && offset.abs() > 0.01
            {
                cursor.y += offset;
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
    // Cursors were placed at LTR X positions; move both the active phys cursor
    // and decorative window cursor visuals to the exact reordered slot.
    let slot_x_positions: Vec<(DisplaySlotId, f32)> = row_chars
        .iter()
        .enumerate()
        .filter_map(
            |(logical_idx, info)| match &frame_glyphs.glyphs[info.glyph_idx] {
                FrameGlyph::Char { slot_id, .. } | FrameGlyph::Stretch { slot_id, .. } => {
                    Some((*slot_id, new_x[logical_idx]))
                }
                _ => None,
            },
        )
        .collect();
    if let Some(ref mut cursor) = frame_glyphs.phys_cursor {
        if let Some(new_cursor_x) = slot_x_for(cursor.slot_id, &slot_x_positions) {
            cursor.x = new_cursor_x;
        }
    }
    for cursor in &mut frame_glyphs.window_cursors {
        if let Some(new_cursor_x) = slot_x_for(cursor.slot_id, &slot_x_positions) {
            cursor.x = new_cursor_x;
        }
    }
}

fn slot_offset_for(slot_id: DisplaySlotId, slot_offsets: &[(DisplaySlotId, f32)]) -> Option<f32> {
    slot_offsets
        .iter()
        .find_map(|(slot, offset)| (*slot == slot_id).then_some(*offset))
}

fn slot_x_for(slot_id: DisplaySlotId, slot_positions: &[(DisplaySlotId, f32)]) -> Option<f32> {
    slot_positions
        .iter()
        .find_map(|(slot, x)| (*slot == slot_id).then_some(*x))
}

#[cfg(test)]
#[path = "bidi_layout_test.rs"]
mod tests;
