# Display Pipeline Refactor: GNU Glyph Matrix Model

## Problem

Neomacs's layout engine outputs `FrameGlyphBuffer` — a flat vector of pixel-positioned
`FrameGlyph` enums with `f32` coordinates. This is GPU-native but wrong for TTY:

1. The TTY backend reverse-converts pixels to grid: `col = (x / char_width) as usize`.
   This is wasteful and lossy.
2. GNU Emacs's battle-tested terminal code (`term.c`, `dispnew.c`) cannot be
   mechanically translated because neomacs uses a different shared representation.
3. The layout engine internally computes on a character grid but only exposes pixel
   floats, forcing both backends through the same GPU-shaped pipe.

## Principle

Match GNU Emacs's display architecture:
- Code GNU implements in C, we translate to Rust.
- The shared representation is a character grid (glyph matrix), not pixel floats.
- TTY and GUI diverge at the `RedisplayInterface` trait boundary.

## Architecture

```
evaluator thread                              render thread
────────────────                              ─────────────

read_char() → execute command
        │
   ┌────▼─────────────────────┐
   │  layout engine            │
   │  (xdisp.c equivalent)    │
   │                           │
   │  For each window:         │
   │  ├─ read buffer text      │
   │  ├─ resolve faces         │
   │  ├─ fontification         │
   │  └─ fill desired_matrix   │
   │     (integer row/col,     │
   │      char + face_id)      │
   └────┬─────────────────────┘
        │
   update_frame() via rif trait
        │
   ┌────┴──────────────────┐
   ▼                        ▼
 TtyRif                   GpuRif
 (same thread)            (sends to render thread)
   │                        │
   │ diff desired           │ send desired_matrix ──→  render thread:
   │ vs current             │ (full frame,              ├─ convert grid
   │ matrix                 │  no diff needed)          │  to pixel coords
   │                        │                           ├─ font shaping
   │ write changed          │                           ├─ glyph atlas
   │ cells as ANSI          │                           ├─ wgpu render
   │ sequences              │                           └─ present
   │ to stdout              │
   │                        │
   │ swap current =         │
   │ desired                │
   └────────────────────────┘
```

### Threading model

| Mode | Threads | Rationale |
|------|---------|-----------|
| TTY (`neomacs -nw`) | 1 thread | Matches GNU. Writing escape sequences to stdout is fast. No benefit from a second thread. Direct translation of `term.c`. |
| GUI (default) | 2 threads | Evaluator thread does layout + fills matrix. Render thread converts grid→pixels + wgpu. Evaluator doesn't block on VSync. |

### What moves where (GUI path)

Today the evaluator thread does grid→pixel conversion inside the layout engine, then
sends pixel data to the render thread. After this refactor:

- **Evaluator thread**: layout engine fills `GlyphMatrix` (grid coordinates only).
  No pixel math. Returns to `read_char()` faster.
- **Render thread**: receives `GlyphMatrix`, converts `row * char_height` / `col * char_width`
  to pixel positions, does font shaping, renders via wgpu. Same as today but the
  pixel conversion moves here.

The channel, cloning pattern, wgpu shaders, glyph atlas, and input handling stay the same.

## Core Data Types

Translated from GNU's `dispextern.h`. We translate the architecture, not every
bitfield — Rust enums and structs replace C unions and bitfields.

### GlyphType

```rust
/// What kind of content this glyph represents.
/// Matches GNU's `enum glyph_type`.
enum GlyphType {
    /// Regular character (including multibyte).
    Char { ch: char },
    /// Composed grapheme cluster (ligatures, emoji ZWJ, combining marks).
    Composite { text: Box<str> },
    /// Whitespace/filler with background color.
    Stretch { width_cols: u16 },
    /// Inline image.
    Image { image_id: i32 },
    /// Character with no available glyph (rendered as hex code or empty box).
    Glyphless { ch: char, display: GlyphlessDisplay },
}
```

### Glyph

```rust
/// One character cell. Equivalent to GNU's `struct glyph`.
///
/// Grid-native: no pixel coordinates. The row index in GlyphRow and the
/// column position within the row's glyph vector determine screen position.
struct Glyph {
    glyph_type: GlyphType,
    face_id: u32,
    /// Buffer position this glyph maps to. Used for cursor placement
    /// and mouse click → buffer position mapping.
    charpos: usize,
    /// Bidirectional resolved level (0 = LTR, 1 = RTL, etc.)
    bidi_level: u8,
    /// True for double-width characters (CJK, etc.)
    wide: bool,
    /// Padding glyph (second cell of a wide character).
    padding: bool,
}
```

### GlyphRow

```rust
/// Three areas of a glyph row, matching GNU's layout.
enum GlyphArea {
    LeftMargin = 0,
    Text = 1,
    RightMargin = 2,
}

/// One screen row. Equivalent to GNU's `struct glyph_row`.
struct GlyphRow {
    /// Glyphs per area: [left_margin, text, right_margin].
    glyphs: [Vec<Glyph>; 3],
    /// Row hash for fast diff. Computed lazily, 0 = not yet computed.
    hash: u64,
    /// Row is valid and should be displayed.
    enabled: bool,
    /// Semantic role: text body, mode-line, header-line, tab-line, etc.
    role: GlyphRowRole,
    /// Cursor column in this row, if cursor is here.
    cursor_col: Option<u16>,
    /// Cursor type (filled box, bar, hbar, hollow).
    cursor_type: Option<CursorStyle>,
    /// This row has been truncated on the left.
    truncated_left: bool,
    /// This row has a continuation mark on the right.
    continued: bool,
    /// Row displays actual buffer text (not a blank filler line).
    displays_text: bool,
    /// Row ends at end of buffer.
    ends_at_zv: bool,
    /// This is a mode-line, header-line, or tab-line row.
    mode_line: bool,
    /// Buffer position range covered by this row.
    start_charpos: usize,
    end_charpos: usize,
}
```

### GlyphMatrix

```rust
/// Per-window 2D character grid. Equivalent to GNU's `struct glyph_matrix`.
///
/// Each window has a `desired_matrix` (freshly computed by layout) and
/// a `current_matrix` (what was last output to the display).
/// The TTY backend diffs these; the GPU backend ignores `current_matrix`.
struct GlyphMatrix {
    rows: Vec<GlyphRow>,
    /// Number of rows actually used.
    nrows: usize,
    /// Number of columns (window width in character cells).
    ncols: usize,
    /// Window origin in frame grid coordinates.
    matrix_x: usize,
    matrix_y: usize,
    /// Has header line.
    header_line: bool,
    /// Has tab line.
    tab_line: bool,
}
```

### FrameDisplayState

The `GlyphMatrix` is the shared representation but the render thread also needs
metadata that today lives in `FrameGlyphBuffer`. This struct carries it alongside
the matrices.

```rust
/// Everything the render thread needs for one frame, replacing FrameGlyphBuffer.
struct FrameDisplayState {
    /// Window matrices (one per visible window).
    window_matrices: Vec<WindowMatrixEntry>,
    /// Frame dimensions in character cells and pixels.
    frame_cols: usize,
    frame_rows: usize,
    frame_pixel_width: f32,
    frame_pixel_height: f32,
    /// Default character cell dimensions in pixels.
    char_width: f32,
    char_height: f32,
    font_pixel_size: f32,
    /// Frame background color.
    background: Color,
    /// Face table snapshot — maps face_id → resolved face attributes.
    faces: HashMap<u32, Face>,
    /// Cursor inverse video info.
    cursor_inverse: Option<CursorInverseInfo>,
    /// Child frame identity (for multi-window/floating frames).
    frame_id: u64,
    parent_id: u64,
    parent_x: f32,
    parent_y: f32,
    z_order: i32,
    /// Window metadata for animation/transition detection.
    window_infos: Vec<WindowInfo>,
    /// Transition and effect hints for the renderer.
    transition_hints: Vec<WindowTransitionHint>,
    effect_hints: Vec<WindowEffectHint>,
    /// Stipple patterns for background fills.
    stipple_patterns: HashMap<i32, StipplePattern>,
}

struct WindowMatrixEntry {
    window_id: u64,
    matrix: GlyphMatrix,
    /// Window bounds in frame pixel coordinates (for render thread pixel conversion).
    pixel_bounds: Rect,
}
```

## The RedisplayInterface Trait

Equivalent to GNU's `struct redisplay_interface`. This is where TTY and GUI diverge.

```rust
/// Backend-specific display operations.
/// Matches GNU's `struct redisplay_interface` (the `rif` vtable).
trait RedisplayInterface {
    /// Called before updating a window.
    fn update_window_begin(&mut self, window_id: u64);
    /// Write glyphs from a row area to the display.
    fn write_glyphs(&mut self, row: &GlyphRow, area: GlyphArea, start: usize, len: usize);
    /// Clear from position to end of line.
    fn clear_end_of_line(&mut self, row: &GlyphRow, area: GlyphArea);
    /// Scroll a region of the display.
    fn scroll_run(&mut self, window_id: u64, run: &ScrollRun);
    /// Called after updating a window.
    fn update_window_end(&mut self, window_id: u64);
    /// Set cursor position.
    fn set_cursor(&mut self, row: u16, col: u16, style: CursorStyle);
    /// Flush pending output.
    fn flush(&mut self);
}
```

### TtyRif

Translated from GNU's `term.c`. Runs on the evaluator thread.

- `write_glyphs`: emit ANSI escape sequences for character attributes + character bytes.
- `clear_end_of_line`: emit terminfo `el` (clear to end of line).
- `scroll_run`: use terminal scroll regions (`csr` + `ri`/`ind`) when available.
- `set_cursor`: compute optimal cursor movement (absolute `cup` vs relative sequences).
- `flush`: write buffered output to stdout.

Maintains `current_matrix` per window. Before calling `write_glyphs`, diffs
`desired_matrix` rows against `current_matrix` rows using row hash, then
cell-by-cell for changed rows. After output, swaps `current = desired`.

Terminal capabilities detected via terminfo (Rust `terminfo` crate or manual
escape sequence detection for modern terminals).

### GpuRif

Runs on the evaluator thread but sends data to the render thread.

- `update_window_begin` / `update_window_end`: no-op (GPU redraws everything).
- `write_glyphs`, `clear_end_of_line`, `scroll_run`: no-op (GPU doesn't do incremental updates).
- `flush`: build `FrameDisplayState` from all window `desired_matrix` data,
  clone and send via crossbeam channel to the render thread.

The GPU backend does NOT diff. It does NOT maintain `current_matrix`.
It sends the full frame every redisplay, same as today.

## What Gets Translated from GNU C → Rust

| GNU C file | Relevant lines | Rust file | Purpose |
|---|---|---|---|
| `dispextern.h` (types) | ~500 | `glyph_types.rs` | `Glyph`, `GlyphRow`, `GlyphMatrix`, `RedisplayInterface` |
| `dispnew.c` (matrix ops) | ~2000 | `dispnew.rs` | Row hashing, `row_equal_p`, `make_current`, matrix alloc/clear |
| `term.c` | ~5000 | `term.rs` | TTY output, cursor optimization, color, scroll regions, capabilities |

## What Stays as Rust-Native

| Component | Rationale |
|---|---|
| Layout engine (`engine.rs`) | Already works. Output changes from `FrameGlyphBuffer` to `GlyphMatrix`. |
| GPU render thread | Already works. Input changes from `FrameGlyphBuffer` to `FrameDisplayState`. |
| Font shaping (cosmic-text) | Pure Rust, stays on render thread. |
| wgpu renderer | No GNU equivalent. |
| Glyph atlas | No GNU equivalent. |
| Input handling | Already works in both modes. |

## What Gets Removed

| Component | Replaced by |
|---|---|
| `FrameGlyphBuffer` | `FrameDisplayState` (carrying `GlyphMatrix` per window) |
| `FrameGlyph` enum (pixel-float variants) | `Glyph` struct (grid-native) |
| `rasterize_frame_glyphs()` in TTY backend | TTY reads `GlyphMatrix` directly |
| `TtyGrid` / `diff_grids()` | GNU-style `desired_matrix` / `current_matrix` diff in `TtyRif` |
| `DisplayBackend` trait | `RedisplayInterface` trait |

## Phased Implementation

### Phase 1: Define GNU glyph types + RedisplayInterface trait

Add new files. No existing code changes. Nothing breaks.

- `neovm-core/src/display/glyph_types.rs` — `Glyph`, `GlyphRow`, `GlyphMatrix`, `FrameDisplayState`
- `neovm-core/src/display/mod.rs` — `RedisplayInterface` trait
- Add `desired_matrix` / `current_matrix` fields to window structs
- Unit tests for row hashing, matrix alloc/clear

### Phase 2: Layout engine outputs GlyphMatrix

Refactor `engine.rs` to fill `desired_matrix` instead of `FrameGlyphBuffer`.

- The layout engine currently computes `x = col * char_width` (pixel float) for every
  glyph. Remove this — just store `col` as an integer in the glyph's position within
  the row's `Vec<Glyph>`.
- Face resolution stays the same — `face_id` goes into `Glyph.face_id`.
- Cursor placement stores row index + column in `GlyphRow.cursor_col`.
- `GpuRif.flush()` builds `FrameDisplayState` from window matrices and sends via channel.
- Render thread receives `FrameDisplayState`, converts to pixel positions, renders.
- GPU path keeps working throughout — just reads different input format.

### Phase 3: Translate `term.c` → `term.rs`

Implement `TtyRif`:

- Terminal capability detection (terminfo or modern ANSI detection).
- `write_glyphs`: ANSI SGR sequences + encoded character bytes.
- `clear_end_of_line`: `\x1b[K` or terminfo `el`.
- `scroll_run`: scroll regions when supported.
- `set_cursor`: optimal cursor movement.
- Matrix diffing: row hash comparison → cell-by-cell for changed rows.
- Single-thread TTY mode working end-to-end.

### Phase 4: Cleanup

- Remove `FrameGlyphBuffer`, `FrameGlyph`, `DisplayBackend`, `TtyGrid`.
- Remove `rasterize_frame_glyphs()`.
- Remove pixel-float coordinate computation from layout engine.
- Update all tests.

## Success Criteria

1. GUI mode renders identically to current behavior — pixel-perfect regression test via
   screenshot comparison.
2. TTY mode (`neomacs -nw`) works in standard terminals (xterm, alacritty, kitty,
   tmux, screen) over SSH.
3. GNU Emacs's `term.c` terminal capability coverage: 8-color, 256-color, 24-bit
   truecolor, cursor styles, scroll regions.
4. No performance regression in GUI mode — evaluator thread should be faster
   (less pixel math work).
5. Each phase compiles and passes tests independently.
