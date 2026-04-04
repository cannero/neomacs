//! GNU Emacs-compatible glyph matrix types for the shared display path.
//!
//! These types match the architecture of GNU Emacs's `dispextern.h`:
//! `struct glyph`, `struct glyph_row`, `struct glyph_matrix`.
//!
//! The glyph matrix is character-grid native — no pixel coordinates.
//! Both TTY and GUI backends read from this representation.
//! TTY outputs directly; GUI converts to pixel positions on the render thread.

use super::face::Face;
use super::frame_glyphs::{CursorStyle, GlyphRowRole, WindowInfo, WindowTransitionHint};
use super::types::{Color, Rect};
use std::collections::HashMap;

/// What kind of content this glyph represents.
/// Matches GNU's `enum glyph_type` in `dispextern.h`.
#[derive(Clone, Debug, PartialEq)]
pub enum GlyphType {
    /// Regular character (including multibyte).
    Char { ch: char },
    /// Composed grapheme cluster (ligatures, emoji ZWJ, combining marks).
    Composite { text: Box<str> },
    /// Whitespace/filler — occupies `width_cols` character cells.
    Stretch { width_cols: u16 },
    /// Inline image referenced by ID.
    Image { image_id: i32 },
    /// Character with no available glyph (rendered as hex code or thin-space).
    Glyphless { ch: char },
}

/// Three areas within a glyph row, matching GNU's `enum glyph_row_area`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlyphArea {
    LeftMargin = 0,
    Text = 1,
    RightMargin = 2,
}

/// One character cell on screen.
/// Equivalent to GNU's `struct glyph` in `dispextern.h`.
///
/// Grid-native: no pixel coordinates. Screen position is determined by
/// the row index in `GlyphRow` and position within the area's glyph vector.
#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    /// What this glyph displays.
    pub glyph_type: GlyphType,
    /// Face ID for looking up colors, font, and decoration.
    pub face_id: u32,
    /// Buffer position this glyph maps to (for cursor placement, mouse clicks).
    pub charpos: usize,
    /// Bidirectional resolved level (0 = LTR base, 1 = RTL, etc.).
    pub bidi_level: u8,
    /// True for double-width characters (CJK, etc.).
    pub wide: bool,
    /// Padding glyph — second cell of a wide character.
    pub padding: bool,
}

impl Glyph {
    /// Create a simple character glyph with default attributes.
    pub fn char(ch: char, face_id: u32, charpos: usize) -> Self {
        Self {
            glyph_type: GlyphType::Char { ch },
            face_id,
            charpos,
            bidi_level: 0,
            wide: false,
            padding: false,
        }
    }

    /// Create a stretch (whitespace) glyph.
    pub fn stretch(width_cols: u16, face_id: u32) -> Self {
        Self {
            glyph_type: GlyphType::Stretch { width_cols },
            face_id,
            charpos: 0,
            bidi_level: 0,
            wide: false,
            padding: false,
        }
    }

    /// Create a padding glyph (second cell of a wide character).
    pub fn padding_for(face_id: u32, charpos: usize) -> Self {
        Self {
            glyph_type: GlyphType::Char { ch: ' ' },
            face_id,
            charpos,
            bidi_level: 0,
            wide: false,
            padding: true,
        }
    }
}

/// One screen row. Equivalent to GNU's `struct glyph_row`.
///
/// Contains three glyph areas (left margin, text, right margin) matching
/// GNU's layout. Row hashing enables fast diff: if hashes match, the rows
/// are likely identical; if they differ, the row needs redrawing.
#[derive(Clone, Debug)]
pub struct GlyphRow {
    /// Glyphs per area: [left_margin, text, right_margin].
    pub glyphs: [Vec<Glyph>; 3],
    /// Row hash for fast diff. 0 = not yet computed.
    pub hash: u64,
    /// Row is valid and should be displayed.
    pub enabled: bool,
    /// Semantic role: text body, mode-line, header-line, tab-line, etc.
    pub role: GlyphRowRole,
    /// Cursor column in this row, if cursor is here.
    pub cursor_col: Option<u16>,
    /// Cursor type when cursor is in this row.
    pub cursor_type: Option<CursorStyle>,
    /// Row has been truncated on the left.
    pub truncated_left: bool,
    /// Row has a continuation mark on the right.
    pub continued: bool,
    /// Row displays actual buffer text (not blank filler).
    pub displays_text: bool,
    /// Row ends at end of buffer.
    pub ends_at_zv: bool,
    /// This is a mode-line, header-line, or tab-line row.
    pub mode_line: bool,
    /// Buffer position at start of this row.
    pub start_charpos: usize,
    /// Buffer position at end of this row.
    pub end_charpos: usize,
}

impl GlyphRow {
    pub fn new(role: GlyphRowRole) -> Self {
        Self {
            glyphs: [Vec::new(), Vec::new(), Vec::new()],
            hash: 0,
            enabled: true,
            role,
            cursor_col: None,
            cursor_type: None,
            truncated_left: false,
            continued: false,
            displays_text: false,
            ends_at_zv: false,
            mode_line: false,
            start_charpos: 0,
            end_charpos: 0,
        }
    }

    /// Compute FNV-1a hash over all glyph areas.
    /// Returns 0 for empty rows (sentinel meaning "not computed").
    pub fn compute_hash(&self) -> u64 {
        let total: usize = self.glyphs.iter().map(|a| a.len()).sum();
        if total == 0 {
            return 0;
        }

        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;

        let mut hash = FNV_OFFSET;
        for area in &self.glyphs {
            for glyph in area {
                let ch_val = match &glyph.glyph_type {
                    GlyphType::Char { ch } => *ch as u64,
                    GlyphType::Composite { text } => {
                        let mut h = 0u64;
                        for b in text.bytes() {
                            h = h.wrapping_mul(31).wrapping_add(b as u64);
                        }
                        h
                    }
                    GlyphType::Stretch { width_cols } => 0x8000_0000 | (*width_cols as u64),
                    GlyphType::Image { image_id } => 0x4000_0000 | (*image_id as u64),
                    GlyphType::Glyphless { ch } => 0x2000_0000 | (*ch as u64),
                };
                hash ^= ch_val;
                hash = hash.wrapping_mul(FNV_PRIME);
                hash ^= glyph.face_id as u64;
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        }
        hash
    }

    pub fn row_equal(&self, other: &GlyphRow) -> bool {
        if self.hash != 0 && other.hash != 0 && self.hash != other.hash {
            return false;
        }
        for i in 0..3 {
            if self.glyphs[i].len() != other.glyphs[i].len() {
                return false;
            }
            for (a, b) in self.glyphs[i].iter().zip(other.glyphs[i].iter()) {
                if a != b {
                    return false;
                }
            }
        }
        true
    }

    pub fn used(&self, area: GlyphArea) -> usize {
        self.glyphs[area as usize].len()
    }

    pub fn total_glyphs(&self) -> usize {
        self.glyphs[0].len() + self.glyphs[1].len() + self.glyphs[2].len()
    }

    pub fn clear(&mut self) {
        for area in &mut self.glyphs {
            area.clear();
        }
        self.hash = 0;
        self.cursor_col = None;
        self.cursor_type = None;
        self.truncated_left = false;
        self.continued = false;
        self.displays_text = false;
        self.ends_at_zv = false;
        self.start_charpos = 0;
        self.end_charpos = 0;
    }
}

#[cfg(test)]
#[path = "glyph_matrix_test.rs"]
mod tests;
