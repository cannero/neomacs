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

#[cfg(test)]
#[path = "glyph_matrix_test.rs"]
mod tests;
