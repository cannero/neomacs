//! GlyphMatrixBuilder — records text content into GlyphMatrix during layout.
//!
//! This builder runs alongside the existing FrameGlyphBuffer output path.
//! It observes character emissions and records them into a GlyphMatrix grid.
//! The resulting FrameDisplayState can be compared against the pixel output
//! for validation, and will eventually replace FrameGlyphBuffer entirely.

use crate::bidi::{self, BidiDir};
use neomacs_display_protocol::face::Face;
use neomacs_display_protocol::frame_glyphs::{
    CursorStyle, DisplaySlotId, GlyphRowRole, PhysCursor, StipplePattern, WindowEffectHint,
    WindowInfo, WindowTransitionHint,
};
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::types::{Color, Rect};
use std::collections::HashMap;

pub struct GlyphMatrixBuilder {
    windows: Vec<WindowMatrixEntry>,
    current_matrix: Option<GlyphMatrix>,
    current_window_id: u64,
    current_pixel_bounds: Rect,
    /// Whether the window currently open in the builder is the
    /// selected window. Copied into `WindowMatrixEntry.selected`
    /// by `end_window`. Mirrors GNU's per-frame
    /// `w == XWINDOW (selected_window)` check in
    /// `src/xdisp.c::update_window`.
    current_selected: bool,
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
    phys_cursor: Option<PhysCursor>,
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

#[derive(Clone)]
struct BidiGlyphUnit {
    ch: char,
    cols: Vec<usize>,
    glyphs: Vec<Glyph>,
}

fn bidi_char_for_glyph(glyph: &Glyph) -> Option<char> {
    if glyph.padding {
        return None;
    }

    match &glyph.glyph_type {
        GlyphType::Char { ch } | GlyphType::Glyphless { ch } => Some(*ch),
        GlyphType::Composite { text } => text.chars().next(),
        GlyphType::Stretch { .. } => Some(' '),
        GlyphType::Image { .. } => None,
    }
}

fn apply_bidi_mirroring(glyph: &mut Glyph, level: u8) {
    if level & 1 == 0 {
        return;
    }

    match &mut glyph.glyph_type {
        GlyphType::Char { ch } | GlyphType::Glyphless { ch } => {
            if let Some(mirrored) = bidi::bidi_mirror(*ch) {
                *ch = mirrored;
            }
        }
        GlyphType::Composite { .. } | GlyphType::Stretch { .. } | GlyphType::Image { .. } => {}
    }
}

impl GlyphMatrixBuilder {
    fn write_row_metrics(row: &mut GlyphRow, pixel_y_rel: f32, height_px: f32, ascent_px: f32) {
        row.pixel_y = pixel_y_rel;
        row.height_px = height_px.max(0.0);
        row.ascent_px = ascent_px.max(0.0).min(row.height_px.max(0.0));
    }

    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            current_matrix: None,
            current_window_id: 0,
            current_pixel_bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            current_selected: false,
            current_row: 0,
            in_row: false,
            backgrounds: Vec::new(),
            borders: Vec::new(),
            cursors: Vec::new(),
            images: Vec::new(),
            videos: Vec::new(),
            webkits: Vec::new(),
            scroll_bars: Vec::new(),
            phys_cursor: None,
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
        self.current_selected = false;
        self.current_row = 0;
        self.in_row = false;
        self.backgrounds.clear();
        self.borders.clear();
        self.cursors.clear();
        self.images.clear();
        self.videos.clear();
        self.webkits.clear();
        self.scroll_bars.clear();
        self.phys_cursor = None;
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

    pub fn begin_window(
        &mut self,
        window_id: u64,
        nrows: usize,
        ncols: usize,
        pixel_bounds: Rect,
        selected: bool,
    ) {
        self.current_matrix = Some(GlyphMatrix::new(nrows, ncols));
        self.current_window_id = window_id;
        self.current_pixel_bounds = pixel_bounds;
        self.current_selected = selected;
        self.current_row = 0;
        self.in_row = false;
    }

    pub fn end_window(&mut self) {
        if let Some(matrix) = self.current_matrix.take() {
            self.windows.push(WindowMatrixEntry {
                window_id: self.current_window_id,
                matrix,
                pixel_bounds: self.current_pixel_bounds,
                selected: self.current_selected,
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
                matrix.rows[row].mode_line = matches!(role, GlyphRowRole::ModeLine);
            }
        }
    }

    pub fn end_row(&mut self) {
        self.reorder_current_row_bidi();
        self.in_row = false;
    }

    /// Record authoritative geometry for the currently open row.
    ///
    /// `pixel_y` is frame-absolute; the builder stores rows
    /// window-relative to match GNU `struct glyph_row::y`.
    pub fn set_current_row_metrics(&mut self, pixel_y: f32, height_px: f32, ascent_px: f32) {
        if let Some(ref mut matrix) = self.current_matrix
            && self.current_row < matrix.rows.len()
        {
            let pixel_y_rel = pixel_y - self.current_pixel_bounds.y;
            Self::write_row_metrics(
                &mut matrix.rows[self.current_row],
                pixel_y_rel,
                height_px,
                ascent_px,
            );
        }
    }

    /// Record authoritative geometry for an explicit row in the current window.
    ///
    /// `pixel_y` is frame-absolute; the stored row value is window-relative.
    pub fn set_row_metrics(&mut self, row: usize, pixel_y: f32, height_px: f32, ascent_px: f32) {
        if let Some(ref mut matrix) = self.current_matrix
            && row < matrix.rows.len()
        {
            let pixel_y_rel = pixel_y - self.current_pixel_bounds.y;
            Self::write_row_metrics(&mut matrix.rows[row], pixel_y_rel, height_px, ascent_px);
        }
    }

    /// Install a complete set of text-area glyphs into the currently open row.
    ///
    /// Used by walkers that render directly into the active window matrix
    /// instead of appending a post-window chrome row.
    pub fn install_current_row_glyphs(&mut self, glyphs: Vec<Glyph>) {
        if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row < matrix.rows.len() {
                let row = &mut matrix.rows[self.current_row];
                row.displays_text = !glyphs.is_empty();
                row.glyphs[GlyphArea::Text as usize] = glyphs;
            }
        }
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

    pub fn set_cursor(
        &mut self,
        col: u16,
        style: neomacs_display_protocol::frame_glyphs::CursorStyle,
    ) {
        self.set_cursor_at_row(self.current_row, col, style);
    }

    pub fn set_cursor_at_row(
        &mut self,
        row: usize,
        col: u16,
        style: neomacs_display_protocol::frame_glyphs::CursorStyle,
    ) {
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

    pub fn push_border(&mut self, window_id: i64, x: f32, y: f32, w: f32, h: f32, color: Color) {
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
        slot_id: DisplaySlotId,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        style: CursorStyle,
        color: Color,
    ) {
        self.cursors.push(CursorItem {
            window_id,
            slot_id,
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
        self.push_image_with_slot_id(
            window_id,
            role,
            clip,
            DisplaySlotId::from_pixels(window_id, x, y, 1.0, 1.0),
            image_id,
            x,
            y,
            w,
            h,
        );
    }

    pub fn push_image_with_slot_id(
        &mut self,
        window_id: i64,
        role: GlyphRowRole,
        clip: Option<Rect>,
        slot_id: DisplaySlotId,
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
            slot_id: Some(slot_id),
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
            slot_id: Some(DisplaySlotId::from_pixels(window_id, x, y, 1.0, 1.0)),
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
            slot_id: Some(DisplaySlotId::from_pixels(window_id, x, y, 1.0, 1.0)),
            webkit_id,
            x,
            y,
            width: w,
            height: h,
        });
    }

    pub fn set_phys_cursor(&mut self, cursor: PhysCursor) {
        let mut cursor = cursor;
        let mut visual_col = None;

        if cursor.window_id as u64 == self.current_window_id
            && let Some(ref matrix) = self.current_matrix
            && cursor.row < matrix.rows.len()
        {
            let row = &matrix.rows[cursor.row];
            let text = &row.glyphs[GlyphArea::Text as usize];
            visual_col = text
                .iter()
                .position(|glyph| !glyph.padding && glyph.charpos == cursor.charpos)
                .map(|idx| idx as u16);

            if let Some(col) = visual_col {
                if col != cursor.col {
                    cursor.col = col;
                    cursor.slot_id.col = col;
                    if matrix.ncols > 0 {
                        let char_w = self.current_pixel_bounds.width / matrix.ncols as f32;
                        cursor.x = self.current_pixel_bounds.x + col as f32 * char_w;
                    }
                }
            }
        }

        if let Some(col) = visual_col
            && let Some(ref mut matrix) = self.current_matrix
            && cursor.row < matrix.rows.len()
        {
            matrix.rows[cursor.row].cursor_col = Some(col);
            matrix.rows[cursor.row].cursor_type = Some(cursor.style);
        }

        self.phys_cursor = Some(cursor);
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

    pub fn windows(&self) -> &[WindowMatrixEntry] {
        &self.windows
    }

    pub fn window_infos(&self) -> &[WindowInfo] {
        &self.window_infos
    }

    pub fn window_infos_last_mut(&mut self) -> Option<&mut WindowInfo> {
        self.window_infos.last_mut()
    }

    pub fn transition_hints(&self) -> &[WindowTransitionHint] {
        &self.transition_hints
    }

    pub fn effect_hints(&self) -> &[WindowEffectHint] {
        &self.effect_hints
    }

    pub fn truncate_transition_hints(&mut self, len: usize) {
        self.transition_hints.truncate(len);
    }

    pub fn truncate_effect_hints(&mut self, len: usize) {
        self.effect_hints.truncate(len);
    }

    pub fn background_color(&self) -> &Color {
        &self.background_color
    }

    pub fn faces(&self) -> &HashMap<u32, Face> {
        &self.faces
    }

    pub fn cursors(&self) -> &[CursorItem] {
        &self.cursors
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

    /// Begin a new status-line row on the most recently stored window.
    ///
    /// Call this AFTER `end_window()`.  It appends a new enabled, mode-line
    /// row with the given `role` to the last window's matrix and returns
    /// `true` on success.  Returns `false` when no window has been stored yet.
    pub fn begin_status_line_row(&mut self, role: GlyphRowRole) -> bool {
        let Some(entry) = self.windows.last_mut() else {
            return false;
        };
        let mut row = GlyphRow::new(role);
        row.enabled = true;
        row.mode_line = true;
        entry.matrix.rows.push(row);
        entry.matrix.nrows += 1;
        true
    }

    /// Record authoritative geometry for the most recently appended row on the
    /// most recently closed window.
    ///
    /// `pixel_y` is frame-absolute; the stored row value is window-relative.
    pub fn set_last_window_last_row_metrics(
        &mut self,
        pixel_y: f32,
        height_px: f32,
        ascent_px: f32,
    ) {
        let Some(entry) = self.windows.last_mut() else {
            return;
        };
        let Some(row) = entry.matrix.rows.last_mut() else {
            return;
        };
        let pixel_y_rel = pixel_y - entry.pixel_bounds.y;
        Self::write_row_metrics(row, pixel_y_rel, height_px, ascent_px);
    }

    /// Install a complete set of text-area glyphs into the current
    /// (last) status-line row of the most recently stored window.
    ///
    /// This is the post-Step-3.6 replacement for the old per-glyph
    /// `push_status_line_char` / `push_status_line_stretch` helpers.
    /// The `_via_backend` walkers in `status_line.rs` accumulate
    /// their glyphs inside a `TtyDisplayBackend`; on flush, the
    /// completed row's text-area `Vec<Glyph>` is installed here
    /// wholesale, which formalizes `TtyDisplayBackend` as the sole
    /// producer of status-line glyphs in the TTY path.
    ///
    /// Must be called after `begin_status_line_row`.
    pub fn install_status_line_row_glyphs(&mut self, glyphs: Vec<Glyph>) {
        let Some(entry) = self.windows.last_mut() else {
            return;
        };
        if let Some(row) = entry.matrix.rows.last_mut() {
            row.displays_text = !glyphs.is_empty();
            row.glyphs[GlyphArea::Text as usize] = glyphs;
            let _ = Self::reorder_row_bidi(row, None);
        }
    }

    /// Normalize a standalone row built outside the window-matrix walker.
    ///
    /// Used for frame-level chrome rows such as the tab bar, which are
    /// produced before any leaf window exists but still need the same bidi
    /// reordering and row bookkeeping as status-line rows.
    pub fn normalize_external_row(row: &mut GlyphRow) {
        row.displays_text = !row.glyphs[GlyphArea::Text as usize].is_empty();
        let _ = Self::reorder_row_bidi(row, None);
    }

    /// Patch the last-closed window matrix so its rightmost
    /// column shows a vertical-border glyph on every enabled row.
    ///
    /// Mirrors GNU `src/dispnew.c::build_frame_matrix_from_leaf_window`
    /// (2568-2697), which — for every window that is not the
    /// rightmost in the frame — takes the window's row slice and
    /// overwrites its last glyph with `right_border_glyph`
    /// (default `|`, face `VERTICAL_BORDER_FACE_ID`):
    ///
    ///   if (!WINDOW_RIGHTMOST_P (w))
    ///     SET_GLYPH_FROM_CHAR (right_border_glyph, '|');
    ///   ...
    ///   if (GLYPH_CHAR (right_border_glyph) != 0) {
    ///     struct glyph *border = window_row->glyphs[LAST_AREA] - 1;
    ///     SET_CHAR_GLYPH_FROM_GLYPH (f, *border, right_border_glyph);
    ///   }
    ///
    /// The window's text has already been laid out to fill all
    /// `ncols` columns; the last glyph position is then replaced
    /// with the border character. On TTY, the column corresponds
    /// to one character cell.
    ///
    /// This helper operates on the LAST window pushed into
    /// `self.windows`, which is the window most recently closed
    /// by `end_window`. Callers (`engine.rs::layout_frame_rust`)
    /// invoke this after `layout_window_rust` returns for a
    /// non-rightmost window.
    pub fn overwrite_last_window_right_border(&mut self, ch: char, face_id: u32) {
        let Some(entry) = self.windows.last_mut() else {
            return;
        };
        let ncols = entry.matrix.ncols;
        if ncols == 0 {
            return;
        }
        let target_col = ncols - 1;

        for row in &mut entry.matrix.rows {
            if !row.enabled {
                continue;
            }

            // Count existing glyphs across the three areas
            // (LeftMargin, Text, RightMargin). We treat every
            // glyph as one column advance — matching the TTY
            // RIF's `col += 1` in rasterize.
            let left_count = row.glyphs[GlyphArea::LeftMargin as usize].len();
            let right_count = row.glyphs[GlyphArea::RightMargin as usize].len();
            let current_total: usize =
                left_count + row.glyphs[GlyphArea::Text as usize].len() + right_count;

            // Truncate anything in the text area that pushes
            // the glyph count past `target_col`. Left/right
            // margin columns belong to the caller — we only
            // touch the text area.
            if current_total > target_col {
                let overshoot = current_total - target_col;
                let text_area = &mut row.glyphs[GlyphArea::Text as usize];
                let drop = overshoot.min(text_area.len());
                text_area.truncate(text_area.len() - drop);
            }

            // Pad the text area with spaces until the combined
            // count reaches `target_col`.
            let combined = |row: &GlyphRow| -> usize {
                row.glyphs[GlyphArea::LeftMargin as usize].len()
                    + row.glyphs[GlyphArea::Text as usize].len()
                    + row.glyphs[GlyphArea::RightMargin as usize].len()
            };
            while combined(row) < target_col {
                row.glyphs[GlyphArea::Text as usize].push(Glyph::char(' ', face_id, 0));
            }

            // Push the border glyph as the final glyph of the
            // text area so it lands at absolute column
            // `target_col = ncols - 1`.
            row.glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, face_id, 0));
        }
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
        state.phys_cursor = self.phys_cursor;
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

    fn collect_bidi_units(text: &[Glyph]) -> Vec<BidiGlyphUnit> {
        let mut units = Vec::new();
        let mut idx = 0;

        while idx < text.len() {
            let glyph = &text[idx];
            let Some(ch) = bidi_char_for_glyph(glyph) else {
                idx += 1;
                continue;
            };

            let mut cols = vec![idx];
            let mut glyphs = vec![glyph.clone()];
            idx += 1;

            if glyph.wide
                && idx < text.len()
                && text[idx].padding
                && text[idx].charpos == glyph.charpos
            {
                cols.push(idx);
                glyphs.push(text[idx].clone());
                idx += 1;
            }

            units.push(BidiGlyphUnit { ch, cols, glyphs });
        }

        units
    }

    fn rewrite_units_into_row(
        row: &mut GlyphRow,
        original_text: &[Glyph],
        units: &[BidiGlyphUnit],
        levels: &[u8],
        visual_order: Vec<usize>,
        cursor_logical_idx: Option<usize>,
        phys_cursor_logical_idx: Option<usize>,
    ) -> Option<u16> {
        let available_cols: Vec<usize> = units
            .iter()
            .flat_map(|unit| unit.cols.iter().copied())
            .collect();
        let mut next_col = 0usize;
        let mut reordered = original_text.to_vec();
        let mut visual_cursor_col = None;
        let mut remapped_phys_cursor_col = None;

        for logical_idx in visual_order {
            let unit = &units[logical_idx];
            let unit_len = unit.glyphs.len();
            let Some(target_cols) = available_cols.get(next_col..next_col + unit_len) else {
                return None;
            };
            if !target_cols.windows(2).all(|w| w[1] == w[0] + 1) {
                return None;
            }

            let target_start = target_cols[0];
            let mut placed = unit.glyphs.clone();
            for glyph in &mut placed {
                glyph.bidi_level = levels[logical_idx];
            }
            if let Some(first) = placed.first_mut() {
                apply_bidi_mirroring(first, levels[logical_idx]);
            }
            for (offset, glyph) in placed.into_iter().enumerate() {
                reordered[target_start + offset] = glyph;
            }

            if cursor_logical_idx == Some(logical_idx) {
                visual_cursor_col = Some(target_start as u16);
            }
            if phys_cursor_logical_idx == Some(logical_idx) {
                remapped_phys_cursor_col = Some(target_start as u16);
            }

            next_col += unit_len;
        }

        row.glyphs[GlyphArea::Text as usize] = reordered;
        if let Some(col) = visual_cursor_col {
            row.cursor_col = Some(col);
        }
        remapped_phys_cursor_col
    }

    fn reorder_row_bidi(row: &mut GlyphRow, phys_cursor_col: Option<u16>) -> Option<u16> {
        let original_text = row.glyphs[GlyphArea::Text as usize].clone();
        if original_text.is_empty() {
            return None;
        }

        let units = Self::collect_bidi_units(&original_text);
        if units.is_empty() {
            return None;
        }

        let chars: String = units.iter().map(|unit| unit.ch).collect();
        let levels = bidi::resolve_levels(&chars, BidiDir::Auto);
        if levels.len() != units.len() {
            return None;
        }

        let cursor_logical_idx = row.cursor_col.and_then(|col| {
            units
                .iter()
                .position(|unit| unit.cols.iter().any(|&idx| idx == col as usize))
        });
        let phys_cursor_logical_idx = phys_cursor_col.and_then(|col| {
            units
                .iter()
                .position(|unit| unit.cols.iter().any(|&idx| idx == col as usize))
        });

        let visual_order = if levels.iter().all(|&level| level == 0) {
            (0..units.len()).collect()
        } else {
            bidi::reorder_visual(&levels)
        };

        Self::rewrite_units_into_row(
            row,
            &original_text,
            &units,
            &levels,
            visual_order,
            cursor_logical_idx,
            phys_cursor_logical_idx,
        )
    }

    fn reorder_current_row_bidi(&mut self) {
        let remapped_cursor_col = if let Some(ref mut matrix) = self.current_matrix {
            if self.current_row >= matrix.rows.len() {
                return;
            }

            let phys_cursor_col = self
                .phys_cursor
                .as_ref()
                .filter(|cursor| {
                    cursor.window_id as u64 == self.current_window_id
                        && cursor.row == self.current_row
                })
                .map(|cursor| cursor.col);

            Self::reorder_row_bidi(&mut matrix.rows[self.current_row], phys_cursor_col)
        } else {
            None
        };

        if let Some(col) = remapped_cursor_col
            && let Some(ref mut cursor) = self.phys_cursor
            && cursor.window_id as u64 == self.current_window_id
            && cursor.row == self.current_row
        {
            cursor.col = col;
            cursor.slot_id.col = col;
            if let Some(ref matrix) = self.current_matrix
                && matrix.ncols > 0
            {
                let char_w = self.current_pixel_bounds.width / matrix.ncols as f32;
                cursor.x = self.current_pixel_bounds.x + col as f32 * char_w;
            }
        }
    }
}

#[cfg(test)]
#[path = "matrix_builder_test.rs"]
mod tests;
