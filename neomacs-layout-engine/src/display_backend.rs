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
use neomacs_display_protocol::glyph_matrix::{Glyph, GlyphRow};

use crate::font_metrics::FontMetricsService;

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

    // ----- Output drain -----

    /// Drain all rows accumulated since the last drain. Callers use
    /// this to hand the completed glyph rows off to the frame
    /// matrix builder. GNU: no direct equivalent — GNU's backend
    /// writes into the caller-supplied `glyph_row` in place, but
    /// neomacs accumulates rows inside the backend so trait-object
    /// dispatch can select TTY vs GUI paths at runtime.
    fn take_rows(&mut self) -> Vec<GlyphRow>;

    /// Read-only view of the in-progress glyphs for the row
    /// currently being walked. Used by the frame tab-bar installer
    /// which peeks at the glyphs before calling `finish_row`.
    fn pending_glyphs(&self) -> &[Glyph];
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
/// The backend accumulates glyphs in `pending_glyphs` as the walker
/// calls `produce_glyph`. When the walker calls `finish_row`, the
/// buffered glyphs are flushed into the caller-supplied
/// `GlyphRow::glyphs[Text]` and the row is pushed onto `pending_rows`.
/// Callers drain the completed rows via `take_rows`.
///
/// **Dormant at introduction.** Unit tests cover the basic push/drain
/// cycle; no existing code routes glyphs through this backend yet.
/// Steps 3.4+ wire it into the minibuffer-echo, tab-bar, and
/// mode-line paths incrementally.
pub struct TtyDisplayBackend {
    /// Glyphs accumulated for the row currently being walked.
    /// Flushed to `pending_rows` on `finish_row`.
    pending_glyphs: Vec<Glyph>,
    /// Completed rows from the current frame. Drained via `take_rows`.
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
            pending_glyphs: Vec::new(),
            pending_rows: Vec::new(),
            cell_width_px: 1.0,
            cell_height_px: 1.0,
        }
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

    fn produce_glyph(&mut self, kind: GlyphKind, face: &Face, charpos: usize) {
        // Build a `Glyph` from the kind and append to the in-progress
        // row. For TTY we use the same glyph representation the rest
        // of the layout engine uses, so downstream rasterization via
        // TtyRif continues to work unchanged. The face_id comes from
        // the caller's resolved face.
        let face_id: u32 = face.id;
        let glyph = match kind {
            GlyphKind::Char(ch) => Glyph::char(ch, face_id, charpos),
            GlyphKind::Glyphless(ch) => Glyph::char(ch, face_id, charpos),
            GlyphKind::Stretch { width_px, .. } => {
                // Convert pixel width to cell count using this
                // backend's cell width (normally 1.0 for TTY).
                let cols = (width_px / self.cell_width_px.max(1.0)).round() as u16;
                Glyph::stretch(cols.max(1), face_id)
            }
        };
        self.pending_glyphs.push(glyph);
    }

    fn finish_row(&mut self, mut row: GlyphRow) {
        // Move the accumulated glyphs into the row's Text area and
        // push the row onto the completed-rows queue. The row's
        // three glyph areas are [left_margin, text, right_margin]
        // matching GNU's glyph_row layout (`dispextern.h`).
        let text_glyphs = std::mem::take(&mut self.pending_glyphs);
        // Index 1 is the text area (matches
        // `neomacs_display_protocol::glyph_matrix::GlyphArea::Text`).
        row.glyphs[1] = text_glyphs;
        self.pending_rows.push(row);
    }

    fn finish_frame(&mut self) {
        // Rows stay queued until `take_rows` is called.
    }

    fn take_rows(&mut self) -> Vec<GlyphRow> {
        std::mem::take(&mut self.pending_rows)
    }

    fn pending_glyphs(&self) -> &[Glyph] {
        &self.pending_glyphs
    }
}

// ---------------------------------------------------------------------------
// GuiDisplayBackend
// ---------------------------------------------------------------------------

/// Pixel-metric backend for the GUI renderer.
///
/// Mirrors the TTY backend's glyph accumulation strategy (the
/// produced `GlyphRow`s look identical in structure) but routes
/// character advance and font metric queries through the shared
/// `FontMetricsService`, which delegates to cosmic-text's shaping
/// and font-metrics lookups. This matches GNU's
/// `gui_produce_glyphs` path in `xdisp.c:33185`, where glyph rows
/// are assembled on the eval thread using the GUI frame's
/// `redisplay_interface` vtable before being handed to the
/// window-system write pipeline.
///
/// The backend borrows the `FontMetricsService` mutably for its
/// lifetime. Callers construct one per walker pass and discard it
/// immediately after draining via `take_rows`, so the borrow never
/// races with any other consumer of `font_metrics` on
/// `LayoutEngine`.
///
/// The pixel-accurate `char_advance` replaces the cell-based value
/// that `TtyDisplayBackend` returns, so text-break calculations
/// inside the walker (`display_text_plain_via_backend` etc.) use
/// the same coordinate system as the `text_width` / `max_width`
/// parameters passed in from GUI-mode call sites.
pub struct GuiDisplayBackend<'a> {
    font_svc: &'a mut FontMetricsService,
    inner: TtyDisplayBackend,
}

impl<'a> GuiDisplayBackend<'a> {
    /// Construct a new GUI backend borrowing the given
    /// `FontMetricsService`. The service remains usable again once
    /// this backend is dropped.
    pub fn new(font_svc: &'a mut FontMetricsService) -> Self {
        Self {
            font_svc,
            inner: TtyDisplayBackend::new(),
        }
    }
}

impl<'a> DisplayBackend for GuiDisplayBackend<'a> {
    fn char_advance(&mut self, face: &Face, ch: char) -> f32 {
        // Delegate to the shared font metrics service; it already
        // caches ASCII widths per face-configuration key.
        let w = self.font_svc.char_width(
            ch,
            &face.font_family,
            face.font_weight,
            face.is_italic(),
            face.font_size,
        );
        if w > 0.0 { w } else { face.font_size.max(1.0) }
    }

    fn font_height(&mut self, face: &Face) -> f32 {
        let m = self.font_svc.font_metrics(
            &face.font_family,
            face.font_weight,
            face.is_italic(),
            face.font_size,
        );
        m.line_height
    }

    fn font_width(&mut self, face: &Face) -> f32 {
        let m = self.font_svc.font_metrics(
            &face.font_family,
            face.font_weight,
            face.is_italic(),
            face.font_size,
        );
        m.char_width
    }

    fn produce_glyph(&mut self, kind: GlyphKind, face: &Face, charpos: usize) {
        // Glyph accumulation is identical to the TTY path. The only
        // difference between GUI and TTY is measurement, which
        // already went through `char_advance` above before the
        // walker called back here.
        self.inner.produce_glyph(kind, face, charpos);
    }

    fn finish_row(&mut self, row: GlyphRow) {
        self.inner.finish_row(row);
    }

    fn finish_frame(&mut self) {
        self.inner.finish_frame();
    }

    fn take_rows(&mut self) -> Vec<GlyphRow> {
        self.inner.take_rows()
    }

    fn pending_glyphs(&self) -> &[Glyph] {
        self.inner.pending_glyphs()
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
// Plain-text walker helper
// ---------------------------------------------------------------------------

/// Emit a plain (non-propertized) text string into a `DisplayBackend`
/// as character glyphs, stopping when the accumulated pixel width
/// would exceed `max_width`. The backend produces glyphs; the caller
/// is responsible for `finish_row` and draining the rows.
///
/// This mirrors the inner loop of GNU's `display_string` for the
/// simplest case: one face, no display properties, no align-to
/// entries. The minibuffer-echo and frame-tab-bar callers use this
/// because they receive a plain `String`, not a propertized `Value`.
///
/// Returns the total pixel advance actually produced (useful for
/// callers that need to know how much of the string was consumed).
///
/// Line terminators (`\n`, `\r`) are skipped — status lines never
/// produce more than one row, matching the behavior of the
/// `render_text_run` method in `status_line.rs`.
pub fn display_text_plain_via_backend(
    backend: &mut dyn DisplayBackend,
    text: &str,
    face: &Face,
    char_width: f32,
    max_width: f32,
) -> f32 {
    let mut x_offset = 0.0f32;
    let mut charpos: usize = 0;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            charpos += 1;
            continue;
        }
        let advance = {
            // Prefer the backend's per-face char advance when it
            // returns a positive value (cell-based TTY or cosmic-text
            // GUI). Fall back to the caller-supplied `char_width` —
            // for TTY backends that treat all cells as width=1.0 but
            // the status-line fallback width may be a different
            // number when fonts report explicit `font_char_width`.
            let a = backend.char_advance(face, ch);
            if a > 0.0 { a } else { char_width.max(1.0) }
        };
        if x_offset + advance > max_width {
            break;
        }
        backend.produce_glyph(GlyphKind::Char(ch), face, charpos);
        x_offset += advance;
        charpos += 1;
    }
    x_offset
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "display_backend_test.rs"]
mod tests;
