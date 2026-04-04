//! GlyphMatrixBuilder — records text content into GlyphMatrix during layout.
//!
//! This builder runs alongside the existing FrameGlyphBuffer output path.
//! It observes character emissions and records them into a GlyphMatrix grid.
//! The resulting FrameDisplayState can be compared against the pixel output
//! for validation, and will eventually replace FrameGlyphBuffer entirely.

use neomacs_display_protocol::face::Face;
use neomacs_display_protocol::frame_glyphs::{
    CursorInverseInfo, CursorStyle, GlyphRowRole, StipplePattern, WindowEffectHint, WindowInfo,
    WindowTransitionHint,
};
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::types::{Color, Rect};
use std::collections::HashMap;

pub struct GlyphMatrixBuilder {
    windows: Vec<WindowMatrixEntry>,
    current_matrix: Option<GlyphMatrix>,
    current_window_id: u64,
    current_pixel_bounds: Rect,
    current_row: usize,
    in_row: bool,

    // Non-grid items
    backgrounds: Vec<BackgroundItem>,
    borders: Vec<BorderItem>,
    cursors: Vec<CursorItem>,
    images: Vec<ImageItem>,
    videos: Vec<VideoItem>,
    webkits: Vec<WebKitItem>,
    scroll_bars: Vec<ScrollBarItem>,
    cursor_inverse: Option<CursorInverseInfo>,
    faces: HashMap<u32, Face>,
    stipple_patterns: HashMap<i32, StipplePattern>,
    window_infos: Vec<WindowInfo>,
    transition_hints: Vec<WindowTransitionHint>,
    effect_hints: Vec<WindowEffectHint>,
    background_color: Color,
    font_pixel_size: f32,
    frame_id: u64,
    parent_id: u64,
    parent_x: f32,
    parent_y: f32,
    z_order: i32,
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
            backgrounds: Vec::new(),
            borders: Vec::new(),
            cursors: Vec::new(),
            images: Vec::new(),
            videos: Vec::new(),
            webkits: Vec::new(),
            scroll_bars: Vec::new(),
            cursor_inverse: None,
            faces: HashMap::new(),
            stipple_patterns: HashMap::new(),
            window_infos: Vec::new(),
            transition_hints: Vec::new(),
            effect_hints: Vec::new(),
            background_color: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            font_pixel_size: 0.0,
            frame_id: 0,
            parent_id: 0,
            parent_x: 0.0,
            parent_y: 0.0,
            z_order: 0,
        }
    }

    pub fn reset(&mut self) {
        self.windows.clear();
        self.current_matrix = None;
        self.current_window_id = 0;
        self.current_row = 0;
        self.in_row = false;
        self.backgrounds.clear();
        self.borders.clear();
        self.cursors.clear();
        self.images.clear();
        self.videos.clear();
        self.webkits.clear();
        self.scroll_bars.clear();
        self.cursor_inverse = None;
        self.faces.clear();
        self.stipple_patterns.clear();
        self.window_infos.clear();
        self.transition_hints.clear();
        self.effect_hints.clear();
        self.background_color = Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        self.font_pixel_size = 0.0;
        self.frame_id = 0;
        self.parent_id = 0;
        self.parent_x = 0.0;
        self.parent_y = 0.0;
        self.z_order = 0;
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

    // -----------------------------------------------------------------------
    // Non-grid item push methods
    // -----------------------------------------------------------------------

    pub fn push_background(&mut self, bounds: Rect, color: Color) {
        self.backgrounds.push(BackgroundItem { bounds, color });
    }

    pub fn push_border(
        &mut self,
        window_id: i64,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Color,
    ) {
        self.borders.push(BorderItem {
            window_id,
            x,
            y,
            width: w,
            height: h,
            color,
        });
    }

    pub fn push_cursor(
        &mut self,
        window_id: i32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        style: CursorStyle,
        color: Color,
    ) {
        self.cursors.push(CursorItem {
            window_id,
            x,
            y,
            width: w,
            height: h,
            style,
            color,
        });
    }

    pub fn push_image(
        &mut self,
        window_id: i64,
        role: GlyphRowRole,
        clip: Option<Rect>,
        image_id: u32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) {
        self.images.push(ImageItem {
            window_id,
            row_role: role,
            clip_rect: clip,
            image_id,
            x,
            y,
            width: w,
            height: h,
        });
    }

    pub fn push_video(
        &mut self,
        window_id: i64,
        role: GlyphRowRole,
        clip: Option<Rect>,
        video_id: u32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        loop_count: i32,
        autoplay: bool,
    ) {
        self.videos.push(VideoItem {
            window_id,
            row_role: role,
            clip_rect: clip,
            video_id,
            x,
            y,
            width: w,
            height: h,
            loop_count,
            autoplay,
        });
    }

    pub fn push_webkit(
        &mut self,
        window_id: i64,
        role: GlyphRowRole,
        clip: Option<Rect>,
        webkit_id: u32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) {
        self.webkits.push(WebKitItem {
            window_id,
            row_role: role,
            clip_rect: clip,
            webkit_id,
            x,
            y,
            width: w,
            height: h,
        });
    }

    pub fn set_cursor_inverse(&mut self, info: CursorInverseInfo) {
        self.cursor_inverse = Some(info);
    }

    pub fn set_faces(&mut self, faces: HashMap<u32, Face>) {
        self.faces = faces;
    }

    pub fn insert_face(&mut self, id: u32, face: Face) {
        self.faces.insert(id, face);
    }

    pub fn set_stipple_patterns(&mut self, patterns: HashMap<i32, StipplePattern>) {
        self.stipple_patterns = patterns;
    }

    pub fn push_window_info(&mut self, info: WindowInfo) {
        self.window_infos.push(info);
    }

    pub fn push_transition_hint(&mut self, hint: WindowTransitionHint) {
        self.transition_hints.push(hint);
    }

    pub fn push_effect_hint(&mut self, hint: WindowEffectHint) {
        self.effect_hints.push(hint);
    }

    pub fn set_background_color(&mut self, color: Color) {
        self.background_color = color;
    }

    pub fn set_font_pixel_size(&mut self, size: f32) {
        self.font_pixel_size = size;
    }

    pub fn set_frame_identity(
        &mut self,
        frame_id: u64,
        parent_id: u64,
        parent_x: f32,
        parent_y: f32,
        z_order: i32,
    ) {
        self.frame_id = frame_id;
        self.parent_id = parent_id;
        self.parent_x = parent_x;
        self.parent_y = parent_y;
        self.z_order = z_order;
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

    pub fn finish(
        mut self,
        frame_cols: usize,
        frame_rows: usize,
        char_width: f32,
        char_height: f32,
    ) -> FrameDisplayState {
        for entry in &mut self.windows {
            entry.matrix.ensure_hashes();
        }
        let mut state = FrameDisplayState::new(frame_cols, frame_rows, char_width, char_height);
        state.window_matrices = self.windows;
        state.backgrounds = self.backgrounds;
        state.borders = self.borders;
        state.cursors = self.cursors;
        state.images = self.images;
        state.videos = self.videos;
        state.webkits = self.webkits;
        state.scroll_bars = self.scroll_bars;
        state.cursor_inverse = self.cursor_inverse;
        state.faces = self.faces;
        state.stipple_patterns = self.stipple_patterns;
        state.window_infos = self.window_infos;
        state.transition_hints = self.transition_hints;
        state.effect_hints = self.effect_hints;
        state.background = self.background_color;
        state.font_pixel_size = self.font_pixel_size;
        state.frame_id = self.frame_id;
        state.parent_id = self.parent_id;
        state.parent_x = self.parent_x;
        state.parent_y = self.parent_y;
        state.z_order = self.z_order;
        state
    }
}

#[cfg(test)]
#[path = "matrix_builder_test.rs"]
mod tests;
