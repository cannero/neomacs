# Display Glyph Matrix — Phase 2a: Parallel GlyphMatrix Builder

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `GlyphMatrixBuilder` that records text content into `GlyphMatrix` alongside the existing `FrameGlyphBuffer` output, without changing any downstream consumers. Validate the matrix is consistent with the pixel output.

**Architecture:** The `GlyphMatrixBuilder` is a helper struct that lives on `LayoutEngine`. During `layout_frame_rust`, at key points where characters are emitted to `FrameGlyphBuffer`, we also record the character + face_id + charpos into the builder's `GlyphMatrix`. At the end of `layout_frame_rust`, we build a `FrameDisplayState` from the accumulated matrices and store it on the engine for inspection/testing. No downstream code changes — `FrameGlyphBuffer` is still the only output sent to the render thread.

**Tech Stack:** Rust. `neomacs-display-protocol` (GlyphMatrix types from Phase 1). `neomacs-layout-engine` (layout engine instrumentation).

**Testing:** Use `cargo nextest run -p neomacs-layout-engine`. Always redirect output to a file. Tests go in separate `*_test.rs` files.

**Scope boundary:** Only text-area characters are tracked in the matrix. Mode-line, header-line, tab-line, window dividers, backgrounds, and cursors are NOT in the matrix yet — those come in Phase 2b. The goal is to prove the row/col grid model captures the same text content as the pixel-based output.

---

## File Structure

| File | Action | Purpose |
|------|--------|---------|
| `neomacs-layout-engine/src/matrix_builder.rs` | Create | `GlyphMatrixBuilder` — accumulates glyphs into `GlyphMatrix` during layout |
| `neomacs-layout-engine/src/matrix_builder_test.rs` | Create | Tests for the builder |
| `neomacs-layout-engine/src/engine.rs` | Modify | Add builder field to `LayoutEngine`, instrument `layout_frame_rust` and `layout_window_rust` to feed the builder |
| `neomacs-layout-engine/src/lib.rs` | Modify | Add `mod matrix_builder` |

---

### Task 1: Create GlyphMatrixBuilder

**Files:**
- Create: `neomacs-layout-engine/src/matrix_builder.rs`
- Create: `neomacs-layout-engine/src/matrix_builder_test.rs`
- Modify: `neomacs-layout-engine/src/lib.rs`

The builder tracks which window/row we're on and accumulates glyphs.

- [ ] **Step 1: Write failing tests**

Create `neomacs-layout-engine/src/matrix_builder_test.rs`:

```rust
use super::*;
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::frame_glyphs::GlyphRowRole;
use neomacs_display_protocol::types::Rect;

#[test]
fn builder_starts_empty() {
    let builder = GlyphMatrixBuilder::new();
    let state = builder.finish(80, 24, 8.0, 16.0);
    assert!(state.window_matrices.is_empty());
}

#[test]
fn builder_tracks_single_window_single_row() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 24, 80, Rect::new(0.0, 0.0, 640.0, 384.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('H', 0, 0);
    builder.push_char('i', 0, 1);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(80, 24, 8.0, 16.0);
    assert_eq!(state.window_matrices.len(), 1);
    let matrix = &state.window_matrices[0].matrix;
    assert_eq!(matrix.nrows, 24);
    assert_eq!(matrix.ncols, 80);
    assert_eq!(matrix.rows[0].used(GlyphArea::Text), 2);

    let g0 = &matrix.rows[0].glyphs[GlyphArea::Text as usize][0];
    assert_eq!(g0.glyph_type, GlyphType::Char { ch: 'H' });
    assert_eq!(g0.face_id, 0);
    assert_eq!(g0.charpos, 0);

    let g1 = &matrix.rows[0].glyphs[GlyphArea::Text as usize][1];
    assert_eq!(g1.glyph_type, GlyphType::Char { ch: 'i' });
    assert_eq!(g1.charpos, 1);
}

#[test]
fn builder_tracks_multiple_rows() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 3, 10, Rect::new(0.0, 0.0, 80.0, 48.0));

    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('a', 0, 0);
    builder.end_row();

    builder.begin_row(1, GlyphRowRole::Text);
    builder.push_char('b', 0, 5);
    builder.push_char('c', 0, 6);
    builder.end_row();

    builder.end_window();

    let state = builder.finish(10, 3, 8.0, 16.0);
    let matrix = &state.window_matrices[0].matrix;
    assert_eq!(matrix.rows[0].used(GlyphArea::Text), 1);
    assert_eq!(matrix.rows[1].used(GlyphArea::Text), 2);
    assert_eq!(matrix.rows[2].used(GlyphArea::Text), 0); // row 2 untouched
}

#[test]
fn builder_tracks_wide_chars() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 5, 20, Rect::new(0.0, 0.0, 160.0, 80.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_wide_char('\u{4e16}', 0, 0); // CJK '世' — 2 cells
    builder.push_char('x', 0, 3);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(20, 5, 8.0, 16.0);
    let glyphs = &state.window_matrices[0].matrix.rows[0].glyphs[GlyphArea::Text as usize];
    // Wide char + padding + normal char = 3 glyphs
    assert_eq!(glyphs.len(), 3);
    assert!(glyphs[0].wide);
    assert!(!glyphs[0].padding);
    assert!(glyphs[1].padding);
    assert!(!glyphs[2].wide);
    assert!(!glyphs[2].padding);
}

#[test]
fn builder_handles_stretch_glyphs() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 5, 20, Rect::new(0.0, 0.0, 160.0, 80.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('a', 0, 0);
    builder.push_stretch(4, 0);
    builder.push_char('b', 0, 5);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(20, 5, 8.0, 16.0);
    let glyphs = &state.window_matrices[0].matrix.rows[0].glyphs[GlyphArea::Text as usize];
    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs[1].glyph_type, GlyphType::Stretch { width_cols: 4 });
}

#[test]
fn builder_computes_row_hashes_on_finish() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 2, 10, Rect::new(0.0, 0.0, 80.0, 32.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('x', 0, 0);
    builder.end_row();
    builder.end_window();

    let state = builder.finish(10, 2, 8.0, 16.0);
    let row = &state.window_matrices[0].matrix.rows[0];
    assert_ne!(row.hash, 0, "hash should be computed on finish");
}

#[test]
fn builder_resets_on_new_frame() {
    let mut builder = GlyphMatrixBuilder::new();
    builder.begin_window(1, 2, 10, Rect::new(0.0, 0.0, 80.0, 32.0));
    builder.begin_row(0, GlyphRowRole::Text);
    builder.push_char('x', 0, 0);
    builder.end_row();
    builder.end_window();

    builder.reset();

    let state = builder.finish(10, 2, 8.0, 16.0);
    assert!(state.window_matrices.is_empty());
}
```

- [ ] **Step 2: Add module declaration**

In `neomacs-layout-engine/src/lib.rs`, add:

```rust
pub mod matrix_builder;
```

- [ ] **Step 3: Create matrix_builder.rs with the builder implementation**

Create `neomacs-layout-engine/src/matrix_builder.rs`:

```rust
//! GlyphMatrixBuilder — records text content into GlyphMatrix during layout.
//!
//! This builder runs alongside the existing FrameGlyphBuffer output path.
//! It observes character emissions and records them into a GlyphMatrix grid.
//! The resulting FrameDisplayState can be compared against the pixel output
//! for validation, and will eventually replace FrameGlyphBuffer entirely.

use neomacs_display_protocol::frame_glyphs::GlyphRowRole;
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::types::Rect;

/// Accumulates glyphs into GlyphMatrix rows during layout.
///
/// Usage:
/// ```ignore
/// builder.reset();
/// for each window:
///     builder.begin_window(id, rows, cols, bounds);
///     for each row:
///         builder.begin_row(row_idx, role);
///         for each character:
///             builder.push_char(ch, face_id, charpos);
///         builder.end_row();
///     builder.end_window();
/// let state = builder.finish(frame_cols, frame_rows, char_w, char_h);
/// ```
pub struct GlyphMatrixBuilder {
    /// Accumulated window matrices.
    windows: Vec<WindowMatrixEntry>,
    /// Current window's matrix being built (None if not inside begin_window/end_window).
    current_matrix: Option<GlyphMatrix>,
    /// Current window id.
    current_window_id: u64,
    /// Current window pixel bounds.
    current_pixel_bounds: Rect,
    /// Current row index within the matrix.
    current_row: usize,
    /// Whether we're inside a begin_row/end_row pair.
    in_row: bool,
}

impl GlyphMatrixBuilder {
    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            current_matrix: None,
            current_window_id: 0,
            current_pixel_bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            current_row: 0,
            in_row: false,
        }
    }

    /// Reset for a new frame.
    pub fn reset(&mut self) {
        self.windows.clear();
        self.current_matrix = None;
        self.current_window_id = 0;
        self.current_row = 0;
        self.in_row = false;
    }

    /// Begin a new window's matrix.
    pub fn begin_window(&mut self, window_id: u64, nrows: usize, ncols: usize, pixel_bounds: Rect) {
        self.current_matrix = Some(GlyphMatrix::new(nrows, ncols));
        self.current_window_id = window_id;
        self.current_pixel_bounds = pixel_bounds;
        self.current_row = 0;
        self.in_row = false;
    }

    /// End the current window and save its matrix.
    pub fn end_window(&mut self) {
        if let Some(matrix) = self.current_matrix.take() {
            self.windows.push(WindowMatrixEntry {
                window_id: self.current_window_id,
                matrix,
                pixel_bounds: self.current_pixel_bounds,
            });
        }
    }

    /// Begin a row within the current window.
    pub fn begin_row(&mut self, row: usize, role: GlyphRowRole) {
        self.current_row = row;
        self.in_row = true;
        if let Some(ref mut matrix) = self.current_matrix {
            if row < matrix.rows.len() {
                matrix.rows[row].role = role;
                matrix.rows[row].enabled = true;
            }
        }
    }

    /// End the current row.
    pub fn end_row(&mut self) {
        self.in_row = false;
    }

    /// Push a regular character glyph into the current row's text area.
    pub fn push_char(&mut self, ch: char, face_id: u32, charpos: usize) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                matrix.rows[self.current_row].glyphs[GlyphArea::Text as usize]
                    .push(Glyph::char(ch, face_id, charpos));
                matrix.rows[self.current_row].displays_text = true;
            }
        }
    }

    /// Push a wide (double-width) character glyph + its padding cell.
    pub fn push_wide_char(&mut self, ch: char, face_id: u32, charpos: usize) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                let row = &mut matrix.rows[self.current_row];
                let area = &mut row.glyphs[GlyphArea::Text as usize];
                let mut glyph = Glyph::char(ch, face_id, charpos);
                glyph.wide = true;
                area.push(glyph);
                area.push(Glyph::padding_for(face_id, charpos));
                row.displays_text = true;
            }
        }
    }

    /// Push a stretch (whitespace) glyph.
    pub fn push_stretch(&mut self, width_cols: u16, face_id: u32) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                matrix.rows[self.current_row].glyphs[GlyphArea::Text as usize]
                    .push(Glyph::stretch(width_cols, face_id));
            }
        }
    }

    /// Push a composed (multi-codepoint) glyph.
    pub fn push_composed(&mut self, text: &str, face_id: u32, charpos: usize) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                let glyph = Glyph {
                    glyph_type: GlyphType::Composite {
                        text: text.into(),
                    },
                    face_id,
                    charpos,
                    bidi_level: 0,
                    wide: false,
                    padding: false,
                };
                matrix.rows[self.current_row].glyphs[GlyphArea::Text as usize]
                    .push(glyph);
                matrix.rows[self.current_row].displays_text = true;
            }
        }
    }

    /// Set cursor position in the current row.
    pub fn set_cursor(&mut self, col: u16, style: neomacs_display_protocol::frame_glyphs::CursorStyle) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                matrix.rows[self.current_row].cursor_col = Some(col);
                matrix.rows[self.current_row].cursor_type = Some(style);
            }
        }
    }

    /// Set the charpos range for the current row.
    pub fn set_row_charpos(&mut self, start: usize, end: usize) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                matrix.rows[self.current_row].start_charpos = start;
                matrix.rows[self.current_row].end_charpos = end;
            }
        }
    }

    /// Finalize all matrices, compute hashes, and build FrameDisplayState.
    pub fn finish(
        mut self,
        frame_cols: usize,
        frame_rows: usize,
        char_width: f32,
        char_height: f32,
    ) -> FrameDisplayState {
        // Compute row hashes for all windows
        for entry in &mut self.windows {
            entry.matrix.ensure_hashes();
        }

        let mut state = FrameDisplayState::new(frame_cols, frame_rows, char_width, char_height);
        state.window_matrices = self.windows;
        state
    }
}

#[cfg(test)]
#[path = "matrix_builder_test.rs"]
mod tests;
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p neomacs-layout-engine matrix_builder 2>&1 > /tmp/test-output.log; tail -20 /tmp/test-output.log`

Expected: 7 tests pass.

- [ ] **Step 5: Commit**

```bash
git add neomacs-layout-engine/src/matrix_builder.rs neomacs-layout-engine/src/matrix_builder_test.rs neomacs-layout-engine/src/lib.rs
git commit -m "feat(layout): add GlyphMatrixBuilder for parallel grid output during layout"
```

---

### Task 2: Add GlyphMatrixBuilder to LayoutEngine struct

**Files:**
- Modify: `neomacs-layout-engine/src/engine.rs`

Add the builder as a field on `LayoutEngine` and reset it at the start of each frame.

- [ ] **Step 1: Add field to LayoutEngine**

In `neomacs-layout-engine/src/engine.rs`, add to the `LayoutEngine` struct (after `prev_background` field, around line 1068):

```rust
    /// Parallel GlyphMatrix builder — records text content alongside FrameGlyphBuffer.
    pub matrix_builder: crate::matrix_builder::GlyphMatrixBuilder,
```

- [ ] **Step 2: Initialize in LayoutEngine::new()**

In the `LayoutEngine::new()` function (around line 1073), add to the struct initializer:

```rust
            matrix_builder: crate::matrix_builder::GlyphMatrixBuilder::new(),
```

- [ ] **Step 3: Reset builder at start of layout_frame_rust()**

In `layout_frame_rust()`, right after `frame_glyphs.clear_all();` (line 1694), add:

```rust
        self.matrix_builder.reset();
```

- [ ] **Step 4: Build FrameDisplayState at end of layout_frame_rust()**

At the end of `layout_frame_rust()`, before the display_snapshots replacement (before line 1863), add:

```rust
        // Build parallel GlyphMatrix output for validation.
        // This will eventually replace FrameGlyphBuffer entirely.
        let frame_cols = (frame_params.width / frame_params.char_width.max(1.0)) as usize;
        let frame_rows = (frame_params.height / frame_params.char_height.max(1.0)) as usize;
        let matrix_builder = std::mem::replace(
            &mut self.matrix_builder,
            crate::matrix_builder::GlyphMatrixBuilder::new(),
        );
        let _frame_display_state = matrix_builder.finish(
            frame_cols,
            frame_rows,
            frame_params.char_width,
            frame_params.char_height,
        );
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p neomacs-layout-engine 2>&1 > /tmp/check-output.log && echo OK`

Expected: `OK`

- [ ] **Step 6: Commit**

```bash
git add neomacs-layout-engine/src/engine.rs
git commit -m "feat(layout): add GlyphMatrixBuilder field to LayoutEngine"
```

---

### Task 3: Instrument layout_window_rust to feed the builder

**Files:**
- Modify: `neomacs-layout-engine/src/engine.rs`

This is the key task. We add `begin_window`/`end_window`/`begin_row`/`end_row`/`push_char` calls at strategic points in the existing layout flow.

The layout_window_rust function is ~2000 lines. We instrument it at these points:
1. At the start: `begin_window`
2. At each row boundary (newline or wrap): `begin_row` / `end_row`
3. At each character emission: `push_char`
4. At the end: `end_window`

- [ ] **Step 1: Add begin_window at the start of layout_window_rust**

In `layout_window_rust()` (line 1878), after the buffer setup and before the main text loop begins, find where `text_y` and content area are established (around line 2231). Before the main loop (`while bytes_remaining > 0 ...`), add:

```rust
        // --- GlyphMatrix parallel builder ---
        let matrix_rows = max_rows.max(1) as usize;
        let matrix_cols = cols.max(1);
        self.matrix_builder.begin_window(
            params.window_id as u64,
            matrix_rows,
            matrix_cols,
            params.bounds,
        );
        self.matrix_builder.begin_row(0, GlyphRowRole::Text);
```

- [ ] **Step 2: Add push_char at the main character emission point**

Find where characters are added to `frame_glyphs` in the main text loop. The primary site is where `flush_run` calls `frame_glyphs.add_char()` (around lines 305-315 in `flush_run`), and where individual characters are added inline in `layout_window_rust`.

Since `flush_run` is a free function (not a method on LayoutEngine), the simplest approach is to instrument at the point where each decoded character is processed, BEFORE it enters the ligature buffer. This is in the main character processing loop of `layout_window_rust`.

Find the main character decode point where `ch` is obtained from the buffer text (around line 2488+). After the character is decoded and before face resolution, add the builder call. The exact location is where `ch` (the current character) and `charpos` (buffer position) are known.

Add after each character is decoded and determined to be visible (not invisible):

```rust
                // Feed parallel GlyphMatrix builder
                if !is_wide_char {
                    self.matrix_builder.push_char(ch, frame_glyphs.current_face_id, byte_pos);
                } else {
                    self.matrix_builder.push_wide_char(ch, frame_glyphs.current_face_id, byte_pos);
                }
```

NOTE: The exact insertion point depends on the local variable names in the layout loop. The implementer should:
1. Find where `ch: char` is decoded from buffer bytes
2. Find where `byte_pos` or equivalent charpos is tracked
3. Find where wide character detection happens (`unicode_width` or similar)
4. Insert the builder call after these but before the character enters the ligature buffer

If the character variable is named differently (e.g., `c`, `character`), adapt accordingly. If the charpos variable is `pos`, `buf_pos`, or `offset`, use that. Read the surrounding code to find the right names.

- [ ] **Step 3: Add row transitions at newline/wrap points**

Find where `row` is incremented (newline handling and word-wrap). At each such point, add:

```rust
                self.matrix_builder.end_row();
                self.matrix_builder.begin_row(row as usize, GlyphRowRole::Text);
```

There should be 2-3 such sites:
- Newline character processing (where `row += 1`)
- Word-wrap overflow (where `row += 1`)
- Possibly continuation/truncation

- [ ] **Step 4: Add end_window at the end of layout_window_rust**

Before the function returns (before the mode-line/header-line rendering), add:

```rust
        self.matrix_builder.end_row();
        self.matrix_builder.end_window();
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p neomacs-layout-engine 2>&1 > /tmp/check-output.log && echo OK`

Expected: `OK` (possibly with warnings about unused variables, which is fine)

- [ ] **Step 6: Verify workspace compilation**

Run: `cargo check --workspace 2>&1 > /tmp/workspace-check.log; tail -5 /tmp/workspace-check.log`

Expected: No errors.

- [ ] **Step 7: Commit**

```bash
git add neomacs-layout-engine/src/engine.rs
git commit -m "feat(layout): instrument layout_window_rust to feed GlyphMatrixBuilder"
```

---

### Task 4: Add validation logging

**Files:**
- Modify: `neomacs-layout-engine/src/engine.rs`

Add trace-level logging that compares the GlyphMatrix character count against the FrameGlyphBuffer character count per window, so we can validate correctness.

- [ ] **Step 1: Add validation at end of layout_frame_rust**

Replace the `let _frame_display_state = matrix_builder.finish(...)` from Task 2 with:

```rust
        let frame_display_state = matrix_builder.finish(
            frame_cols,
            frame_rows,
            frame_params.char_width,
            frame_params.char_height,
        );

        // Validate: count text characters in GlyphMatrix vs FrameGlyphBuffer
        let matrix_char_count: usize = frame_display_state
            .window_matrices
            .iter()
            .flat_map(|w| w.matrix.rows.iter())
            .flat_map(|r| r.glyphs[GlyphArea::Text as usize].iter())
            .filter(|g| matches!(g.glyph_type, GlyphType::Char { .. }) && !g.padding)
            .count();

        let buffer_char_count = frame_glyphs
            .glyphs
            .iter()
            .filter(|g| matches!(g, neomacs_display_protocol::frame_glyphs::FrameGlyph::Char { row_role, .. } if *row_role == GlyphRowRole::Text))
            .count();

        if matrix_char_count != buffer_char_count {
            tracing::debug!(
                "GlyphMatrix validation: matrix_chars={} vs buffer_chars={} (delta={})",
                matrix_char_count,
                buffer_char_count,
                (matrix_char_count as i64) - (buffer_char_count as i64),
            );
        }
```

Note: We use `tracing::debug!` (not `warn` or `error`) because mismatches are expected initially — the builder only captures text-area characters while FrameGlyphBuffer includes line numbers, fringe indicators, and other non-text chars. The count comparison is a rough sanity check, not an exact equality assertion.

- [ ] **Step 2: Add the necessary import**

At the top of `engine.rs`, ensure this import exists (it should already from Phase 1 deps):

```rust
use neomacs_display_protocol::glyph_matrix::{GlyphArea, GlyphType};
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p neomacs-layout-engine 2>&1 > /tmp/check-output.log && echo OK`

Expected: `OK`

- [ ] **Step 4: Commit**

```bash
git add neomacs-layout-engine/src/engine.rs
git commit -m "feat(layout): add GlyphMatrix vs FrameGlyphBuffer validation logging"
```

---

### Task 5: Verify full workspace builds and existing tests pass

**Files:** None modified — verification only.

- [ ] **Step 1: Check workspace compiles**

Run: `cargo check --workspace 2>&1 > /tmp/workspace-check.log; tail -5 /tmp/workspace-check.log`

Expected: No errors.

- [ ] **Step 2: Run layout engine tests**

Run: `cargo nextest run -p neomacs-layout-engine 2>&1 > /tmp/test-output.log; tail -20 /tmp/test-output.log`

Expected: All tests pass (both new matrix_builder tests and existing tests).

- [ ] **Step 3: Run display protocol tests**

Run: `cargo nextest run -p neomacs-display-protocol 2>&1 > /tmp/test-output.log; tail -20 /tmp/test-output.log`

Expected: All 13 glyph_matrix tests pass.

- [ ] **Step 4: Format and commit if needed**

```bash
cargo fmt -p neomacs-layout-engine
git diff --stat
git diff --quiet || (git add -A && git commit -m "style: format layout engine")
```
