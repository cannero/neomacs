//! Display-walker status-line rendering.
//!
//! Mode-line, header-line, and tab-line flow through the
//! walker defined here. The walker produces
//! glyphs via TtyDisplayBackend::produce_glyph and installs the
//! completed row into GlyphMatrixBuilder wholesale; above the
//! backend trait boundary the code is frontend-agnostic, matching
//! GNU Emacs's display_mode_line -> display_mode_element ->
//! display_line -> PRODUCE_GLYPHS architecture.
//!
//! Housed types include StatusLineKind, StatusLineFace,
//! StatusLineSpec, OverlayFaceRun, and the
//! build_rust_status_line_spec property harvester that walks
//! text-property intervals (face, font-lock-face, display).
//!
//! History: this module started as status_line.rs, a divergent
//! parallel implementation of display-line rendering that did not
//! process display properties and dropped doom-modeline's
//! (space :align-to ...) forms. Steps 3.3' through 3.6 of the
//! display-engine unification plan merged it into the backend
//! trait and renamed the file to reflect its new role.

use super::engine::LayoutEngine;
use super::neovm_bridge::{FaceResolver, ResolvedFace};
use super::unicode::decode_utf8;
use neomacs_display_protocol::face::{BoxType, Face, FaceAttributes, UnderlineStyle};
use neomacs_display_protocol::frame_glyphs::GlyphRowRole;
use neomacs_display_protocol::types::Color;
use neovm_core::buffer::text_props::TextPropertyTable;
use neovm_core::emacs_core::Value;
use neovm_core::emacs_core::value::get_string_text_properties_table_for_value;
use std::collections::HashMap;

/// Which kind of status line to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusLineKind {
    ModeLine,
    HeaderLine,
    TabLine,
}

impl StatusLineKind {
    fn row_role(self) -> GlyphRowRole {
        match self {
            Self::ModeLine => GlyphRowRole::ModeLine,
            Self::HeaderLine => GlyphRowRole::HeaderLine,
            Self::TabLine => GlyphRowRole::TabLine,
        }
    }
}

/// Shared render-facing face spec for all status-line backends.
#[derive(Debug, Clone)]
pub(crate) struct StatusLineFace {
    pub(crate) face_id: u32,
    pub(crate) foreground: Color,
    pub(crate) background: Color,
    pub(crate) font_family: String,
    pub(crate) font_file_path: Option<String>,
    pub(crate) font_weight: u16,
    pub(crate) italic: bool,
    pub(crate) font_size: f32,
    pub(crate) underline_style: u8,
    pub(crate) underline_color: Option<Color>,
    pub(crate) strike_through: bool,
    pub(crate) strike_through_color: Option<Color>,
    pub(crate) overline: bool,
    pub(crate) overline_color: Option<Color>,
    pub(crate) overstrike: bool,
    pub(crate) box_type: BoxType,
    pub(crate) box_color: Option<Color>,
    pub(crate) box_line_width: i32,
    pub(crate) box_corner_radius: i32,
    pub(crate) box_border_style: u32,
    pub(crate) box_border_speed: f32,
    pub(crate) box_color2: Option<Color>,
    pub(crate) box_h_line_width: i32,
    pub(crate) font_char_width: f32,
    pub(crate) font_ascent: f32,
    pub(crate) font_descent: i32,
    pub(crate) underline_position: i32,
    pub(crate) underline_thickness: i32,
    pub(crate) stipple: i32,
}

impl StatusLineFace {
    pub(crate) fn from_resolved(face_id: u32, face: &ResolvedFace) -> Self {
        let font_descent = if face.font_line_height > 0.0 && face.font_ascent > 0.0 {
            (face.font_line_height - face.font_ascent).max(0.0).ceil() as i32
        } else {
            0
        };
        Self {
            face_id,
            foreground: Color::from_pixel(face.fg),
            background: Color::from_pixel(face.bg),
            font_family: if face.font_family.is_empty() {
                "monospace".to_string()
            } else {
                face.font_family.clone()
            },
            font_file_path: None,
            font_weight: face.font_weight,
            italic: face.italic,
            font_size: face.font_size,
            underline_style: face.underline_style,
            underline_color: (face.underline_style > 0)
                .then(|| Color::from_pixel(face.underline_color)),
            strike_through: face.strike_through,
            strike_through_color: face
                .strike_through
                .then(|| Color::from_pixel(face.strike_through_color)),
            overline: face.overline,
            overline_color: face
                .overline
                .then(|| Color::from_pixel(face.overline_color)),
            overstrike: face.overstrike,
            box_type: if face.box_type != 0 {
                BoxType::Line
            } else {
                BoxType::None
            },
            box_color: (face.box_type != 0 && face.box_color != 0)
                .then(|| Color::from_pixel(face.box_color)),
            box_line_width: face.box_line_width,
            box_corner_radius: 0,
            box_border_style: 0,
            box_border_speed: 1.0,
            box_color2: None,
            box_h_line_width: face.box_line_width,
            font_char_width: face.font_char_width,
            font_ascent: face.font_ascent,
            font_descent,
            underline_position: 1,
            underline_thickness: 1,
            stipple: 0,
        }
    }

    fn with_color_override(&self, face_id: u32, fg: Option<Color>, bg: Option<Color>) -> Self {
        let mut face = self.clone();
        face.face_id = face_id;
        if let Some(color) = fg {
            face.foreground = color;
        }
        if let Some(color) = bg {
            face.background = color;
        }
        face
    }

    pub(crate) fn render_face(&self) -> Face {
        let mut attrs = FaceAttributes::empty();
        if self.font_weight >= 700 {
            attrs |= FaceAttributes::BOLD;
        }
        if self.italic {
            attrs |= FaceAttributes::ITALIC;
        }
        if self.underline_style > 0 {
            attrs |= FaceAttributes::UNDERLINE;
        }
        if self.strike_through {
            attrs |= FaceAttributes::STRIKE_THROUGH;
        }
        if self.overline {
            attrs |= FaceAttributes::OVERLINE;
        }
        if !matches!(self.box_type, BoxType::None) {
            attrs |= FaceAttributes::BOX;
        }
        Face {
            id: self.face_id,
            foreground: self.foreground,
            background: self.background,
            underline_color: self.underline_color,
            overline_color: self.overline_color,
            strike_through_color: self.strike_through_color,
            box_color: self.box_color,
            font_family: self.font_family.clone(),
            font_size: self.font_size,
            font_weight: self.font_weight,
            attributes: attrs,
            underline_style: underline_style_from_code(self.underline_style),
            box_type: self.box_type,
            box_line_width: self.box_line_width,
            box_corner_radius: self.box_corner_radius,
            box_border_style: self.box_border_style,
            box_border_speed: self.box_border_speed,
            box_color2: self.box_color2,
            font_file_path: self.font_file_path.clone(),
            font_ascent: self.font_ascent as i32,
            font_descent: self.font_descent,
            underline_position: self.underline_position.max(1),
            underline_thickness: self.underline_thickness.max(1),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum StatusLineAdvanceMode {
    Fixed,
    Measured,
}

/// A face run within an overlay/display string: byte offset + fg/bg colors + face_id.
#[derive(Debug, Clone)]
pub(crate) struct OverlayFaceRun {
    pub byte_offset: u16,
    pub fg: u32,
    pub bg: u32,
    /// Face has :extend attribute (bg extends to end of visual line)
    pub extend: bool,
    /// Emacs face ID for full face attribute resolution via FFI
    pub face_id: u32,
}

/// Parse face runs appended after text in a buffer.
/// Runs are stored as 14-byte records: u16 byte_offset + u32 fg + u32 bg + u32 face_id.
/// Bit 31 of bg encodes the :extend flag (1 = extends to end of line).
pub(crate) fn parse_overlay_face_runs(
    buf: &[u8],
    text_len: usize,
    nruns: i32,
) -> Vec<OverlayFaceRun> {
    let mut runs = Vec::with_capacity(nruns as usize);
    let runs_start = text_len;
    for ri in 0..nruns as usize {
        let off = runs_start + ri * 14;
        if off + 14 <= buf.len() {
            let byte_offset = u16::from_ne_bytes([buf[off], buf[off + 1]]);
            let fg = u32::from_ne_bytes([buf[off + 2], buf[off + 3], buf[off + 4], buf[off + 5]]);
            let raw_bg =
                u32::from_ne_bytes([buf[off + 6], buf[off + 7], buf[off + 8], buf[off + 9]]);
            let extend = (raw_bg & 0x80000000) != 0;
            let bg = raw_bg & 0x00FFFFFF;
            let face_id =
                u32::from_ne_bytes([buf[off + 10], buf[off + 11], buf[off + 12], buf[off + 13]]);
            runs.push(OverlayFaceRun {
                byte_offset,
                fg,
                bg,
                extend,
                face_id,
            });
        }
    }
    runs
}

/// An align-to entry within an overlay string: byte offset + target pixel position.
#[derive(Debug, Clone)]
pub(crate) struct OverlayAlignEntry {
    pub byte_offset: u16,
    pub align_to_px: f32,
}

/// Parse align-to entries appended after face runs in a buffer.
/// Entries are stored as 6-byte records: u16 byte_offset + f32 align_to_px.
pub(crate) fn parse_overlay_align_entries(
    buf: &[u8],
    text_len: usize,
    nruns: i32,
    naligns: i32,
) -> Vec<OverlayAlignEntry> {
    let mut entries = Vec::with_capacity(naligns as usize);
    let aligns_start = text_len + nruns as usize * 14;
    for ai in 0..naligns as usize {
        let off = aligns_start + ai * 6;
        if off + 6 <= buf.len() {
            let byte_offset = u16::from_ne_bytes([buf[off], buf[off + 1]]);
            let align_to_px =
                f32::from_ne_bytes([buf[off + 2], buf[off + 3], buf[off + 4], buf[off + 5]]);
            entries.push(OverlayAlignEntry {
                byte_offset,
                align_to_px,
            });
        }
    }
    entries
}

/// Get the background color from the overlay face run covering the given byte index.
/// Returns the run's bg color if it has one, otherwise returns `fallback`.
/// This is used for align-to stretches within overlay strings to avoid
/// inheriting the buffer position's face (e.g., minibuffer-prompt).
pub(crate) fn overlay_run_bg_at(
    runs: &[OverlayFaceRun],
    byte_idx: usize,
    fallback: Color,
) -> Color {
    if runs.is_empty() {
        return fallback;
    }
    // Find the run covering byte_idx
    let mut cr = 0;
    while cr + 1 < runs.len() && byte_idx >= runs[cr + 1].byte_offset as usize {
        cr += 1;
    }
    if byte_idx >= runs[cr].byte_offset as usize && runs[cr].bg != 0 {
        Color::from_pixel(runs[cr].bg)
    } else {
        fallback
    }
}

/// Get the background color and extend flag from the overlay face run at byte_idx.
/// Returns (bg_color, extend) if a run covers byte_idx, otherwise None.
pub(crate) fn overlay_run_bg_extend_at(
    runs: &[OverlayFaceRun],
    byte_idx: usize,
) -> Option<(Color, bool)> {
    if runs.is_empty() {
        return None;
    }
    let mut cr = 0;
    while cr + 1 < runs.len() && byte_idx >= runs[cr + 1].byte_offset as usize {
        cr += 1;
    }
    if byte_idx >= runs[cr].byte_offset as usize && runs[cr].bg != 0 {
        Some((Color::from_pixel(runs[cr].bg), runs[cr].extend))
    } else {
        None
    }
}

/// Apply the face run covering the current byte index.
/// Returns the updated current_run index.
pub(crate) fn apply_overlay_face_run(
    runs: &[OverlayFaceRun],
    byte_idx: usize,
    current_run: usize,
) -> usize {
    let mut cr = current_run;
    // Advance to the correct run
    while cr + 1 < runs.len() && byte_idx >= runs[cr + 1].byte_offset as usize {
        cr += 1;
    }
    if byte_idx >= runs[cr].byte_offset as usize {
        // Pre-advance if next run starts at next byte
        if cr + 1 < runs.len() && byte_idx + 1 >= runs[cr + 1].byte_offset as usize {
            cr += 1;
        }
    }
    cr
}

/// A display property record extracted from a mode-line string.
/// Each record is 16 bytes: u16 byte_offset, u16 covers_bytes,
/// u32 gpu_id, u16 width, u16 height, u16 ascent, u16 pad.
#[derive(Debug, Clone)]
struct DisplayPropRecord {
    byte_offset: u16,
    covers_bytes: u16,
    gpu_id: u32,
    width: u16,
    height: u16,
    ascent: u16,
}

/// Parse display property records appended after face runs in a buffer.
fn parse_display_props(buf: &[u8], start: usize, count: usize) -> Vec<DisplayPropRecord> {
    let mut props = Vec::with_capacity(count);
    for i in 0..count {
        let off = start + i * 16;
        if off + 16 <= buf.len() {
            props.push(DisplayPropRecord {
                byte_offset: u16::from_ne_bytes([buf[off], buf[off + 1]]),
                covers_bytes: u16::from_ne_bytes([buf[off + 2], buf[off + 3]]),
                gpu_id: u32::from_ne_bytes([
                    buf[off + 4],
                    buf[off + 5],
                    buf[off + 6],
                    buf[off + 7],
                ]),
                width: u16::from_ne_bytes([buf[off + 8], buf[off + 9]]),
                height: u16::from_ne_bytes([buf[off + 10], buf[off + 11]]),
                ascent: u16::from_ne_bytes([buf[off + 12], buf[off + 13]]),
            });
        }
    }
    props
}

fn parse_status_line_align_entries(
    buf: &[u8],
    start: usize,
    count: usize,
) -> Vec<OverlayAlignEntry> {
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let off = start + i * 6;
        if off + 6 <= buf.len() {
            let byte_offset = u16::from_ne_bytes([buf[off], buf[off + 1]]);
            let align_to_px =
                f32::from_ne_bytes([buf[off + 2], buf[off + 3], buf[off + 4], buf[off + 5]]);
            entries.push(OverlayAlignEntry {
                byte_offset,
                align_to_px,
            });
        }
    }
    entries
}

#[derive(Debug, Clone)]
pub(crate) struct StatusLineSpec {
    kind: StatusLineKind,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    window_id: i64,
    char_width: f32,
    ascent: f32,
    face: StatusLineFace,
    text: Vec<u8>,
    face_runs: Vec<OverlayFaceRun>,
    run_faces: HashMap<u32, StatusLineFace>,
    display_props: Vec<DisplayPropRecord>,
    align_entries: Vec<OverlayAlignEntry>,
    advance_mode: StatusLineAdvanceMode,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StatusLineOutputProgress {
    pub end_x: f32,
    pub end_col: i64,
    pub y: f32,
    pub height: f32,
}

impl StatusLineSpec {
    fn plain(
        kind: StatusLineKind,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        window_id: i64,
        char_width: f32,
        ascent: f32,
        face: StatusLineFace,
        text: String,
    ) -> Self {
        Self {
            kind,
            x,
            y,
            width,
            height,
            window_id,
            char_width,
            ascent,
            face,
            text: text.into_bytes(),
            face_runs: Vec::new(),
            run_faces: HashMap::new(),
            display_props: Vec::new(),
            align_entries: Vec::new(),
            advance_mode: StatusLineAdvanceMode::Fixed,
        }
    }
}

fn same_resolved_face(lhs: &ResolvedFace, rhs: &ResolvedFace) -> bool {
    lhs.fg == rhs.fg
        && lhs.bg == rhs.bg
        && lhs.font_family == rhs.font_family
        && lhs.font_weight == rhs.font_weight
        && lhs.italic == rhs.italic
        && (lhs.font_size - rhs.font_size).abs() <= f32::EPSILON
        && lhs.underline_style == rhs.underline_style
        && lhs.underline_color == rhs.underline_color
        && lhs.strike_through == rhs.strike_through
        && lhs.strike_through_color == rhs.strike_through_color
        && lhs.overline == rhs.overline
        && lhs.overline_color == rhs.overline_color
        && lhs.box_type == rhs.box_type
        && lhs.box_color == rhs.box_color
        && lhs.box_line_width == rhs.box_line_width
        && lhs.extend == rhs.extend
        && lhs.overstrike == rhs.overstrike
}

fn underline_style_from_code(code: u8) -> UnderlineStyle {
    match code {
        1 => UnderlineStyle::Line,
        2 => UnderlineStyle::Wave,
        3 => UnderlineStyle::Double,
        4 => UnderlineStyle::Dotted,
        5 => UnderlineStyle::Dashed,
        _ => UnderlineStyle::None,
    }
}

impl LayoutEngine {
    pub(crate) fn realize_status_line_face(
        &mut self,
        face_id: u32,
        face: &ResolvedFace,
        char_w: f32,
        ascent: f32,
        row_height: f32,
    ) -> StatusLineFace {
        let mut face = StatusLineFace::from_resolved(face_id, face);
        self.ensure_status_line_face_metrics(&mut face, char_w, ascent, row_height);
        face
    }

    pub(crate) fn status_line_row_height_for_face(
        &mut self,
        face: &ResolvedFace,
        char_w: f32,
        fallback_ascent: f32,
        fallback_row_height: f32,
    ) -> f32 {
        // GNU Emacs frame.c:1184-1185 — non-window (TTY) frames have
        //   f->column_width = 1;
        //   f->line_height  = 1;
        // and every row (including mode-line, header-line, tab-line) is
        // exactly one character cell tall. Face font metrics are GUI
        // pixel measurements and must not contribute to row sizing on
        // a TTY frame: the layout engine's `char_w` and
        // `fallback_row_height` are both 1.0 in that case
        // (set by `bootstrap_buffers` at neomacs-bin/src/main.rs:1691-1694),
        // so detect the TTY context by the 1.0-cell markers and return
        // the cell height directly. Without this early return, the
        // face-derived `line_height` above was producing a 3-row-tall
        // mode-line region in the TTY pty capture: the mode-line text
        // painted on the first row and the remaining two rows showed up
        // as blank padding that looked like extra echo-area rows.
        if char_w <= 1.0 && fallback_row_height <= 1.0 {
            return fallback_row_height.max(1.0);
        }
        let face =
            self.realize_status_line_face(0, face, char_w, fallback_ascent, fallback_row_height);
        let line_height = (face.font_ascent + face.font_descent as f32)
            .max(1.0)
            .ceil();
        let box_pixels = if face.box_type != BoxType::None && face.box_h_line_width != 0 {
            2.0 * face.box_h_line_width.unsigned_abs() as f32
        } else {
            0.0
        };
        let minimum_row_height = fallback_row_height.ceil().max(1.0);
        (line_height + box_pixels).max(minimum_row_height)
    }

    fn ensure_status_line_face_metrics(
        &mut self,
        face: &mut StatusLineFace,
        fallback_char_width: f32,
        fallback_ascent: f32,
        row_height: f32,
    ) {
        let needs_metrics = face.font_char_width <= 0.0
            || face.font_ascent <= 0.0
            || (face.font_ascent + face.font_descent as f32) <= 0.0;

        if needs_metrics {
            let metrics = self.status_line_font_metrics(face);

            if face.font_char_width <= 0.0 && metrics.char_width > 0.0 {
                face.font_char_width = metrics.char_width;
            }
            if face.font_ascent <= 0.0 && metrics.ascent > 0.0 {
                face.font_ascent = metrics.ascent;
            }
            if (face.font_ascent + face.font_descent as f32) <= 0.0 && metrics.line_height > 0.0 {
                face.font_descent = (metrics.line_height - metrics.ascent).max(0.0).ceil() as i32;
            }
        }

        if face.font_char_width <= 0.0 {
            face.font_char_width = fallback_char_width.max(1.0);
        }
        if face.font_ascent <= 0.0 {
            face.font_ascent = fallback_ascent.max(1.0);
        }
        if (face.font_ascent + face.font_descent as f32) <= 0.0 {
            face.font_descent = (row_height - face.font_ascent).max(0.0).ceil() as i32;
        }
    }

    /// Step 3.5: backend-routed twin of `render_status_line_spec`.
    ///
    /// Walks a pre-built `StatusLineSpec` the same way
    /// `render_status_line_spec` does, but emits every character and
    /// stretch glyph through a `TtyDisplayBackend` before bridging
    /// the produced `GlyphRow` back into the caller's
    /// `GlyphMatrixBuilder` via `push_status_line_char` /
    /// `push_status_line_stretch`.
    ///
    /// The bridging step is intentional architectural scaffolding:
    /// the glyphs traverse the `DisplayBackend::produce_glyph` call
    /// (exercising the trait-object boundary that GNU's `RIF`
    /// abstracts), and the bridge feeds them into the matrix
    /// builder in the same shape the old path produced. Step 3.6
    /// will delete `push_status_line_*` and have the backend feed
    /// the frame matrix directly.
    ///
    /// Preserves every 3.3′ behavior bit-for-bit: align-to gaps
    /// emit `N` individual space glyphs (matching `TtyRif::glyph_to_char`
    /// which renders a `Stretch` glyph as a single cell regardless of
    /// `width_cols`), display-property stretch entries advance
    /// `sl_x_offset` without producing a visible glyph, face runs
    /// update `active_run_face` exactly as before.
    pub(crate) fn render_status_line_spec_via_backend(
        &mut self,
        spec: &StatusLineSpec,
        matrix_row: Option<usize>,
        mut builder: Option<&mut crate::matrix_builder::GlyphMatrixBuilder>,
        mut on_progress: Option<&mut dyn FnMut(StatusLineOutputProgress)>,
    ) -> Option<StatusLineOutputProgress> {
        use crate::display_backend::{DisplayBackend, GlyphKind, TtyDisplayBackend};
        use neomacs_display_protocol::glyph_matrix::GlyphRow;

        // Face registration on the matrix builder runs on the same
        // schedule as the old path so the builder's face cache has
        // the right entries when rasterization resolves face ids.
        if let Some(ref mut b) = builder {
            if let Some(row) = matrix_row {
                b.begin_row(row, spec.kind.row_role());
                let row_ascent = if spec.face.font_ascent > 0.0 {
                    spec.face.font_ascent
                } else {
                    spec.ascent
                }
                .max(0.0)
                .min(spec.height.max(1.0));
                b.set_current_row_metrics(spec.y, spec.height, row_ascent);
            } else if b.begin_status_line_row(spec.kind.row_role()) {
                let row_ascent = if spec.face.font_ascent > 0.0 {
                    spec.face.font_ascent
                } else {
                    spec.ascent
                }
                .max(0.0)
                .min(spec.height.max(1.0));
                b.set_last_window_last_row_metrics(spec.y, spec.height, row_ascent);
            }
        }

        {
            let rendered = spec.face.render_face();
            if let Some(ref mut b) = builder {
                b.insert_face(spec.face.face_id, rendered);
            }
        }

        if spec.text.is_empty() {
            return Some(StatusLineOutputProgress {
                end_x: 0.0,
                end_col: 0,
                y: spec.y,
                height: spec.height,
            });
        }

        // The backend collects all character / stretch glyphs for
        // the row. Face ids are set on each produced glyph via the
        // TtyDisplayBackend::produce_glyph path (which now honors
        // `face.id` after the Step 3.4 fix), so no per-glyph face id
        // bookkeeping is needed outside the backend.
        //
        // Note: the status-line walker does its own per-character
        // measurement via `spec.char_width` / `status_line_advance`
        // (which consult `self.font_metrics` on the GUI path), so
        // `backend.char_advance` is never consulted here. The
        // backend is used purely for glyph accumulation, and that
        // accumulation is identical between `TtyDisplayBackend` and
        // `GuiDisplayBackend`. So this site always constructs a
        // `TtyDisplayBackend`, which also avoids the borrow
        // conflict between `self.font_metrics` (held by a
        // hypothetical `GuiDisplayBackend`) and the walker's
        // `self.status_line_advance` call a few lines below.
        let mut backend = TtyDisplayBackend::new();
        // The backend Face we feed into produce_glyph — rebuilt on
        // each face change so that `face.id` on the produced glyph
        // matches the active run face.
        let mut current_render_face = spec.face.render_face();

        let mut sl_x_offset = 0.0f32;
        let mut byte_idx = 0usize;
        let mut current_run = 0usize;
        let mut dp_idx = 0usize;
        let mut align_idx = 0usize;
        let mut active_run_face: Option<StatusLineFace> = None;
        let mut emit_progress = |end_x: f32| {
            if let Some(ref mut cb) = on_progress {
                cb(StatusLineOutputProgress {
                    end_x: end_x.min(spec.width).max(0.0),
                    end_col: (end_x / spec.char_width.max(1.0)).round().max(0.0) as i64,
                    y: spec.y,
                    height: spec.height,
                });
            }
        };

        // Text-geometry fields mirror the original; they are unused
        // for glyph emission (the backend produces cell glyphs with
        // no per-glyph y coordinate) but computed here so future
        // GUI backends can receive them through the same walker.
        let _ascent = if spec.face.font_ascent > 0.0 {
            spec.face.font_ascent
        } else {
            spec.ascent
        };
        let _inset = if spec.face.box_h_line_width > 0 {
            spec.face.box_h_line_width as f32
        } else {
            0.0
        };

        while byte_idx < spec.text.len() && sl_x_offset < spec.width {
            // --- align-to entries ---
            if align_idx < spec.align_entries.len()
                && byte_idx == spec.align_entries[align_idx].byte_offset as usize
            {
                let target_x = spec.align_entries[align_idx].align_to_px;
                if target_x > sl_x_offset {
                    let gap = target_x - sl_x_offset;
                    let cols = (gap / spec.char_width.max(1.0)).round() as usize;
                    // Emit `cols` individual space glyphs via the
                    // backend. The TtyDisplayBackend then materializes
                    // them as Char(' ') glyphs, matching the 3.3′
                    // workaround for TtyRif::glyph_to_char's
                    // single-cell Stretch rendering.
                    for _ in 0..cols {
                        backend.produce_glyph(GlyphKind::Char(' '), &current_render_face, 0);
                        sl_x_offset += spec.char_width.max(1.0);
                        emit_progress(sl_x_offset);
                    }
                    sl_x_offset = target_x;
                    emit_progress(sl_x_offset);
                }
                align_idx += 1;
                let (_ch, ch_len) = decode_utf8(&spec.text[byte_idx..]);
                byte_idx += ch_len;
                continue;
            }

            // --- display-prop entries (stretch / image) ---
            if dp_idx < spec.display_props.len() {
                let dp = &spec.display_props[dp_idx];
                if byte_idx == dp.byte_offset as usize {
                    if dp.width > 0 {
                        sl_x_offset += dp.width as f32;
                        emit_progress(sl_x_offset);
                    }
                    byte_idx = (dp.byte_offset + dp.covers_bytes) as usize;
                    dp_idx += 1;
                    continue;
                }
            }

            // --- resolve face for current run ---
            if current_run < spec.face_runs.len() {
                while current_run + 1 < spec.face_runs.len()
                    && byte_idx >= spec.face_runs[current_run + 1].byte_offset as usize
                {
                    current_run += 1;
                }
                if byte_idx >= spec.face_runs[current_run].byte_offset as usize {
                    let run = &spec.face_runs[current_run];
                    if run.fg != 0 || run.bg != 0 {
                        if let Some(run_face) = spec.run_faces.get(&run.face_id) {
                            if let Some(ref mut b) = builder {
                                b.insert_face(run_face.face_id, run_face.render_face());
                            }
                            current_render_face = run_face.render_face();
                            active_run_face = Some(run_face.clone());
                        } else if run.face_id != 0 {
                            let rf = spec.face.with_color_override(
                                run.face_id,
                                Some(Color::from_pixel(run.fg)),
                                Some(Color::from_pixel(run.bg)),
                            );
                            if let Some(ref mut b) = builder {
                                b.insert_face(run.face_id, rf.render_face());
                            }
                            current_render_face = rf.render_face();
                            active_run_face = Some(rf);
                        } else {
                            current_render_face = spec.face.render_face();
                            active_run_face = None;
                        }
                    }
                }
            }

            // --- compute end of current text segment ---
            let mut end_byte = spec.text.len();
            if align_idx < spec.align_entries.len() {
                end_byte = end_byte.min(spec.align_entries[align_idx].byte_offset as usize);
            }
            if dp_idx < spec.display_props.len() {
                end_byte = end_byte.min(spec.display_props[dp_idx].byte_offset as usize);
            }
            if current_run < spec.face_runs.len() {
                let current_run_offset = spec.face_runs[current_run].byte_offset as usize;
                if byte_idx < current_run_offset {
                    end_byte = end_byte.min(current_run_offset);
                } else if current_run + 1 < spec.face_runs.len() {
                    end_byte = end_byte.min(spec.face_runs[current_run + 1].byte_offset as usize);
                }
            }
            end_byte = end_byte.max(byte_idx);

            // --- emit text run through the backend ---
            //
            // Walks the text slice char-by-char, measuring via the
            // backend's own char_advance and stopping when the
            // remaining width is exhausted. Mirrors the inner loop of
            // `render_text_run` for the backend path.
            let effective_face = active_run_face.as_ref().unwrap_or(&spec.face);
            let fallback_width = spec.char_width.max(1.0);
            let mut run_offset = 0usize;
            let mut run_advance = 0.0f32;
            while run_offset < (end_byte - byte_idx) && sl_x_offset + run_advance < spec.width {
                let (ch, ch_len) = decode_utf8(&spec.text[byte_idx + run_offset..end_byte]);
                run_offset += ch_len;
                if ch == '\n' || ch == '\r' {
                    continue;
                }
                let advance = unsafe {
                    self.status_line_advance(&spec.advance_mode, effective_face, fallback_width, ch)
                };
                backend.produce_glyph(GlyphKind::Char(ch), &current_render_face, 0);
                run_advance += advance;
                emit_progress(sl_x_offset + run_advance);
            }
            sl_x_offset += run_advance;
            byte_idx = end_byte;
        }

        // Flush the row through the backend and install the
        // produced text-area glyphs into the caller's matrix
        // builder wholesale. The backend is the sole producer.
        let mut flush_row = GlyphRow::new(spec.kind.row_role());
        flush_row.enabled = true;
        flush_row.mode_line = true;
        backend.finish_row(flush_row);
        if let Some(ref mut b) = builder {
            for mut row in backend.take_rows() {
                let text_glyphs = std::mem::take(&mut row.glyphs[1]);
                if matrix_row.is_some() {
                    b.install_current_row_glyphs(text_glyphs);
                    b.end_row();
                } else {
                    b.install_status_line_row_glyphs(text_glyphs);
                }
            }
        }
        Some(StatusLineOutputProgress {
            end_x: sl_x_offset.min(spec.width).max(0.0),
            end_col: (sl_x_offset / spec.char_width.max(1.0)).round().max(0.0) as i64,
            y: spec.y,
            height: spec.height,
        })
    }

    /// Step 3.5 entry point: equivalent to `render_rust_status_line_value`
    /// but routes glyph emission through `TtyDisplayBackend` via
    /// `render_status_line_spec_via_backend`.
    pub(crate) fn render_rust_status_line_value_via_backend(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        matrix_row: usize,
        window_id: i64,
        char_w: f32,
        ascent: f32,
        next_face_id: &mut u32,
        face: &ResolvedFace,
        rendered: Value,
        face_resolver: &FaceResolver,
        kind: StatusLineKind,
        builder: Option<&mut crate::matrix_builder::GlyphMatrixBuilder>,
        on_progress: Option<&mut dyn FnMut(StatusLineOutputProgress)>,
    ) -> Option<StatusLineOutputProgress> {
        if let Some(spec) = self.build_rust_status_line_spec(
            x,
            y,
            width,
            height,
            window_id,
            char_w,
            ascent,
            next_face_id,
            face,
            rendered,
            face_resolver,
            kind,
        ) {
            return self.render_status_line_spec_via_backend(
                &spec,
                Some(matrix_row),
                builder,
                on_progress,
            );
        }
        None
    }

    fn resolved_status_line_face_at_string_byte(
        face_resolver: &FaceResolver,
        base_face: &ResolvedFace,
        props: &TextPropertyTable,
        bytepos: usize,
    ) -> ResolvedFace {
        let mut face = base_face.clone();
        if let Some(value) = props.get_property(bytepos, "face")
            && let Some(next) = face_resolver.resolve_face_value_over(&face, value)
        {
            face = next;
        }
        if let Some(value) = props.get_property(bytepos, "font-lock-face")
            && let Some(next) = face_resolver.resolve_face_value_over(&face, value)
        {
            face = next;
        }
        face
    }

    pub(crate) fn build_rust_status_line_spec(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        window_id: i64,
        char_w: f32,
        ascent: f32,
        next_face_id: &mut u32,
        base_face: &ResolvedFace,
        rendered: Value,
        face_resolver: &FaceResolver,
        kind: StatusLineKind,
    ) -> Option<StatusLineSpec> {
        let text = rendered.as_str_owned()?;
        let base_face_id = *next_face_id;
        *next_face_id += 1;
        let face = self.realize_status_line_face(base_face_id, base_face, char_w, ascent, height);
        let char_width = self.status_line_char_width(&face, char_w);
        let mut spec = StatusLineSpec::plain(
            kind, x, y, width, height, window_id, char_width, ascent, face, text,
        );

        if !rendered.is_string() {
            return Some(spec);
        }
        let Some(props) = get_string_text_properties_table_for_value(rendered) else {
            return Some(spec);
        };

        let mut boundaries = vec![0usize];
        for interval in props.intervals_snapshot() {
            if interval.properties.contains_key("face")
                || interval.properties.contains_key("font-lock-face")
            {
                boundaries.push(interval.start);
                boundaries.push(interval.end);
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();

        let mut current_face = base_face.clone();
        for boundary in boundaries {
            if boundary >= spec.text.len() {
                continue;
            }
            let resolved = Self::resolved_status_line_face_at_string_byte(
                face_resolver,
                base_face,
                &props,
                boundary,
            );
            if same_resolved_face(&resolved, &current_face) {
                continue;
            }

            let (face_id, run_face) = if same_resolved_face(&resolved, base_face) {
                (spec.face.face_id, None)
            } else {
                let face_id = *next_face_id;
                *next_face_id += 1;
                let run_face =
                    self.realize_status_line_face(face_id, &resolved, char_w, ascent, height);
                (face_id, Some(run_face))
            };

            spec.face_runs.push(OverlayFaceRun {
                byte_offset: boundary as u16,
                fg: resolved.fg,
                bg: resolved.bg,
                extend: resolved.extend,
                face_id,
            });
            if let Some(run_face) = run_face {
                spec.run_faces.insert(face_id, run_face);
            }
            current_face = resolved;
        }

        // ----- Display property harvesting for (space :width N) and
        // (space :align-to E) -----
        //
        // Mirrors GNU's handle_display_prop → produce_stretch_glyph
        // chain (xdisp.c:5858 → 32510): when the walker encounters a
        // string byte whose `display` text property is a `(space …)`
        // spec, it emits a stretch glyph of the evaluated width/
        // position. For neomacs, we evaluate the spec here at harvest
        // time and feed the result into the existing align_entries /
        // display_props buffers that the render loop already knows
        // how to consume (status_line.rs:651+).
        //
        // The older harvester at lines 803-812 intentionally only
        // scanned for `face` and `font-lock-face` — `display` was
        // silently dropped, which caused doom-modeline's
        // `(space :align-to (- right rhs-width))` to collapse to zero
        // width and the mode-line to render without right-aligned
        // content. Step 2 of the unification plan landed the
        // calc_pixel_width_or_height port; this harvester extension
        // is what actually wires it into the mode-line path.
        //
        // Coordinate system: the align_entries/display_props fields
        // on StatusLineSpec use status-line-local pixel offsets
        // (0 = left edge of the status line). We build a
        // PixelCalcContext where `text_area_*` reflect the status
        // line's own width, so a form like `(- right 200)` resolves
        // to `spec.width - 200` in the same coordinate system.
        use crate::display_pixel_calc::{PixelCalcContext, calc_pixel_width_or_height};
        let pctx = PixelCalcContext {
            frame_column_width: char_width as f64,
            frame_line_height: height as f64,
            frame_res_x: 96.0,
            frame_res_y: 96.0,
            face_font_height: height as f64,
            face_font_width: char_width as f64,
            text_area_left: 0.0,
            text_area_right: width as f64,
            text_area_width: width as f64,
            left_margin_left: 0.0,
            left_margin_width: 0.0,
            right_margin_left: width as f64,
            right_margin_width: 0.0,
            left_fringe_width: 0.0,
            right_fringe_width: 0.0,
            fringes_outside_margins: false,
            scroll_bar_width: 0.0,
            scroll_bar_on_left: false,
            line_number_pixel_width: 0.0,
        };

        for interval in props.intervals_snapshot() {
            let Some(disp_prop) = interval.properties.get("display") else {
                continue;
            };
            // Only handle (space …) specs here. Other display values
            // (strings, images, margin specs) follow their own paths
            // that status_line.rs does not yet support; they remain
            // TODO for future commits.
            if !disp_prop.is_cons() {
                continue;
            }
            if !disp_prop.cons_car().is_symbol_named("space") {
                continue;
            }
            let Some(items) = neovm_core::emacs_core::value::list_to_vec(disp_prop) else {
                continue;
            };
            // items[0] = 'space; walk the keyword-value plist.
            let mut i = 1usize;
            let mut done = false;
            while i + 1 < items.len() && !done {
                let key = items[i];
                let val = items[i + 1];
                if key.is_symbol_named(":width") {
                    if let Some(pixels) = calc_pixel_width_or_height(&pctx, &val, true, None) {
                        let byte_offset =
                            (interval.start as u16).min(spec.text.len().saturating_sub(1) as u16);
                        spec.display_props.push(DisplayPropRecord {
                            byte_offset,
                            covers_bytes: interval.end.saturating_sub(interval.start) as u16,
                            gpu_id: 0,
                            width: (pixels as u16).max(0),
                            height: 0,
                            ascent: 0,
                        });
                        done = true;
                    }
                } else if key.is_symbol_named(":align-to") {
                    let mut align_to: i32 = -1;
                    if let Some(pixels) =
                        calc_pixel_width_or_height(&pctx, &val, true, Some(&mut align_to))
                    {
                        // See the buffer-text analogue in
                        // engine.rs::eval_display_space_as_width for
                        // the same shape of post-processing: if the
                        // expression contained a window-box symbol
                        // (`right`, `text`, …) it resolved a base
                        // position and `pixels` is the offset from it;
                        // otherwise `pixels` is a column-relative
                        // offset and the caller adds content_x. For
                        // status-line-local coordinates, content_x is
                        // 0.
                        let target_x = if align_to >= 0 {
                            align_to as f32 + pixels as f32
                        } else {
                            pixels as f32
                        };
                        let byte_offset =
                            (interval.start as u16).min(spec.text.len().saturating_sub(1) as u16);
                        spec.align_entries.push(OverlayAlignEntry {
                            byte_offset,
                            align_to_px: target_x,
                        });
                        done = true;
                    }
                }
                i += 2;
            }
        }

        // The render loop expects align_entries and display_props
        // sorted by byte_offset so its single-pass walker can advance
        // through them in order.
        spec.align_entries.sort_by_key(|e| e.byte_offset);
        spec.display_props.sort_by_key(|d| d.byte_offset);

        Some(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // Helper: build a 14-byte face run record (native-endian)
    // ---------------------------------------------------------------
    fn make_run_bytes(byte_offset: u16, fg: u32, bg: u32) -> [u8; 14] {
        let mut rec = [0u8; 14];
        rec[0..2].copy_from_slice(&byte_offset.to_ne_bytes());
        rec[2..6].copy_from_slice(&fg.to_ne_bytes());
        rec[6..10].copy_from_slice(&bg.to_ne_bytes());
        // face_id defaults to 0
        rec
    }

    // ---------------------------------------------------------------
    // StatusLineKind enum
    // ---------------------------------------------------------------

    #[test]
    fn status_line_kind_variants_exist() {
        // Ensure all variants can be constructed (compile-time check
        // made explicit).
        let _ml = StatusLineKind::ModeLine;
        let _hl = StatusLineKind::HeaderLine;
        let _tl = StatusLineKind::TabLine;
    }

    #[test]
    fn status_line_kind_is_distinct() {
        // Discriminants should differ (match each variant).
        let check = |k: &StatusLineKind| -> u8 {
            match k {
                StatusLineKind::ModeLine => 0,
                StatusLineKind::HeaderLine => 1,
                StatusLineKind::TabLine => 2,
            }
        };
        assert_eq!(check(&StatusLineKind::ModeLine), 0);
        assert_eq!(check(&StatusLineKind::HeaderLine), 1);
        assert_eq!(check(&StatusLineKind::TabLine), 2);
    }

    // ---------------------------------------------------------------
    // OverlayFaceRun struct
    // ---------------------------------------------------------------

    #[test]
    fn overlay_face_run_construction_defaults() {
        let run = OverlayFaceRun {
            byte_offset: 0,
            fg: 0,
            bg: 0,
            extend: false,
            face_id: 0,
        };
        assert_eq!(run.byte_offset, 0);
        assert_eq!(run.fg, 0);
        assert_eq!(run.bg, 0);
        assert_eq!(run.extend, false);
    }

    #[test]
    fn overlay_face_run_construction_max_values() {
        let run = OverlayFaceRun {
            byte_offset: u16::MAX,
            fg: u32::MAX,
            bg: u32::MAX,
            extend: true,
            face_id: 0,
        };
        assert_eq!(run.byte_offset, u16::MAX);
        assert_eq!(run.fg, u32::MAX);
        assert_eq!(run.bg, u32::MAX);
        assert_eq!(run.extend, true);
    }

    #[test]
    fn overlay_face_run_construction_typical() {
        // Typical Emacs color values: 0x00RRGGBB
        let run = OverlayFaceRun {
            byte_offset: 42,
            fg: 0x00FFFFFF,
            bg: 0x00000000,
            extend: false,
            face_id: 0,
        };
        assert_eq!(run.byte_offset, 42);
        assert_eq!(run.fg, 0x00FFFFFF);
        assert_eq!(run.bg, 0x00000000);
        assert_eq!(run.extend, false);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: empty / zero
    // ---------------------------------------------------------------

    #[test]
    fn parse_empty_buffer_zero_runs() {
        let buf: &[u8] = &[];
        let runs = parse_overlay_face_runs(buf, 0, 0);
        assert!(runs.is_empty());
    }

    #[test]
    fn parse_zero_runs_with_text() {
        // Buffer has text but no face runs requested.
        let buf = b"Hello, world!";
        let runs = parse_overlay_face_runs(buf, buf.len(), 0);
        assert!(runs.is_empty());
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: single run
    // ---------------------------------------------------------------

    #[test]
    fn parse_single_run() {
        let text = b"Hello";
        let text_len = text.len(); // 5
        let rec = make_run_bytes(0, 0x00FF0000, 0x0000FF00);

        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&rec);

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].byte_offset, 0);
        assert_eq!(runs[0].fg, 0x00FF0000);
        assert_eq!(runs[0].bg, 0x0000FF00);
    }

    #[test]
    fn parse_single_run_nonzero_offset() {
        let text = b"ABCDEF";
        let text_len = text.len(); // 6
        // Use 24-bit bg (realistic sRGB). Bit 31 = 0 → extend = false.
        let rec = make_run_bytes(3, 0xAABBCCDD, 0x00223344);

        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&rec);

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].byte_offset, 3);
        assert_eq!(runs[0].fg, 0xAABBCCDD);
        assert_eq!(runs[0].bg, 0x00223344);
        assert_eq!(runs[0].extend, false);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: multiple runs
    // ---------------------------------------------------------------

    #[test]
    fn parse_multiple_runs() {
        let text = b"mode-line text here";
        let text_len = text.len();

        let r0 = make_run_bytes(0, 0x00FFFFFF, 0x00000000);
        let r1 = make_run_bytes(10, 0x0000FF00, 0x00FF0000);
        let r2 = make_run_bytes(15, 0x000000FF, 0x00FFFF00);

        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&r0);
        buf.extend_from_slice(&r1);
        buf.extend_from_slice(&r2);

        let runs = parse_overlay_face_runs(&buf, text_len, 3);
        assert_eq!(runs.len(), 3);

        assert_eq!(runs[0].byte_offset, 0);
        assert_eq!(runs[0].fg, 0x00FFFFFF);
        assert_eq!(runs[0].bg, 0x00000000);

        assert_eq!(runs[1].byte_offset, 10);
        assert_eq!(runs[1].fg, 0x0000FF00);
        assert_eq!(runs[1].bg, 0x00FF0000);

        assert_eq!(runs[2].byte_offset, 15);
        assert_eq!(runs[2].fg, 0x000000FF);
        assert_eq!(runs[2].bg, 0x00FFFF00);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: truncated data
    // ---------------------------------------------------------------

    #[test]
    fn parse_truncated_single_run() {
        // Buffer has text but only 5 bytes of run data (needs 14).
        let text = b"ABC";
        let text_len = text.len();
        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&[0u8; 5]); // only half a record

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert!(runs.is_empty(), "truncated record should be skipped");
    }

    #[test]
    fn parse_truncated_second_run() {
        // First record is complete, second is truncated.
        let text = b"ABCD";
        let text_len = text.len();
        let rec0 = make_run_bytes(0, 0x11111111, 0x22222222);

        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&rec0);
        buf.extend_from_slice(&[0xFFu8; 7]); // 7 bytes, need 14

        let runs = parse_overlay_face_runs(&buf, text_len, 2);
        assert_eq!(runs.len(), 1, "only the first complete record should parse");
        assert_eq!(runs[0].fg, 0x11111111);
    }

    #[test]
    fn parse_nruns_exceeds_buffer() {
        // nruns claims 5 records but buffer only has space for 2.
        let text = b"XY";
        let text_len = text.len();
        let r0 = make_run_bytes(0, 1, 2);
        let r1 = make_run_bytes(1, 3, 4);

        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&r0);
        buf.extend_from_slice(&r1);

        let runs = parse_overlay_face_runs(&buf, text_len, 5);
        assert_eq!(runs.len(), 2, "should only parse records that fit");
        assert_eq!(runs[0].fg, 1);
        assert_eq!(runs[1].fg, 3);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: zero text_len (runs start at offset 0)
    // ---------------------------------------------------------------

    #[test]
    fn parse_zero_text_len() {
        // No text at all; runs start at offset 0 in the buffer.
        // 0xCAFEBABE has bit 31 set → extend = true, bg = lower 24 bits.
        let rec = make_run_bytes(0, 0xDEADBEEF, 0xCAFEBABE);
        let buf = Vec::from(&rec[..]);

        let runs = parse_overlay_face_runs(&buf, 0, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].fg, 0xDEADBEEF);
        assert_eq!(runs[0].bg, 0x00FEBABE); // lower 24 bits of 0xCAFEBABE
        assert_eq!(runs[0].extend, true); // bit 31 was set
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: endianness verification
    // ---------------------------------------------------------------

    #[test]
    fn parse_verifies_native_endian_u16() {
        // The u16 byte_offset is stored as native-endian bytes.
        // Build a buffer where byte_offset = 0x0102 and verify it
        // decodes correctly on the current platform.
        let expected: u16 = 0x0102;
        let rec = make_run_bytes(expected, 0, 0);
        let buf = Vec::from(&rec[..]);

        let runs = parse_overlay_face_runs(&buf, 0, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].byte_offset, expected);
    }

    #[test]
    fn parse_verifies_native_endian_u32() {
        // Similarly for u32 fg/bg.
        // Use 24-bit bg to avoid extend bit masking.
        let fg_expected: u32 = 0x01020304;
        let bg_expected: u32 = 0x00060708;
        let rec = make_run_bytes(0, fg_expected, bg_expected);
        let buf = Vec::from(&rec[..]);

        let runs = parse_overlay_face_runs(&buf, 0, 1);
        assert_eq!(runs[0].fg, fg_expected);
        assert_eq!(runs[0].bg, bg_expected);
        assert_eq!(runs[0].extend, false);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: exact boundary (off + 10 == buf.len())
    // ---------------------------------------------------------------

    #[test]
    fn parse_exact_fit() {
        // Buffer is exactly text_len + 14 bytes — the run should parse.
        let text = b"T";
        let text_len = text.len(); // 1
        let rec = make_run_bytes(0, 42, 99);
        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&rec);
        assert_eq!(buf.len(), text_len + 14);

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].fg, 42);
        assert_eq!(runs[0].bg, 99);
    }

    #[test]
    fn parse_one_byte_short() {
        // Buffer is text_len + 13 bytes — one byte short, run should NOT parse.
        let text = b"T";
        let text_len = text.len();
        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&[0u8; 13]);
        assert_eq!(buf.len(), text_len + 13);

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert!(runs.is_empty());
    }

    // ---------------------------------------------------------------
    // apply_overlay_face_run: basic advancement
    // ---------------------------------------------------------------

    #[test]
    fn apply_overlay_single_run_before_offset() {
        // byte_idx < run.byte_offset  =>  no face change, cr unchanged.
        let runs = vec![OverlayFaceRun {
            byte_offset: 5,
            fg: 0x00FF0000,
            bg: 0x00000000,
            extend: false,
            face_id: 0,
        }];
        // byte_idx = 0, which is < 5
        let cr = apply_overlay_face_run(&runs, 0, 0);
        // Since byte_idx (0) < runs[0].byte_offset (5), the condition at
        // line 57 (`byte_idx >= runs[cr].byte_offset`) is false,
        // so the function just returns cr unchanged.
        assert_eq!(cr, 0);
    }

    #[test]
    fn apply_overlay_single_run_at_offset() {
        // byte_idx == run.byte_offset  =>  face applied, cr stays 0.
        let runs = vec![OverlayFaceRun {
            byte_offset: 5,
            fg: 0x00FF0000,
            bg: 0x0000FF00,
            extend: false,
            face_id: 0,
        }];
        let cr = apply_overlay_face_run(&runs, 5, 0);
        assert_eq!(cr, 0);
    }

    #[test]
    fn apply_overlay_single_run_past_offset() {
        let runs = vec![OverlayFaceRun {
            byte_offset: 5,
            fg: 0x00FF0000,
            bg: 0x0000FF00,
            extend: false,
            face_id: 0,
        }];
        let cr = apply_overlay_face_run(&runs, 10, 0);
        assert_eq!(cr, 0);
    }

    #[test]
    fn apply_overlay_multiple_runs_advance() {
        let runs = vec![
            OverlayFaceRun {
                byte_offset: 0,
                fg: 0x00FF0000,
                bg: 0x00000000,
                extend: false,
                face_id: 0,
            },
            OverlayFaceRun {
                byte_offset: 5,
                fg: 0x0000FF00,
                bg: 0x00000000,
                extend: false,
                face_id: 0,
            },
            OverlayFaceRun {
                byte_offset: 10,
                fg: 0x000000FF,
                bg: 0x00000000,
                extend: false,
                face_id: 0,
            },
        ];
        // byte_idx=0 => should stay at run 0
        let cr = apply_overlay_face_run(&runs, 0, 0);
        assert_eq!(cr, 0);

        // byte_idx=5 => should advance to run 1
        let cr = apply_overlay_face_run(&runs, 5, 0);
        assert_eq!(cr, 1);

        // byte_idx=10 => should advance to run 2
        let cr = apply_overlay_face_run(&runs, 10, 0);
        assert_eq!(cr, 2);
    }

    #[test]
    fn apply_overlay_pre_advance_to_next_byte() {
        // Test the pre-advance logic: if byte_idx + 1 >= next run's byte_offset,
        // cr is pre-advanced.
        let runs = vec![
            OverlayFaceRun {
                byte_offset: 0,
                fg: 1,
                bg: 0,
                extend: false,
                face_id: 0,
            },
            OverlayFaceRun {
                byte_offset: 5,
                fg: 2,
                bg: 0,
                extend: false,
                face_id: 0,
            },
        ];
        // byte_idx=4, cr=0: byte_idx(4) >= runs[0].byte_offset(0) => face applied.
        // Pre-advance: byte_idx+1=5 >= runs[1].byte_offset(5) => cr becomes 1.
        let cr = apply_overlay_face_run(&runs, 4, 0);
        assert_eq!(cr, 1, "should pre-advance when byte_idx+1 reaches next run");
    }

    #[test]
    fn apply_overlay_zero_fg_bg_no_face_change() {
        // When both fg and bg are 0, no face change occurs.
        let runs = vec![OverlayFaceRun {
            byte_offset: 0,
            fg: 0,
            bg: 0,
            extend: false,
            face_id: 0,
        }];

        let cr = apply_overlay_face_run(&runs, 0, 0);
        assert_eq!(cr, 0);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: stress / many runs
    // ---------------------------------------------------------------

    #[test]
    fn parse_many_runs() {
        let text_len = 0;
        let n = 100;

        let mut buf = Vec::new();
        for i in 0..n {
            let rec = make_run_bytes(i as u16, i as u32 * 100, i as u32 * 200);
            buf.extend_from_slice(&rec);
        }

        let runs = parse_overlay_face_runs(&buf, text_len, n);
        assert_eq!(runs.len(), n as usize);

        for i in 0..n as usize {
            assert_eq!(runs[i].byte_offset, i as u16);
            assert_eq!(runs[i].fg, i as u32 * 100);
            assert_eq!(runs[i].bg, i as u32 * 200);
        }
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: large text_len offset
    // ---------------------------------------------------------------

    #[test]
    fn parse_large_text_offset() {
        // Simulate a buffer where 500 bytes are text, followed by 1 run.
        let text_len = 500;
        let mut buf = vec![0x41u8; text_len]; // 'A' * 500
        let rec = make_run_bytes(100, 0xDEAD, 0xBEEF);
        buf.extend_from_slice(&rec);

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].byte_offset, 100);
        assert_eq!(runs[0].fg, 0xDEAD);
        assert_eq!(runs[0].bg, 0xBEEF);
    }

    // ---------------------------------------------------------------
    // apply_overlay_face_run: starting from non-zero current_run
    // ---------------------------------------------------------------

    #[test]
    fn apply_overlay_start_from_middle_run() {
        let runs = vec![
            OverlayFaceRun {
                byte_offset: 0,
                fg: 1,
                bg: 0,
                extend: false,
                face_id: 0,
            },
            OverlayFaceRun {
                byte_offset: 5,
                fg: 2,
                bg: 0,
                extend: false,
                face_id: 0,
            },
            OverlayFaceRun {
                byte_offset: 10,
                fg: 3,
                bg: 0,
                extend: false,
                face_id: 0,
            },
        ];
        // Start at current_run=1, byte_idx=10 => should advance to run 2
        let cr = apply_overlay_face_run(&runs, 10, 1);
        assert_eq!(cr, 2);
    }

    #[test]
    fn apply_overlay_start_at_last_run() {
        let runs = vec![
            OverlayFaceRun {
                byte_offset: 0,
                fg: 1,
                bg: 0,
                extend: false,
                face_id: 0,
            },
            OverlayFaceRun {
                byte_offset: 5,
                fg: 2,
                bg: 0,
                extend: false,
                face_id: 0,
            },
        ];
        // Already at last run, byte_idx well past it
        let cr = apply_overlay_face_run(&runs, 100, 1);
        assert_eq!(cr, 1);
    }

    #[test]
    fn status_line_row_height_for_face_uses_realized_line_height_and_box() {
        let mut engine = LayoutEngine::new();
        let mut face = ResolvedFace::default();
        face.font_family = "monospace".to_string();
        face.font_size = 14.0;
        face.font_ascent = 9.0;
        face.font_line_height = 12.0;
        face.box_type = 1;
        face.box_line_width = 1;

        assert_eq!(
            engine.status_line_row_height_for_face(&face, 8.0, 12.0, 20.0),
            20.0
        );
    }
}
