//! Backend abstraction for the display engine.
//!
//! This is neomacs's equivalent of GNU Emacs's
//! `struct redisplay_interface` ("RIF") defined in
//! `src/dispextern.h:3033`. It sits between the shared display walker
//! (which will be introduced in Step 3.3 of the display-engine
//! unification plan) and the frontend-specific output stages.
//!
//! The trait is dormant at introduction: no code calls it yet. Later
//! steps in the refactor will route the buffer-text walker, the
//! mode-line walker, and the tab-bar/header-line/tab-line/minibuffer
//! walkers through the same trait object so that every display source
//! flows through one code path and benefits from the shared
//! `display_pixel_calc` evaluator for `(space :align-to …)` forms.
//!
//! Backend impls (planned):
//!
//! - `TtyDisplayBackend` (Step 3.1 — this file): cell-based
//!   measurement, synchronous write-row on the eval thread, matches
//!   GNU's TTY backend in `term.c`.
//! - `GuiDisplayBackend` (Step 4.1): cosmic-text-based measurement,
//!   defers glyph rasterization to the render thread.
//!
//! See `docs/plans/2026-04-11-display-engine-unification-execution.md`
//! for the full plan.

use neomacs_display_protocol::face::Face;
use neomacs_display_protocol::glyph_matrix::GlyphRow;

/// Which kind of glyph the display walker is asking the backend to
/// produce. Mirrors GNU's `enum it_method` values that feed through
/// `PRODUCE_GLYPHS` (`dispextern.h:2926`). Neomacs only needs the
/// subset that corresponds to actual output — iterator-internal
/// methods like `GET_FROM_BUFFER` don't map to backend operations.
#[derive(Debug, Clone, Copy)]
pub enum GlyphKind {
    /// A normal character glyph.
    Char(char),
    /// A "glyphless" character (terminal can't render it) — the
    /// backend produces a hex-escape or acronym depending on
    /// configuration. GNU: `produce_glyphless_glyph`
    /// (`term.c:1935`).
    Glyphless(char),
    /// A stretch glyph from a `(space :width …)` or
    /// `(space :align-to …)` display spec. The walker has already
    /// evaluated the expression via
    /// `display_pixel_calc::calc_pixel_width_or_height` and supplies
    /// the pre-computed pixel width. GNU: `produce_stretch_glyph`
    /// (`xdisp.c:32510`).
    Stretch {
        width_px: f32,
        ascent: f32,
        descent: f32,
    },
}

/// Backend trait for the display walker.
///
/// Each method mirrors a call the walker makes for one element in a
/// glyph row. The trait intentionally does **not** carry the walker's
/// iterator state — the walker owns a `struct It` equivalent (coming
/// in Step 3.2) and passes the minimal per-element data to each
/// call.
///
/// This is the "RIF" boundary for neomacs: above the trait,
/// everything is frontend-agnostic (iterator, display-property
/// processing, mode-line / buffer-text walking); below the trait,
/// TTY/GUI implementations are free to choose their own measurement
/// and output strategies.
pub trait DisplayBackend {
    // ----- Measurement -----

    /// Return the pixel advance of a character in the given face.
    /// Called by the walker before emitting the glyph so that
    /// positions are tracked correctly.
    ///
    /// GNU: inline cell-width for TTY (`wcwidth`-equivalent, 1 or 2
    /// cells); font-metric-based shaping for GUI via
    /// `gui_produce_glyphs` (`xdisp.c:33185`).
    fn char_advance(&mut self, face: &Face, ch: char) -> f32;

    /// Return the font height for a face. Used by the walker to
    /// compute line ascent/descent.
    ///
    /// GNU: `normal_char_height(font, -1)` in `calc_pixel_width_or_height`
    /// (`xdisp.c:30158`).
    fn font_height(&mut self, face: &Face) -> f32;

    /// Return the font width for a face — the mean/space width used
    /// as a base unit for numeric `(space :width …)` forms.
    ///
    /// GNU: `font->average_width` or `font->space_width` fallback
    /// (`xdisp.c:30164`).
    fn font_width(&mut self, face: &Face) -> f32;

    // ----- Glyph production -----

    /// Produce a glyph of the given kind at the current walker
    /// position. The walker is responsible for advancing its own
    /// position counters after the call.
    ///
    /// GNU: the `PRODUCE_GLYPHS` macro (`dispextern.h:2926`) that
    /// dispatches to the per-backend `produce_glyphs` function.
    fn produce_glyph(&mut self, kind: GlyphKind, face: &Face, charpos: usize);

    // ----- Row completion -----

    /// Signal that the walker has finished producing glyphs for one
    /// logical display row. The backend can commit the row to its
    /// output buffer, run diff-and-emit for TTY, enqueue for the
    /// render thread for GUI, etc.
    ///
    /// GNU: `display_line` returns after filling the glyph row; the
    /// row is then picked up by `update_frame` / `write_matrix` in
    /// `dispnew.c`.
    fn finish_row(&mut self, row: GlyphRow);

    // ----- Frame completion -----

    /// Signal that the walker has finished producing all rows for
    /// one frame. The backend can flush its buffered output.
    fn finish_frame(&mut self);
}

// ---------------------------------------------------------------------------
// TtyDisplayBackend
// ---------------------------------------------------------------------------

/// Cell-based backend for the terminal UI.
///
/// All measurements are in character cells (1 for ASCII/narrow, 2 for
/// CJK/wide). Matches GNU's TTY backend in `term.c` at the architecture
/// level: synchronous, single-threaded, emits glyphs into a row buffer
/// that is later diffed and written as ANSI escape sequences.
///
/// **Dormant at introduction.** The struct holds the rows that the
/// walker produces but no code calls it yet. Step 3.3 wires it up to
/// the buffer-text walker path; Step 3.4 wires it up to mode-line.
pub struct TtyDisplayBackend {
    /// Accumulated rows from the current frame. Drained by
    /// `finish_frame` into whatever output pipeline the caller
    /// provides.
    pending_rows: Vec<GlyphRow>,
    /// Default cell width in pixels, used for stretch-glyph width
    /// computation. Normally 1.0 for pure cell-grid TUI.
    cell_width_px: f32,
    /// Default cell height in pixels. Normally 1.0.
    cell_height_px: f32,
}

impl TtyDisplayBackend {
    pub fn new() -> Self {
        Self {
            pending_rows: Vec::new(),
            cell_width_px: 1.0,
            cell_height_px: 1.0,
        }
    }

    /// Take the accumulated rows since the last drain. Used by
    /// callers that want to feed the rows into `TtyRif::rasterize`
    /// or a similar output stage.
    pub fn take_rows(&mut self) -> Vec<GlyphRow> {
        std::mem::take(&mut self.pending_rows)
    }
}

impl Default for TtyDisplayBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayBackend for TtyDisplayBackend {
    fn char_advance(&mut self, _face: &Face, ch: char) -> f32 {
        // Cell-based: wide characters (CJK, emoji) take 2 cells,
        // everything else 1. `is_wide_char` lives in
        // `engine.rs` today and we reimplement the simple predicate
        // here rather than cross-crate-depending.
        if is_wide_char_inline(ch) {
            2.0 * self.cell_width_px
        } else {
            self.cell_width_px
        }
    }

    fn font_height(&mut self, _face: &Face) -> f32 {
        self.cell_height_px
    }

    fn font_width(&mut self, _face: &Face) -> f32 {
        self.cell_width_px
    }

    fn produce_glyph(&mut self, _kind: GlyphKind, _face: &Face, _charpos: usize) {
        // TODO(Step 3.3): append the glyph to the in-progress row.
        // The current `GlyphRow` API is built up by
        // `GlyphMatrixBuilder` in `matrix_builder.rs`; we'll route
        // through the same builder from here or take ownership of
        // a row-in-progress directly.
    }

    fn finish_row(&mut self, row: GlyphRow) {
        self.pending_rows.push(row);
    }

    fn finish_frame(&mut self) {
        // Rows stay queued until `take_rows` is called.
    }
}

/// Inline replica of `engine::is_wide_char` to avoid cross-module
/// coupling during the trait-introduction step. Once the walker is
/// folded into this module (Step 3.3) the original lives in one
/// place.
fn is_wide_char_inline(ch: char) -> bool {
    // Common double-width ranges. This is a placeholder — the real
    // table lives in unicode.rs and will be wired in when the walker
    // is ported. For now it handles CJK which is what ASCII+CJK
    // tests need.
    matches!(
        ch as u32,
        0x1100..=0x115F     // Hangul Jamo
            | 0x2E80..=0x303E  // CJK Radicals Supplement, Kangxi, Ideographic
            | 0x3041..=0x33FF  // Hiragana, Katakana, Bopomofo, Hangul Compat Jamo, CJK symbols
            | 0x3400..=0x4DBF  // CJK Unified Ideographs Extension A
            | 0x4E00..=0x9FFF  // CJK Unified Ideographs
            | 0xA000..=0xA4CF  // Yi
            | 0xAC00..=0xD7A3  // Hangul Syllables
            | 0xF900..=0xFAFF  // CJK Compatibility Ideographs
            | 0xFE30..=0xFE4F  // CJK Compatibility Forms
            | 0xFF00..=0xFF60  // Fullwidth forms
            | 0xFFE0..=0xFFE6  // Fullwidth signs
            | 0x20000..=0x2FFFD // CJK Extension B-F
            | 0x30000..=0x3FFFD
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use neomacs_display_protocol::face::Face;

    fn default_face() -> Face {
        Face::default()
    }

    #[test]
    fn tty_char_advance_ascii_is_one_cell() {
        let mut be = TtyDisplayBackend::new();
        let f = default_face();
        assert_eq!(be.char_advance(&f, 'A'), 1.0);
        assert_eq!(be.char_advance(&f, ' '), 1.0);
        assert_eq!(be.char_advance(&f, '#'), 1.0);
    }

    #[test]
    fn tty_char_advance_cjk_is_two_cells() {
        let mut be = TtyDisplayBackend::new();
        let f = default_face();
        // 中 = U+4E2D (CJK Unified Ideograph)
        assert_eq!(be.char_advance(&f, '中'), 2.0);
        // あ = U+3042 (Hiragana)
        assert_eq!(be.char_advance(&f, 'あ'), 2.0);
    }

    #[test]
    fn tty_take_rows_drains() {
        let mut be = TtyDisplayBackend::new();
        // No rows yet.
        assert!(be.take_rows().is_empty());
    }

    #[test]
    fn tty_trait_object_is_usable() {
        // Compile-time check: the trait is object-safe.
        let mut be: Box<dyn DisplayBackend> = Box::new(TtyDisplayBackend::new());
        let f = default_face();
        let _ = be.char_advance(&f, 'x');
        let _ = be.font_height(&f);
        let _ = be.font_width(&f);
    }
}
