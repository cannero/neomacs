//! GlyphMatrixBuilder — records text content into GlyphMatrix during layout.
//!
//! This builder runs alongside the existing FrameGlyphBuffer output path.
//! It observes character emissions and records them into a GlyphMatrix grid.
//! The resulting FrameDisplayState can be compared against the pixel output
//! for validation, and will eventually replace FrameGlyphBuffer entirely.

use neomacs_display_protocol::frame_glyphs::GlyphRowRole;
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::types::Rect;

pub struct GlyphMatrixBuilder {
    windows: Vec<WindowMatrixEntry>,
    current_matrix: Option<GlyphMatrix>,
    current_window_id: u64,
    current_pixel_bounds: Rect,
    current_row: usize,
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

    pub fn reset(&mut self) {
        self.windows.clear();
        self.current_matrix = None;
        self.current_window_id = 0;
        self.current_row = 0;
        self.in_row = false;
    }

    pub fn begin_window(&mut self, window_id: u64, nrows: usize, ncols: usize, pixel_bounds: Rect) {
        self.current_matrix = Some(GlyphMatrix::new(nrows, ncols));
        self.current_window_id = window_id;
        self.current_pixel_bounds = pixel_bounds;
        self.current_row = 0;
        self.in_row = false;
    }

    pub fn end_window(&mut self) {
        if let Some(matrix) = self.current_matrix.take() {
            self.windows.push(WindowMatrixEntry {
                window_id: self.current_window_id,
                matrix,
                pixel_bounds: self.current_pixel_bounds,
            });
        }
    }

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

    pub fn end_row(&mut self) {
        self.in_row = false;
    }

    pub fn push_left_margin_char(&mut self, ch: char, face_id: u32) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                matrix.rows[self.current_row].glyphs[GlyphArea::LeftMargin as usize]
                    .push(Glyph::char(ch, face_id, 0));
            }
        }
    }

    pub fn push_left_margin_stretch(&mut self, width_cols: u16, face_id: u32) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                matrix.rows[self.current_row].glyphs[GlyphArea::LeftMargin as usize]
                    .push(Glyph::stretch(width_cols, face_id));
            }
        }
    }

    pub fn push_char(&mut self, ch: char, face_id: u32, charpos: usize) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                matrix.rows[self.current_row].glyphs[GlyphArea::Text as usize]
                    .push(Glyph::char(ch, face_id, charpos));
                matrix.rows[self.current_row].displays_text = true;
            }
        }
    }

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

    pub fn push_stretch(&mut self, width_cols: u16, face_id: u32) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                matrix.rows[self.current_row].glyphs[GlyphArea::Text as usize]
                    .push(Glyph::stretch(width_cols, face_id));
            }
        }
    }

    pub fn push_composed(&mut self, text: &str, face_id: u32, charpos: usize) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                let glyph = Glyph {
                    glyph_type: GlyphType::Composite { text: text.into() },
                    face_id,
                    charpos,
                    bidi_level: 0,
                    wide: false,
                    padding: false,
                };
                matrix.rows[self.current_row].glyphs[GlyphArea::Text as usize].push(glyph);
                matrix.rows[self.current_row].displays_text = true;
            }
        }
    }

    pub fn set_cursor(&mut self, col: u16, style: neomacs_display_protocol::frame_glyphs::CursorStyle) {
        self.set_cursor_at_row(self.current_row, col, style);
    }

    pub fn set_cursor_at_row(&mut self, row: usize, col: u16, style: neomacs_display_protocol::frame_glyphs::CursorStyle) {
        if let Some(ref mut matrix) = self.current_matrix {
            if row < matrix.rows.len() {
                matrix.rows[row].cursor_col = Some(col);
                matrix.rows[row].cursor_type = Some(style);
            }
        }
    }

    pub fn set_row_charpos(&mut self, start: usize, end: usize) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                matrix.rows[self.current_row].start_charpos = start;
                matrix.rows[self.current_row].end_charpos = end;
            }
        }
    }

    /// Extract status-line characters from FrameGlyphBuffer and append as a new matrix row.
    ///
    /// This bridges the gap until status-line rendering is fully migrated to matrix output.
    /// Call this AFTER `end_window()` — it appends a row to the most recently stored window's
    /// matrix, filtering `FrameGlyph::Char` entries that match the given `window_id` and `role`.
    pub fn push_status_line_from_buffer(
        &mut self,
        glyphs: &[neomacs_display_protocol::frame_glyphs::FrameGlyph],
        role: GlyphRowRole,
        window_id: i64,
    ) {
        use neomacs_display_protocol::frame_glyphs::FrameGlyph;
        let Some(entry) = self.windows.last_mut() else {
            return;
        };
        // Append a new row for the status line
        let mut row = neomacs_display_protocol::glyph_matrix::GlyphRow::new(role);
        row.enabled = true;
        row.mode_line = true;
        let area = &mut row.glyphs[GlyphArea::Text as usize];
        for glyph in glyphs {
            if let FrameGlyph::Char {
                window_id: wid,
                row_role,
                char: ch,
                face_id,
                ..
            } = glyph
            {
                if *wid == window_id && *row_role == role {
                    area.push(Glyph::char(*ch, *face_id, 0));
                }
            }
        }
        entry.matrix.rows.push(row);
        entry.matrix.nrows += 1;
    }

    pub fn finish(mut self, frame_cols: usize, frame_rows: usize, char_width: f32, char_height: f32) -> FrameDisplayState {
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
