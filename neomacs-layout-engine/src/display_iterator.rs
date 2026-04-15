//! Display iterator: the Rust equivalent of GNU Emacs's `struct it`
//! (defined in `dispextern.h:2394`).
//!
//! The iterator carries all per-element state as the display walker
//! advances through a buffer, string, overlay, mode-line format, or
//! similar source. It is the central data structure of the display
//! engine: one `It` is initialized at the start of a redisplay pass
//! and is threaded through every layer of the walker, from
//! `redisplay_window` down to `produce_glyphs`.
//!
//! **Dormant at introduction.** This file defines the type and a
//! constructor; nothing uses it yet. Step 3.3 of the display-engine
//! unification plan wires the iterator into a new walker that
//! replaces the mode-line rendering path (and eventually the
//! buffer-text rendering path).
//!
//! # What this port includes (and doesn't)
//!
//! GNU's `struct it` has ~300 lines of fields, many of which are
//! either `#ifdef HAVE_WINDOW_SYSTEM`-guarded or carry xdisp-internal
//! cache state. We port the fields that `display_line`,
//! `display_mode_element`, `handle_display_prop`, and
//! `produce_glyphs` actually read/write — the minimum viable surface
//! to run the mode-line walker correctly.
//!
//! **Bidi fields are explicitly included.** `bidi_it`,
//! `paragraph_embedding`, and `bidi_p` are core iterator state used
//! by `display_line` regardless of backend. GNU has supported bidi
//! text rendering in TTY mode since Emacs 24. We declare the slots
//! now even though the actual bidi reordering algorithm is a
//! separate future project — the walker will skip reordering when
//! `bidi_p` is false, which is the day-1 neomacs behavior.
//!
//! See the proposal doc
//! (`docs/plans/2026-04-11-display-engine-unification.md`) and
//! execution plan
//! (`docs/plans/2026-04-11-display-engine-unification-execution.md`)
//! for the broader context.

use neomacs_display_protocol::glyph_matrix::GlyphRow;
use neovm_core::emacs_core::Value;

/// What the iterator is currently producing. GNU: `enum
/// it_method` (`dispextern.h:2380-ish`). Neomacs uses this to tell
/// the backend what kind of glyph to emit and to track which
/// "source" the iterator is drawing from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItMethod {
    /// The iterator is reading characters from a buffer.
    FromBuffer,
    /// The iterator is reading characters from a Lisp string
    /// (overlay string, display property string, mode-line format
    /// segment, etc.).
    FromString,
    /// The iterator is handling an image display spec.
    FromImage,
    /// The iterator is handling a stretch glyph from a
    /// `(space :width …)` or `(space :align-to …)` display spec.
    FromStretch,
    /// The iterator is handling a composition (multi-codepoint
    /// grapheme cluster, CJK composition, etc.).
    FromComposition,
}

/// What kind of display element the iterator is about to emit
/// next. GNU: `enum display_element_type` via `it->what`
/// (`dispextern.h:2402`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItWhat {
    /// No element ready; the iterator needs to advance.
    Empty,
    /// A character glyph (the normal case).
    Character,
    /// A composition glyph.
    Composition,
    /// A "glyphless" character (couldn't find a font glyph for it).
    Glyphless,
    /// An image.
    Image,
    /// A stretch glyph.
    Stretch,
    /// End of line / end of row.
    EolOrEob,
    /// Truncation marker (hit the right edge, line continues
    /// off-screen).
    Truncation,
    /// Continuation marker (line wraps to the next visible row).
    Continuation,
}

/// How line wrapping should be performed. GNU: `enum line_wrap_method`
/// (`dispextern.h:1340`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineWrap {
    /// Never wrap; truncate instead.
    TruncateLines,
    /// Wrap at the window edge without any special word-boundary
    /// handling.
    WordWrap,
    /// Wrap at whitespace/word boundaries.
    WrapAtWhitespace,
}

/// Bidirectional text direction. GNU: `bidi_dir_t`
/// (`dispextern.h:2400`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BidiDir {
    /// Direction isn't specified; defer to the paragraph's base
    /// direction.
    Neutral,
    /// Left-to-right.
    Ltr,
    /// Right-to-left.
    Rtl,
}

/// Placeholder bidi iterator state. GNU's `struct bidi_it` is a
/// large struct in `bidi.c`; neomacs will either port it or use its
/// existing `bidi_layout` module. For Step 3.2 this is a marker that
/// occupies the slot in `It` so downstream code can reference
/// `it.bidi_it` without type errors.
///
/// TODO(bidi): replace with a real bidi iterator port once we reach
/// Step 3.3 and discover what the walker actually needs.
#[derive(Debug, Clone, Default)]
pub struct BidiIt {
    /// Placeholder field so the struct isn't zero-sized.
    pub resolved_level: u8,
}

/// Rust equivalent of GNU's `struct it`.
///
/// # Invariants
///
/// - Fields that correspond directly to GNU struct-it fields are
///   documented with their GNU source locations.
/// - Fields present only in neomacs are marked "neomacs-only".
/// - The struct is large by necessity — GNU's original is also
///   large. This is the whole point of the abstraction: one place
///   for all per-element display state.
///
/// # Construction
///
/// Use `It::new_for_window(...)` to create an iterator positioned
/// at the start of a window's visible text, or `It::new_for_string
/// (...)` for mode-line / overlay string walks.
#[derive(Debug, Clone)]
pub struct It {
    // ----- Navigation -----
    /// Current character position in the source.
    /// GNU: `it->current.pos.charpos` (`dispextern.h:2444`).
    pub charpos: i64,
    /// Current byte position in the source.
    /// GNU: `it->current.pos.bytepos`.
    pub bytepos: i64,
    /// Character position where the next property change may
    /// occur. GNU: `it->stop_charpos` (`dispextern.h:2453`).
    pub stop_charpos: i64,
    /// Upper bound for iteration (end of buffer/string).
    /// GNU: `it->end_charpos` (`dispextern.h:2449`).
    pub end_charpos: i64,

    // ----- Content source -----
    /// The current source object — Lisp string when walking a
    /// mode-line / overlay / display-property string; nil when
    /// walking buffer text. GNU: `it->string` (`dispextern.h:2507`).
    pub string: Value,
    /// The overall "what we are iterating" — buffer vs string
    /// vs image vs stretch vs composition. GNU: `it->method`
    /// (`dispextern.h:2401`).
    pub method: ItMethod,
    /// What kind of glyph is ready to be emitted next. GNU:
    /// `it->what` (`dispextern.h:2402`).
    pub what: ItWhat,
    /// Set when the current string comes from a `display`
    /// property rather than a raw overlay. GNU:
    /// `it->from_disp_prop_p` (`dispextern.h:2595`).
    pub from_disp_prop_p: bool,

    // ----- Per-character -----
    /// The character currently being processed. GNU: `it->c`
    /// (`dispextern.h:2529`).
    pub c: i32,
    /// The character to actually display (may differ from `c` for
    /// glyphless handling or control-char decoding). GNU:
    /// `it->char_to_display` (`dispextern.h:2533`).
    pub char_to_display: i32,

    // ----- Face and glyph state -----
    /// The current face id. GNU: `it->face_id`
    /// (`dispextern.h:2527`).
    pub face_id: u32,

    // ----- Pixel/column position -----
    /// Current x coordinate (pixel). GNU: `it->current_x`
    /// (`dispextern.h:2543`).
    pub current_x: f32,
    /// Current y coordinate (pixel). GNU: `it->current_y`
    /// (`dispextern.h:2547`).
    pub current_y: f32,
    /// Ascent of the tallest glyph in the current row. GNU:
    /// `it->ascent` (`dispextern.h:2549`).
    pub ascent: f32,
    /// Descent of the tallest glyph. GNU: `it->descent`
    /// (`dispextern.h:2549`).
    pub descent: f32,
    /// Pixel width of the last-produced glyph. GNU:
    /// `it->pixel_width` (`dispextern.h:2551`).
    pub pixel_width: f32,

    // ----- Row target -----
    /// The glyph row this iterator is filling. Populated by the
    /// backend's `finish_row` callback when the walker reaches an
    /// end-of-row condition.
    /// neomacs-only: in GNU this is `it->glyph_row` and is set
    /// during `init_iterator`; here we keep the slot but let the
    /// walker own the row directly and pass it to the backend.
    pub glyph_row: Option<GlyphRow>,

    // ----- Line wrapping / multibyte -----
    /// Current line-wrap strategy. GNU: `it->line_wrap`
    /// (`dispextern.h:2604`).
    pub line_wrap: LineWrap,
    /// Whether the source carries multibyte characters. GNU:
    /// `it->multibyte_p` (`dispextern.h:2592`).
    pub multibyte_p: bool,

    // ----- Mode-line flag -----
    /// True when the glyph row this iterator is filling is a
    /// mode-line row. Mirrors GNU's `glyph_row->mode_line_p` flag
    /// set by `display_mode_line` (`xdisp.c:27887`).
    pub mode_line_p: bool,

    // ----- Bidi state (core, not X-specific) -----
    /// Whether bidi reordering is active. GNU: `it->bidi_p`
    /// (`dispextern.h:2597`). Note: this is NOT X-specific; GNU
    /// supports bidi in TTY mode too. Day-1 neomacs sets this to
    /// false and uses unicode-code-point order; a future commit
    /// will wire in real bidi.
    pub bidi_p: bool,
    /// Paragraph embedding direction for bidi reordering. GNU:
    /// `it->paragraph_embedding` (`dispextern.h:2591`).
    pub paragraph_embedding: BidiDir,
    /// Bidi iterator state. GNU: `it->bidi_it`
    /// (`dispextern.h:2891`, actual struct in `bidi.c`).
    pub bidi_it: BidiIt,
}

impl It {
    /// Create a new iterator for walking a mode-line format.
    /// Mirrors GNU's `init_iterator(&it, w, -1, -1, NULL, face_id)`
    /// pattern at `xdisp.c:27884`.
    ///
    /// The -1 charpos/bytepos is GNU's sentinel meaning "not a
    /// buffer position"; we represent it here with the same
    /// convention.
    pub fn new_for_mode_line(face_id: u32) -> Self {
        Self {
            charpos: -1,
            bytepos: -1,
            stop_charpos: i64::MAX,
            end_charpos: i64::MAX,
            string: Value::NIL,
            method: ItMethod::FromString,
            what: ItWhat::Empty,
            from_disp_prop_p: false,
            c: 0,
            char_to_display: 0,
            face_id,
            current_x: 0.0,
            current_y: 0.0,
            ascent: 0.0,
            descent: 0.0,
            pixel_width: 0.0,
            glyph_row: None,
            line_wrap: LineWrap::TruncateLines,
            multibyte_p: true,
            mode_line_p: true,
            bidi_p: false,
            paragraph_embedding: BidiDir::Ltr,
            bidi_it: BidiIt::default(),
        }
    }

    /// Create a new iterator for walking buffer text starting at
    /// the given position. Mirrors GNU's `init_iterator(&it, w,
    /// charpos, bytepos, row, face_id)` pattern used by
    /// `start_display` (`xdisp.c:ish`).
    pub fn new_for_buffer(charpos: i64, bytepos: i64, face_id: u32) -> Self {
        Self {
            charpos,
            bytepos,
            stop_charpos: charpos,
            end_charpos: i64::MAX,
            string: Value::NIL,
            method: ItMethod::FromBuffer,
            what: ItWhat::Empty,
            from_disp_prop_p: false,
            c: 0,
            char_to_display: 0,
            face_id,
            current_x: 0.0,
            current_y: 0.0,
            ascent: 0.0,
            descent: 0.0,
            pixel_width: 0.0,
            glyph_row: None,
            line_wrap: LineWrap::WrapAtWhitespace,
            multibyte_p: true,
            mode_line_p: false,
            bidi_p: false,
            paragraph_embedding: BidiDir::Ltr,
            bidi_it: BidiIt::default(),
        }
    }

    /// Reset the iterator's per-row geometry (current_x, ascent,
    /// descent, pixel_width) at the start of a new row. GNU:
    /// this is normally handled by `prepare_desired_row` +
    /// `reseat_at_next_visible_line_start` at the start of
    /// `display_line`.
    pub fn reset_row_geometry(&mut self) {
        self.current_x = 0.0;
        self.ascent = 0.0;
        self.descent = 0.0;
        self.pixel_width = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "display_iterator_test.rs"]
mod tests;
