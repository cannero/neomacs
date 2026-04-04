# Display Glyph Matrix — Phase 1: Define GNU Glyph Types

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce GNU Emacs-compatible glyph matrix data types (`Glyph`, `GlyphRow`, `GlyphMatrix`) and the `RedisplayInterface` trait into the `neomacs-display-protocol` crate, with full test coverage.

**Architecture:** New types live in `neomacs-display-protocol` alongside the existing `FrameGlyphBuffer`. Nothing is removed or modified yet — this is pure addition. The types follow GNU Emacs's `dispextern.h` model but use Rust idioms (enums instead of C unions/bitfields). Tests verify row hashing, matrix construction, and the trait interface.

**Tech Stack:** Rust, `neomacs-display-protocol` crate. No new dependencies.

**Testing:** Use `cargo nextest run -p neomacs-display-protocol`. Always redirect output to a file first: `cargo nextest run ... 2>&1 > /tmp/test-output.log`. Tests go in separate `*_test.rs` files.

---

### Task 1: Create glyph type definitions

**Files:**
- Create: `neomacs-display-protocol/src/glyph_matrix.rs`
- Modify: `neomacs-display-protocol/src/lib.rs` (add module declaration)

- [ ] **Step 1: Add module declaration to lib.rs**

In `neomacs-display-protocol/src/lib.rs`, add the module and re-export after the existing modules:

```rust
pub mod glyph_matrix;
pub use glyph_matrix::*;
```

- [ ] **Step 2: Create glyph_matrix.rs with core enums and Glyph struct**

Create `neomacs-display-protocol/src/glyph_matrix.rs`:

```rust
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
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p neomacs-display-protocol 2>&1 > /tmp/check-output.log && echo OK`

Expected: `OK`

- [ ] **Step 4: Commit**

```bash
git add neomacs-display-protocol/src/glyph_matrix.rs neomacs-display-protocol/src/lib.rs
git commit -m "feat(display): add Glyph and GlyphType definitions for GNU glyph matrix model"
```

---

### Task 2: Add GlyphRow with row hashing

**Files:**
- Modify: `neomacs-display-protocol/src/glyph_matrix.rs`
- Create: `neomacs-display-protocol/src/glyph_matrix_test.rs`

- [ ] **Step 1: Write failing tests for GlyphRow and row hashing**

Create `neomacs-display-protocol/src/glyph_matrix_test.rs`:

```rust
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
```

- [ ] **Step 2: Add test module declaration to glyph_matrix.rs**

At the bottom of `neomacs-display-protocol/src/glyph_matrix.rs`, add:

```rust
#[cfg(test)]
#[path = "glyph_matrix_test.rs"]
mod tests;
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo nextest run -p neomacs-display-protocol glyph_matrix 2>&1 > /tmp/test-output.log; grep -E "FAIL|PASS|error" /tmp/test-output.log`

Expected: Compilation errors — `GlyphRow` not defined yet.

- [ ] **Step 4: Implement GlyphRow with hashing**

Add to `neomacs-display-protocol/src/glyph_matrix.rs`, after the `Glyph` impl block:

```rust
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
    /// Create a new empty row with the given role.
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

    /// Compute the hash of this row's glyph content.
    /// Uses FNV-1a for speed — this is not cryptographic, just for diffing.
    pub fn compute_hash(&self) -> u64 {
        // FNV-1a constants for 64-bit
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;

        let mut hash = FNV_OFFSET;
        for area in &self.glyphs {
            for glyph in area {
                // Hash the character/type
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

                // Hash the face
                hash ^= glyph.face_id as u64;
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        }
        hash
    }

    /// Check if two rows have equal glyph content.
    /// Uses hash as a fast rejection filter, then falls back to
    /// cell-by-cell comparison if hashes match.
    pub fn row_equal(&self, other: &GlyphRow) -> bool {
        // Fast path: different hashes → definitely different
        if self.hash != 0 && other.hash != 0 && self.hash != other.hash {
            return false;
        }
        // Slow path: compare cell by cell
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

    /// Number of glyphs in a specific area.
    pub fn used(&self, area: GlyphArea) -> usize {
        self.glyphs[area as usize].len()
    }

    /// Total glyphs across all areas.
    pub fn total_glyphs(&self) -> usize {
        self.glyphs[0].len() + self.glyphs[1].len() + self.glyphs[2].len()
    }

    /// Clear all glyphs and reset state for reuse.
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p neomacs-display-protocol glyph_matrix 2>&1 > /tmp/test-output.log; grep -E "FAIL|PASS|ok" /tmp/test-output.log`

Expected: All 7 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add neomacs-display-protocol/src/glyph_matrix.rs neomacs-display-protocol/src/glyph_matrix_test.rs
git commit -m "feat(display): add GlyphRow with FNV-1a row hashing for matrix diffing"
```

---

### Task 3: Add GlyphMatrix

**Files:**
- Modify: `neomacs-display-protocol/src/glyph_matrix.rs`
- Modify: `neomacs-display-protocol/src/glyph_matrix_test.rs`

- [ ] **Step 1: Write failing tests for GlyphMatrix**

Append to `neomacs-display-protocol/src/glyph_matrix_test.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p neomacs-display-protocol glyph_matrix 2>&1 > /tmp/test-output.log; grep -E "FAIL|error" /tmp/test-output.log`

Expected: Compilation errors — `GlyphMatrix` not defined yet.

- [ ] **Step 3: Implement GlyphMatrix**

Add to `neomacs-display-protocol/src/glyph_matrix.rs`, after the `GlyphRow` impl block:

```rust
/// Per-window 2D character grid. Equivalent to GNU's `struct glyph_matrix`.
///
/// Each window has a `desired_matrix` (freshly computed by layout) and
/// a `current_matrix` (what was last output to the display).
/// TTY diffs these; GPU ignores `current_matrix`.
#[derive(Clone, Debug)]
pub struct GlyphMatrix {
    /// The rows of this matrix.
    pub rows: Vec<GlyphRow>,
    /// Number of rows used.
    pub nrows: usize,
    /// Number of columns (window width in character cells).
    pub ncols: usize,
    /// Window origin in frame grid coordinates.
    pub matrix_x: usize,
    pub matrix_y: usize,
    /// Has header line.
    pub header_line: bool,
    /// Has tab line.
    pub tab_line: bool,
}

impl GlyphMatrix {
    /// Create a new matrix with the given dimensions.
    /// All rows are initialized as enabled text rows.
    pub fn new(nrows: usize, ncols: usize) -> Self {
        let rows = (0..nrows)
            .map(|_| GlyphRow::new(GlyphRowRole::Text))
            .collect();
        Self {
            rows,
            nrows,
            ncols,
            matrix_x: 0,
            matrix_y: 0,
            header_line: false,
            tab_line: false,
        }
    }

    /// Clear all row content without changing dimensions.
    pub fn clear(&mut self) {
        for row in &mut self.rows {
            row.clear();
        }
    }

    /// Resize the matrix. New rows are empty text rows.
    /// Existing rows beyond the new size are dropped.
    pub fn resize(&mut self, nrows: usize, ncols: usize) {
        self.rows.resize_with(nrows, || GlyphRow::new(GlyphRowRole::Text));
        self.rows.truncate(nrows);
        self.nrows = nrows;
        self.ncols = ncols;
    }

    /// Compute hashes for all rows that don't have one yet (hash == 0).
    pub fn ensure_hashes(&mut self) {
        for row in &mut self.rows {
            if row.hash == 0 && row.total_glyphs() > 0 {
                row.hash = row.compute_hash();
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p neomacs-display-protocol glyph_matrix 2>&1 > /tmp/test-output.log; grep -E "FAIL|PASS|ok" /tmp/test-output.log`

Expected: All 11 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add neomacs-display-protocol/src/glyph_matrix.rs neomacs-display-protocol/src/glyph_matrix_test.rs
git commit -m "feat(display): add GlyphMatrix with resize, clear, and hash computation"
```

---

### Task 4: Add RedisplayInterface trait and FrameDisplayState

**Files:**
- Modify: `neomacs-display-protocol/src/glyph_matrix.rs`
- Modify: `neomacs-display-protocol/src/glyph_matrix_test.rs`

- [ ] **Step 1: Write failing test for FrameDisplayState construction**

Append to `neomacs-display-protocol/src/glyph_matrix_test.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p neomacs-display-protocol glyph_matrix 2>&1 > /tmp/test-output.log; grep -E "FAIL|error" /tmp/test-output.log`

Expected: Compilation errors.

- [ ] **Step 3: Implement FrameDisplayState, WindowMatrixEntry, and RedisplayInterface**

Add to `neomacs-display-protocol/src/glyph_matrix.rs`:

```rust
/// Entry pairing a window's glyph matrix with its pixel bounds.
/// The render thread uses `pixel_bounds` to convert grid→pixel positions.
#[derive(Clone, Debug)]
pub struct WindowMatrixEntry {
    /// Window identifier.
    pub window_id: u64,
    /// The glyph matrix for this window.
    pub matrix: GlyphMatrix,
    /// Window bounds in frame pixel coordinates.
    /// Used by the GPU render thread for grid→pixel conversion.
    pub pixel_bounds: Rect,
}

/// Everything the render thread (or TTY backend) needs for one frame.
/// Replaces `FrameGlyphBuffer` as the data sent across the channel.
#[derive(Clone, Debug)]
pub struct FrameDisplayState {
    /// Per-window glyph matrices.
    pub window_matrices: Vec<WindowMatrixEntry>,
    /// Frame dimensions in character cells.
    pub frame_cols: usize,
    pub frame_rows: usize,
    /// Frame dimensions in pixels.
    pub frame_pixel_width: f32,
    pub frame_pixel_height: f32,
    /// Default character cell dimensions in pixels.
    pub char_width: f32,
    pub char_height: f32,
    /// Default font pixel size.
    pub font_pixel_size: f32,
    /// Frame background color.
    pub background: Color,
    /// Face table snapshot — maps face_id → resolved face attributes.
    pub faces: HashMap<u32, Face>,
    /// Child frame identity.
    pub frame_id: u64,
    pub parent_id: u64,
    pub parent_x: f32,
    pub parent_y: f32,
    pub z_order: i32,
    /// Window metadata for animation/transition detection.
    pub window_infos: Vec<WindowInfo>,
    /// Transition hints for the renderer.
    pub transition_hints: Vec<WindowTransitionHint>,
}

impl FrameDisplayState {
    /// Create a new empty frame display state.
    pub fn new(frame_cols: usize, frame_rows: usize, char_width: f32, char_height: f32) -> Self {
        Self {
            window_matrices: Vec::new(),
            frame_cols,
            frame_rows,
            frame_pixel_width: frame_cols as f32 * char_width,
            frame_pixel_height: frame_rows as f32 * char_height,
            char_width,
            char_height,
            font_pixel_size: char_height,
            background: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            faces: HashMap::new(),
            frame_id: 0,
            parent_id: 0,
            parent_x: 0.0,
            parent_y: 0.0,
            z_order: 0,
            window_infos: Vec::new(),
            transition_hints: Vec::new(),
        }
    }
}

/// Scroll region description for optimized terminal scrolling.
#[derive(Clone, Debug)]
pub struct ScrollRun {
    /// Window this scroll applies to.
    pub window_id: u64,
    /// First row of the scroll region.
    pub first_row: usize,
    /// Last row (exclusive) of the scroll region.
    pub last_row: usize,
    /// Number of rows to scroll (positive = up, negative = down).
    pub distance: i32,
}

/// Backend-specific display operations.
/// Matches GNU's `struct redisplay_interface` (the `rif` vtable).
///
/// TTY implements this with ANSI escape sequences on the evaluator thread.
/// GPU implements this by building FrameDisplayState and sending to the render thread.
pub trait RedisplayInterface {
    /// Called before updating a window.
    fn update_window_begin(&mut self, window_id: u64);
    /// Write glyphs from a row area to the display.
    fn write_glyphs(&mut self, row: &GlyphRow, area: GlyphArea, start: usize, len: usize);
    /// Clear from position to end of line.
    fn clear_end_of_line(&mut self, row: &GlyphRow, area: GlyphArea);
    /// Scroll a region of the display.
    fn scroll_run(&mut self, run: &ScrollRun);
    /// Called after updating a window.
    fn update_window_end(&mut self, window_id: u64);
    /// Set cursor position and style.
    fn set_cursor(&mut self, row: u16, col: u16, style: CursorStyle);
    /// Flush pending output to the display.
    fn flush(&mut self);
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p neomacs-display-protocol glyph_matrix 2>&1 > /tmp/test-output.log; grep -E "FAIL|PASS|ok" /tmp/test-output.log`

Expected: All 13 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add neomacs-display-protocol/src/glyph_matrix.rs neomacs-display-protocol/src/glyph_matrix_test.rs
git commit -m "feat(display): add FrameDisplayState, RedisplayInterface trait, and ScrollRun"
```

---

### Task 5: Verify full crate compiles and all tests pass

**Files:** None modified — verification only.

- [ ] **Step 1: Run full crate check**

Run: `cargo check -p neomacs-display-protocol 2>&1 > /tmp/check-output.log && echo OK`

Expected: `OK`

- [ ] **Step 2: Run all tests in the crate**

Run: `cargo nextest run -p neomacs-display-protocol 2>&1 > /tmp/test-output.log; grep -c "PASS" /tmp/test-output.log`

Expected: 13 tests pass (or more if the crate had pre-existing tests).

- [ ] **Step 3: Run workspace check to ensure no breakage**

Run: `cargo check --workspace 2>&1 > /tmp/workspace-check.log; tail -5 /tmp/workspace-check.log`

Expected: No errors. The new types are additive — nothing in the workspace depends on them yet.

- [ ] **Step 4: Commit (if any formatting fixes needed)**

```bash
cargo fmt -p neomacs-display-protocol
git add -A
git diff --cached --stat
# Only commit if there are changes
git diff --cached --quiet || git commit -m "style: format glyph matrix module"
```
