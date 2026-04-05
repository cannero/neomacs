//! TTY rendering backend -- reads GlyphMatrix, outputs ANSI escape sequences.
//!
//! This implements a terminal display backend matching the approach of
//! GNU Emacs's term.c. It maintains two character grids (current and desired),
//! rasterizes `FrameDisplayState` into the desired grid, then diffs against
//! current to produce minimal ANSI output.
//!
//! Runs on the evaluator thread (single-threaded, no channel needed).

use crate::face::{Face, FaceAttributes, UnderlineStyle};
use crate::glyph_matrix::*;
use crate::types::Color;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Cell attributes
// ---------------------------------------------------------------------------

/// Attributes for a single terminal cell (maps to ANSI SGR sequences).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellAttrs {
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub bold: bool,
    pub italic: bool,
    /// 0=none, 1=single, 2=curly/wave, 3=double, 4=dotted, 5=dashed
    pub underline: u8,
    pub strikethrough: bool,
    pub inverse: bool,
}

impl Default for CellAttrs {
    fn default() -> Self {
        Self {
            fg: (255, 255, 255),
            bg: (0, 0, 0),
            bold: false,
            italic: false,
            underline: 0,
            strikethrough: false,
            inverse: false,
        }
    }
}

// ---------------------------------------------------------------------------
// TtyCell
// ---------------------------------------------------------------------------

/// A single cell in the terminal grid.
#[derive(Clone, Debug, PartialEq)]
pub struct TtyCell {
    pub ch: char,
    pub attrs: CellAttrs,
    /// True if this is a padding cell for a wide (double-width) character.
    pub padding: bool,
}

impl Default for TtyCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            attrs: CellAttrs::default(),
            padding: false,
        }
    }
}

// ---------------------------------------------------------------------------
// TtyGrid
// ---------------------------------------------------------------------------

/// Terminal character grid.
#[derive(Clone, Debug)]
pub struct TtyGrid {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<TtyCell>,
}

impl TtyGrid {
    pub fn new(width: usize, height: usize) -> Self {
        let cells = vec![TtyCell::default(); width * height];
        Self {
            width,
            height,
            cells,
        }
    }

    /// Clear all cells to spaces with the given background color.
    pub fn clear(&mut self, bg: (u8, u8, u8)) {
        let blank = TtyCell {
            ch: ' ',
            attrs: CellAttrs {
                bg,
                ..CellAttrs::default()
            },
            padding: false,
        };
        for cell in &mut self.cells {
            *cell = blank.clone();
        }
    }

    /// Set a cell at (row, col). No-op if out of bounds.
    pub fn set(&mut self, row: usize, col: usize, ch: char, attrs: CellAttrs, padding: bool) {
        if row < self.height && col < self.width {
            let idx = row * self.width + col;
            self.cells[idx] = TtyCell { ch, attrs, padding };
        }
    }

    /// Resize the grid, filling new cells with blanks.
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.cells.resize(width * height, TtyCell::default());
    }
}

// ---------------------------------------------------------------------------
// TtyRif
// ---------------------------------------------------------------------------

/// TTY Redisplay Interface implementation.
///
/// Usage pattern:
/// 1. `rasterize(&state)` -- convert FrameDisplayState into the desired grid
/// 2. `diff_and_render()` -- diff desired vs current, emit ANSI sequences
/// 3. `take_output()` -- get the buffered bytes to write to stdout
pub struct TtyRif {
    /// What is currently displayed on the terminal.
    current: TtyGrid,
    /// What we want to display.
    desired: TtyGrid,
    /// Buffered output bytes (ANSI sequences).
    output: Vec<u8>,
    /// Cursor row to set after rendering.
    cursor_row: u16,
    /// Cursor column to set after rendering.
    cursor_col: u16,
    /// Whether the cursor should be visible.
    cursor_visible: bool,
    /// Face lookup table (face_id -> Face).
    faces: HashMap<u32, Face>,
    /// Default background color (r, g, b).
    default_bg: (u8, u8, u8),
    /// Default foreground color (r, g, b).
    default_fg: (u8, u8, u8),
}

impl TtyRif {
    /// Create a new TtyRif for a terminal of the given dimensions.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            current: TtyGrid::new(width, height),
            desired: TtyGrid::new(width, height),
            output: Vec::with_capacity(4096),
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: false,
            faces: HashMap::new(),
            default_bg: (0, 0, 0),
            default_fg: (255, 255, 255),
        }
    }

    /// Resize the terminal grids. Clears both grids (forces full redraw).
    pub fn resize(&mut self, width: usize, height: usize) {
        self.current = TtyGrid::new(width, height);
        self.desired = TtyGrid::new(width, height);
    }

    /// Set the face table for resolving face_ids.
    pub fn set_faces(&mut self, faces: HashMap<u32, Face>) {
        self.faces = faces;
    }

    /// Width of the terminal grid.
    pub fn width(&self) -> usize {
        self.desired.width
    }

    /// Height of the terminal grid.
    pub fn height(&self) -> usize {
        self.desired.height
    }

    /// Rasterize a `FrameDisplayState` into the desired grid.
    ///
    /// Converts each window's `GlyphMatrix` rows into `TtyGrid` cells by
    /// iterating over glyph areas (left margin, text, right margin) and
    /// resolving face attributes.
    pub fn rasterize(&mut self, state: &FrameDisplayState) {
        self.faces = state.faces.clone();
        self.default_bg = color_to_rgb8(&state.background);
        self.desired.clear(self.default_bg);
        self.cursor_visible = false;

        for entry in &state.window_matrices {
            // Derive screen position from pixel_bounds.
            // In TTY mode, pixel_bounds uses char-cell units (char_w=1, char_h=1),
            // so bounds.x/y directly give the screen column/row.
            let char_w = state.char_width.max(1.0);
            let char_h = state.char_height.max(1.0);
            let win_col = (entry.pixel_bounds.x / char_w) as usize;
            let win_row = (entry.pixel_bounds.y / char_h) as usize;

            for (row_idx, glyph_row) in entry.matrix.rows.iter().enumerate() {
                if !glyph_row.enabled {
                    continue;
                }
                let screen_row = win_row + row_idx;
                if screen_row >= self.desired.height {
                    break;
                }

                let mut col = win_col;

                // Render all three glyph areas in order.
                for area_idx in 0..3 {
                    for glyph in &glyph_row.glyphs[area_idx] {
                        if col >= self.desired.width {
                            break;
                        }

                        if glyph.padding {
                            // Padding cell for wide character -- mark but don't advance.
                            let attrs = self.resolve_attrs(glyph.face_id);
                            self.desired.set(screen_row, col, ' ', attrs, true);
                            col += 1;
                            continue;
                        }

                        let attrs = self.resolve_attrs(glyph.face_id);
                        let ch = glyph_to_char(glyph);
                        self.desired.set(screen_row, col, ch, attrs, false);
                        col += 1;

                        // Wide character occupies two columns.
                        if glyph.wide && col < self.desired.width {
                            self.desired.set(screen_row, col, ' ', attrs, true);
                            col += 1;
                        }
                    }
                }

                // Handle cursor position.
                if let Some(cursor_col_in_row) = glyph_row.cursor_col {
                    self.cursor_row = screen_row as u16;
                    self.cursor_col = (win_col + cursor_col_in_row as usize) as u16;
                    self.cursor_visible = true;
                }
            }
        }
    }

    /// Resolve face_id into terminal cell attributes.
    fn resolve_attrs(&self, face_id: u32) -> CellAttrs {
        if let Some(face) = self.faces.get(&face_id) {
            CellAttrs {
                fg: color_to_rgb8(&face.foreground),
                bg: color_to_rgb8(&face.background),
                bold: face.is_bold(),
                italic: face.is_italic(),
                underline: match face.underline_style {
                    UnderlineStyle::None => 0,
                    UnderlineStyle::Line => 1,
                    UnderlineStyle::Wave => 2,
                    UnderlineStyle::Double => 3,
                    UnderlineStyle::Dotted => 4,
                    UnderlineStyle::Dashed => 5,
                },
                strikethrough: face.attributes.contains(FaceAttributes::STRIKE_THROUGH),
                inverse: face.attributes.contains(FaceAttributes::INVERSE),
            }
        } else {
            CellAttrs {
                fg: self.default_fg,
                bg: self.default_bg,
                ..CellAttrs::default()
            }
        }
    }

    /// Diff the desired grid against the current grid and generate ANSI escape
    /// sequences for the changed cells.
    ///
    /// After this call, `current` is swapped to reflect what is now on screen.
    /// Retrieve the buffered output with [`take_output`].
    pub fn diff_and_render(&mut self) {
        self.output.clear();

        // Hide cursor during update to avoid flicker.
        self.output.extend_from_slice(b"\x1b[?25l");

        let mut last_attrs: Option<CellAttrs> = None;

        for row in 0..self.desired.height {
            for col in 0..self.desired.width {
                let idx = row * self.desired.width + col;
                let desired = &self.desired.cells[idx];
                let current = &self.current.cells[idx];

                if desired == current {
                    continue;
                }

                // Skip padding cells -- they were drawn by the wide character.
                if desired.padding {
                    continue;
                }

                // Move cursor to (row, col). ANSI is 1-based.
                write_cursor_goto(&mut self.output, row as u16 + 1, col as u16 + 1);

                // Set attributes if changed from what we last emitted.
                if last_attrs.as_ref() != Some(&desired.attrs) {
                    write_sgr(&mut self.output, &desired.attrs);
                    last_attrs = Some(desired.attrs);
                }

                // Write the character.
                let mut buf = [0u8; 4];
                let s = desired.ch.encode_utf8(&mut buf);
                self.output.extend_from_slice(s.as_bytes());
            }
        }

        // Reset attributes after all updates.
        self.output.extend_from_slice(b"\x1b[0m");

        // Position cursor and show it if visible.
        if self.cursor_visible {
            write_cursor_goto(&mut self.output, self.cursor_row + 1, self.cursor_col + 1);
            self.output.extend_from_slice(b"\x1b[?25h");
        }

        // Swap: current now reflects what is on screen.
        std::mem::swap(&mut self.current, &mut self.desired);
    }

    /// Take the buffered output bytes. The caller writes these to stdout.
    ///
    /// After calling this, the internal buffer is empty.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }
}

// ---------------------------------------------------------------------------
// ANSI helper functions
// ---------------------------------------------------------------------------

/// Convert a display-protocol `Color` (f32 0.0-1.0) to 8-bit RGB tuple.
fn color_to_rgb8(c: &Color) -> (u8, u8, u8) {
    (
        (c.r.clamp(0.0, 1.0) * 255.0) as u8,
        (c.g.clamp(0.0, 1.0) * 255.0) as u8,
        (c.b.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

/// Write an ANSI CUP (cursor position) escape sequence.
/// Row and col are 1-based.
fn write_cursor_goto(buf: &mut Vec<u8>, row: u16, col: u16) {
    use std::io::Write;
    let _ = write!(buf, "\x1b[{};{}H", row, col);
}

/// Write ANSI SGR (select graphic rendition) escape sequences for the given
/// attributes. Always resets first, then enables the needed attributes.
fn write_sgr(buf: &mut Vec<u8>, attrs: &CellAttrs) {
    use std::io::Write;
    // Reset all attributes first.
    buf.extend_from_slice(b"\x1b[0m");

    if attrs.bold {
        buf.extend_from_slice(b"\x1b[1m");
    }
    if attrs.italic {
        buf.extend_from_slice(b"\x1b[3m");
    }
    match attrs.underline {
        1 => buf.extend_from_slice(b"\x1b[4m"),   // single underline
        2 => buf.extend_from_slice(b"\x1b[4:3m"), // curly/wave underline
        3 => buf.extend_from_slice(b"\x1b[21m"),  // double underline
        4 => buf.extend_from_slice(b"\x1b[4:4m"), // dotted underline
        5 => buf.extend_from_slice(b"\x1b[4:5m"), // dashed underline
        _ => {}
    }
    if attrs.strikethrough {
        buf.extend_from_slice(b"\x1b[9m");
    }
    if attrs.inverse {
        buf.extend_from_slice(b"\x1b[7m");
    }

    // 24-bit true-color foreground and background.
    let _ = write!(
        buf,
        "\x1b[38;2;{};{};{}m",
        attrs.fg.0, attrs.fg.1, attrs.fg.2
    );
    let _ = write!(
        buf,
        "\x1b[48;2;{};{};{}m",
        attrs.bg.0, attrs.bg.1, attrs.bg.2
    );
}

/// Convert a `Glyph` to its display character.
fn glyph_to_char(glyph: &Glyph) -> char {
    match &glyph.glyph_type {
        GlyphType::Char { ch } => *ch,
        GlyphType::Composite { text } => text.chars().next().unwrap_or(' '),
        GlyphType::Stretch { .. } => ' ',
        GlyphType::Image { .. } => ' ',
        GlyphType::Glyphless { ch } => *ch,
    }
}

#[cfg(test)]
#[path = "tty_rif_test.rs"]
mod tests;

impl TtyRif {
    /// Debug: dump the desired grid content as plain text lines.
    pub fn dump_desired(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for row in 0..self.desired.height {
            let mut line = String::new();
            for col in 0..self.desired.width {
                let idx = row * self.desired.width + col;
                line.push(self.desired.cells[idx].ch);
            }
            lines.push(line);
        }
        lines
    }
}
