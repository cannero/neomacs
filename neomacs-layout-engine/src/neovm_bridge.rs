//! Bridge between neovm-core data types and the layout engine.
//!
//! Provides functions to build `WindowParams` and `FrameParams` from
//! the Rust Evaluator's state, replacing C FFI data sources.

use neovm_core::buffer::Buffer;
use neovm_core::emacs_core::value::list_to_vec;
use neovm_core::emacs_core::{Evaluator, Value};
use neovm_core::face::{
    Color as NeoColor, Face as NeoFace, FaceHeight, FaceTable, FontWeight,
    UnderlineStyle as NeoUnderlineStyle,
};
use neovm_core::window::{Frame, FrameId, Window};

use super::types::{FrameParams, WindowParams};
use neomacs_display_protocol::types::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisplayLineNumbersMode {
    Off,
    Absolute,
    Relative,
    Visual,
}

pub(crate) fn buffer_local_value<'a>(buffer: &'a Buffer, name: &str) -> Option<&'a Value> {
    buffer
        .properties
        .get(name)
        .and_then(|binding| binding.as_ref())
}

/// Build `FrameParams` from a neovm-core `Frame`, reading default face
/// colors from the face table.
pub fn frame_params_from_neovm(frame: &Frame, face_table: &FaceTable) -> FrameParams {
    // Read default face background from face table
    let default_face = face_table.get("default");
    let bg = default_face
        .and_then(|f| f.background)
        .map(|c| (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32)
        .unwrap_or(0x00FFFFFF); // white fallback
    let fg = default_face
        .and_then(|f| f.foreground)
        .map(|c| (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32)
        .unwrap_or(0x00000000); // black fallback

    FrameParams {
        width: frame.width as f32,
        height: frame.height as f32,
        char_width: frame.char_width,
        char_height: frame.char_height,
        font_pixel_size: frame.font_pixel_size,
        background: bg,
        vertical_border_fg: fg,
        right_divider_width: 0,
        bottom_divider_width: 0,
        divider_fg: fg,
        divider_first_fg: fg,
        divider_last_fg: fg,
    }
}

/// Helper: extract an integer buffer-local variable.
pub(crate) fn buffer_local_int(buffer: &Buffer, name: &str, default: i64) -> i64 {
    match buffer_local_value(buffer, name) {
        Some(Value::Int(n)) => *n,
        _ => default,
    }
}

/// Helper: extract a boolean buffer-local variable (nil = false, anything else = true).
pub(crate) fn buffer_local_bool(buffer: &Buffer, name: &str) -> bool {
    match buffer_local_value(buffer, name) {
        Some(Value::Nil) | None => false,
        Some(_) => true,
    }
}

pub(crate) fn buffer_local_bool_default(buffer: &Buffer, name: &str, default: bool) -> bool {
    match buffer_local_value(buffer, name) {
        Some(Value::Nil) => false,
        Some(_) => true,
        None => default,
    }
}

pub(crate) fn buffer_local_string_owned(buffer: &Buffer, name: &str) -> Option<String> {
    buffer_local_value(buffer, name).and_then(Value::as_str_owned)
}

pub(crate) fn buffer_local_list_values(buffer: &Buffer, name: &str) -> Vec<Value> {
    buffer_local_value(buffer, name)
        .and_then(list_to_vec)
        .unwrap_or_default()
}

pub(crate) fn buffer_display_line_numbers_mode(buffer: &Buffer) -> DisplayLineNumbersMode {
    match buffer_local_value(buffer, "display-line-numbers") {
        Some(Value::True) => DisplayLineNumbersMode::Absolute,
        Some(value) if value.is_symbol_named("relative") => DisplayLineNumbersMode::Relative,
        Some(value) if value.is_symbol_named("visual") => DisplayLineNumbersMode::Visual,
        _ => DisplayLineNumbersMode::Off,
    }
}

pub(crate) fn buffer_selective_display(buffer: &Buffer) -> i32 {
    match buffer_local_value(buffer, "selective-display") {
        Some(Value::Int(n)) => *n as i32,
        Some(Value::True) => i32::MAX,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CursorSpec {
    cursor_type: u8,
    bar_width: i32,
}

fn parse_color_pixel(value: &Value) -> Option<u32> {
    value
        .as_str_owned()
        .or_else(|| value.as_symbol_name().map(str::to_string))
        .and_then(|spec| NeoColor::parse(&spec))
        .map(|color| color_to_pixel(&color))
}

fn parse_cursor_spec(value: &Value) -> Option<CursorSpec> {
    if value.is_nil() {
        return None;
    }

    if *value == Value::True || value.is_symbol_named("box") {
        return Some(CursorSpec {
            cursor_type: 0,
            bar_width: 1,
        });
    }
    if value.is_symbol_named("hollow") {
        return Some(CursorSpec {
            cursor_type: 3,
            bar_width: 1,
        });
    }
    if value.is_symbol_named("bar") {
        return Some(CursorSpec {
            cursor_type: 1,
            bar_width: 2,
        });
    }
    if value.is_symbol_named("hbar") {
        return Some(CursorSpec {
            cursor_type: 2,
            bar_width: 2,
        });
    }
    if matches!(value, Value::Cons(_)) {
        let car = value.cons_car();
        let cdr = value.cons_cdr();
        let bar_width = cdr.as_int().unwrap_or(1).max(0) as i32;
        if car.is_symbol_named("box") {
            return Some(CursorSpec {
                cursor_type: 0,
                bar_width,
            });
        }
        if car.is_symbol_named("bar") {
            return Some(CursorSpec {
                cursor_type: 1,
                bar_width,
            });
        }
        if car.is_symbol_named("hbar") {
            return Some(CursorSpec {
                cursor_type: 2,
                bar_width,
            });
        }
    }

    Some(CursorSpec {
        cursor_type: 3,
        bar_width: 1,
    })
}

fn frame_cursor_spec(frame: &Frame) -> CursorSpec {
    frame
        .parameters
        .get("cursor-type")
        .and_then(parse_cursor_spec)
        .unwrap_or(CursorSpec {
            cursor_type: 0,
            bar_width: 1,
        })
}

fn default_cursor_color_pixel(face_table: &FaceTable) -> u32 {
    face_table
        .resolve("cursor")
        .background
        .or_else(|| face_table.resolve("default").foreground)
        .map(|color| color_to_pixel(&color))
        .unwrap_or(0x000000)
}

fn frame_cursor_color_pixel(frame: &Frame, face_table: &FaceTable) -> u32 {
    frame
        .parameters
        .get("cursor-color")
        .and_then(parse_color_pixel)
        .unwrap_or_else(|| default_cursor_color_pixel(face_table))
}

fn effective_cursor_spec(
    frame: &Frame,
    buffer: &Buffer,
    is_selected: bool,
    is_minibuffer: bool,
    window_cursor_type: Value,
) -> Option<CursorSpec> {
    let base = if window_cursor_type != Value::True {
        parse_cursor_spec(&window_cursor_type)
    } else if let Some(buffer_cursor_type) = buffer_local_value(buffer, "cursor-type") {
        if *buffer_cursor_type == Value::True {
            Some(frame_cursor_spec(frame))
        } else {
            parse_cursor_spec(buffer_cursor_type)
        }
    } else {
        Some(frame_cursor_spec(frame))
    }?;

    if is_selected {
        return Some(base);
    }

    if is_minibuffer {
        return None;
    }

    let alt_cursor = buffer_local_value(buffer, "cursor-in-non-selected-windows");
    if let Some(value) = alt_cursor
        && *value != Value::True
    {
        return parse_cursor_spec(value);
    }

    let mut adjusted = base;
    match adjusted.cursor_type {
        0 => adjusted.cursor_type = 3,
        1 | 2 if adjusted.bar_width > 1 => adjusted.bar_width -= 1,
        _ => {}
    }
    Some(adjusted)
}

/// Build `WindowParams` from neovm-core window + buffer + frame data.
///
/// `is_selected` indicates whether this window is the frame's selected window.
/// `is_minibuffer` indicates whether this is the minibuffer window.
///
/// Returns `None` for internal (non-leaf) windows.
pub fn window_params_from_neovm(
    window: &Window,
    buffer: &Buffer,
    frame: &Frame,
    face_table: &FaceTable,
    is_selected: bool,
    is_minibuffer: bool,
    window_cursor_type: Value,
) -> Option<WindowParams> {
    // Only leaf windows can be laid out.
    let (win_id, _buf_id, bounds, window_start, _point, hscroll, margins, fringes) = match window {
        Window::Leaf {
            id,
            buffer_id,
            bounds,
            window_start,
            point,
            hscroll,
            margins,
            fringes,
            ..
        } => (
            *id,
            *buffer_id,
            bounds,
            *window_start,
            *point,
            *hscroll,
            *margins,
            *fringes,
        ),
        Window::Internal { .. } => return None,
    };

    let char_width = frame.char_width;
    let char_height = frame.char_height;
    let default_face = face_table.resolve("default");
    let default_fg = default_face
        .foreground
        .map(|color| color_to_pixel(&color))
        .unwrap_or(0x000000);
    let default_bg = default_face
        .background
        .map(|color| color_to_pixel(&color))
        .unwrap_or(0x00FFFFFF);

    // Convert neovm-core Rect to display Rect (same fields, different types).
    let display_bounds = Rect::new(bounds.x, bounds.y, bounds.width, bounds.height);

    // Compute text bounds (bounds minus fringes and margins).
    let left_fringe = fringes.0 as f32;
    let right_fringe = fringes.1 as f32;
    let left_margin = margins.0 as f32 * char_width;
    let right_margin = margins.1 as f32 * char_width;
    let text_x = bounds.x + left_fringe + left_margin;
    let text_width =
        (bounds.width - left_fringe - right_fringe - left_margin - right_margin).max(0.0);
    let text_bounds = Rect::new(text_x, bounds.y, text_width, bounds.height);

    // Read buffer-local variables.
    let truncate_lines = buffer_local_bool(buffer, "truncate-lines");
    let word_wrap = buffer_local_bool(buffer, "word-wrap");
    let tab_width = buffer_local_int(buffer, "tab-width", 8) as i32;

    // Mode-line: non-minibuffer windows get one line of mode-line.
    let mode_line_height = if is_minibuffer { 0.0 } else { char_height };

    let cursor_in_non_selected =
        buffer_local_bool_default(buffer, "cursor-in-non-selected-windows", true);
    let cursor_spec = effective_cursor_spec(
        frame,
        buffer,
        is_selected,
        is_minibuffer,
        window_cursor_type,
    )
    .unwrap_or(CursorSpec {
        cursor_type: 4,
        bar_width: 1,
    });
    let cursor_color = frame_cursor_color_pixel(frame, face_table);

    // Header-line: show if header-line-format is non-nil
    let header_line_height = if buffer_local_bool(buffer, "header-line-format") {
        char_height
    } else {
        0.0
    };

    // Tab-line: show if tab-line-format is non-nil
    let tab_line_height = if buffer_local_bool(buffer, "tab-line-format") {
        char_height
    } else {
        0.0
    };

    Some(WindowParams {
        window_id: win_id.0 as i64,
        buffer_id: buffer.id.0,
        bounds: display_bounds,
        text_bounds,
        selected: is_selected,
        is_minibuffer,
        // Window::window_start tracks GNU marker positions (1-based).
        // Normalize to the layout engine's internal 0-based char positions.
        window_start: window_start.saturating_sub(1) as i64,
        window_end: 0, // filled after layout
        // Use buffer.pt (authoritative point) rather than the Window's
        // cached copy, which may be stale after self-insert-command etc.
        point: buffer.pt as i64,
        buffer_size: buffer.zv as i64,
        buffer_begv: buffer.begv as i64,
        hscroll: hscroll as i32,
        vscroll: 0,
        truncate_lines,
        word_wrap,
        tab_width,
        tab_stop_list: buffer_local_list_values(buffer, "tab-stop-list")
            .iter()
            .filter_map(|v| v.as_int().map(|n| n as i32))
            .collect(),
        default_fg,
        default_bg,
        char_width,
        char_height,
        font_pixel_size: frame.font_pixel_size,
        // FIXME: 0.8 ratio is a rough heuristic.  Store actual font_ascent on
        // Frame (via FontMetricsService) when Phase 3 face/font integration lands.
        font_ascent: frame.font_pixel_size * 0.8,
        mode_line_height,
        header_line_height,
        tab_line_height,
        cursor_type: cursor_spec.cursor_type,
        cursor_bar_width: cursor_spec.bar_width,
        cursor_color,
        left_fringe_width: left_fringe,
        right_fringe_width: right_fringe,
        indicate_empty_lines: 0,
        show_trailing_whitespace: buffer_local_bool(buffer, "show-trailing-whitespace"),
        trailing_ws_bg: 0,
        fill_column_indicator: buffer_local_int(buffer, "display-fill-column-indicator-column", 0)
            as i32,
        fill_column_indicator_char: '|',
        fill_column_indicator_fg: 0,
        extra_line_spacing: match buffer_local_value(buffer, "line-spacing") {
            Some(Value::Int(n)) => *n as f32,
            Some(Value::Float(f, _)) => *f as f32,
            _ => 0.0,
        },
        cursor_in_non_selected,
        selective_display: buffer_selective_display(buffer),
        escape_glyph_fg: 0,
        nobreak_char_display: 0,
        nobreak_char_fg: 0,
        glyphless_char_fg: 0,
        wrap_prefix: Vec::new(),
        line_prefix: Vec::new(),
        left_margin_width: left_margin,
        right_margin_width: right_margin,
    })
}

/// Collect all leaf windows from a frame (including minibuffer) and build
/// `WindowParams` for each.
///
/// Returns `(FrameParams, Vec<WindowParams>)`, or `None` if the frame does
/// not exist.
pub fn collect_layout_params(
    evaluator: &Evaluator,
    frame_id: FrameId,
) -> Option<(FrameParams, Vec<WindowParams>)> {
    let frame = evaluator.frame_manager().get(frame_id)?;
    let frame_params = frame_params_from_neovm(frame, evaluator.face_table());

    let mut window_params = Vec::new();

    // Collect leaf windows from the root window tree.
    let leaf_ids = frame.root_window.leaf_ids();
    for win_id in &leaf_ids {
        let Some(window) = frame.root_window.find(*win_id) else {
            continue;
        };
        let Some(buf_id) = window.buffer_id() else {
            continue;
        };
        let Some(buffer) = evaluator.buffer_manager().get(buf_id) else {
            continue;
        };
        let is_selected = frame.selected_window == *win_id;
        let window_cursor_type = evaluator.frame_manager().window_cursor_type(*win_id);
        if let Some(wp) = window_params_from_neovm(
            window,
            buffer,
            frame,
            evaluator.face_table(),
            is_selected,
            false,
            window_cursor_type,
        ) {
            tracing::debug!(
                "layout window cursor: win={} selected={} minibuffer=false type={} width={} color=#{:06x} window-cursor-type={:?}",
                wp.window_id,
                wp.selected,
                wp.cursor_type,
                wp.cursor_bar_width,
                wp.cursor_color,
                window_cursor_type,
            );
            window_params.push(wp);
        }
    }

    // Add minibuffer window if present.
    if let Some(mini_leaf) = &frame.minibuffer_leaf {
        let buf_id = mini_leaf.buffer_id();
        let buffer = buf_id.and_then(|id| evaluator.buffer_manager().get(id));
        if let Some(buffer) = buffer {
            let is_selected = frame.selected_window == mini_leaf.id();
            let window_cursor_type = evaluator.frame_manager().window_cursor_type(mini_leaf.id());
            if let Some(wp) = window_params_from_neovm(
                mini_leaf,
                buffer,
                frame,
                evaluator.face_table(),
                is_selected,
                true,
                window_cursor_type,
            ) {
                tracing::debug!(
                    "layout window cursor: win={} selected={} minibuffer=true type={} width={} color=#{:06x} window-cursor-type={:?}",
                    wp.window_id,
                    wp.selected,
                    wp.cursor_type,
                    wp.cursor_bar_width,
                    wp.cursor_color,
                    window_cursor_type,
                );
                window_params.push(wp);
            }
        }
    }

    Some((frame_params, window_params))
}

/// Buffer accessor for the layout engine.
///
/// Wraps a reference to a neovm-core `Buffer` and provides the operations
/// that the layout engine needs: text byte copying, position conversion,
/// and line counting.
pub struct RustBufferAccess<'a> {
    buffer: &'a Buffer,
}

impl<'a> RustBufferAccess<'a> {
    /// Create a new buffer accessor.
    pub fn new(buffer: &'a Buffer) -> Self {
        Self { buffer }
    }

    /// Convert an internal neovm buffer character position to a byte position.
    ///
    /// `WindowParams` used by the pure-Rust layout path carry neovm-core's
    /// internal character positions, which are 0-based and use an exclusive
    /// end (`zv_char` / `buffer_size`).
    pub fn charpos_to_bytepos(&self, charpos: i64) -> i64 {
        if charpos <= 0 {
            return 0;
        }
        self.buffer.text.char_to_byte(charpos as usize) as i64
    }

    /// Convert a GNU Lisp-visible buffer position to a byte position.
    ///
    /// GNU Lisp positions are 1-based, so this is only appropriate for
    /// values coming from Lisp APIs such as `minibuffer-prompt-end`.
    pub fn lisp_charpos_to_bytepos(&self, charpos: i64) -> i64 {
        if charpos <= 1 {
            return 0;
        }
        self.buffer.text.char_to_byte((charpos - 1) as usize) as i64
    }

    /// Copy buffer text bytes in the range `[byte_from, byte_to)` into `out`.
    ///
    /// Uses the efficient `copy_bytes_to` method on the gap buffer.
    pub fn copy_text(&self, byte_from: i64, byte_to: i64, out: &mut Vec<u8>) {
        let from = (byte_from as usize).min(self.buffer.text.len());
        let to = (byte_to as usize).min(self.buffer.text.len());
        if from >= to {
            out.clear();
            return;
        }
        self.buffer.text.copy_bytes_to(from, to, out);
    }

    /// Count the number of newlines in `[byte_from, byte_to)`.
    ///
    /// Used for line number display.
    pub fn count_lines(&self, byte_from: i64, byte_to: i64) -> i64 {
        let from = (byte_from as usize).min(self.buffer.text.len());
        let to = (byte_to as usize).min(self.buffer.text.len());
        if from >= to {
            return 0;
        }
        // Count newlines by iterating byte by byte
        let mut count: i64 = 0;
        for pos in from..to {
            if self.buffer.text.byte_at(pos) == b'\n' {
                count += 1;
            }
        }
        count
    }

    /// Read a single byte at the given byte position.
    ///
    /// Returns `None` if the position is out of bounds.
    pub fn byte_at(&self, byte_pos: i64) -> Option<u8> {
        if byte_pos < 0 {
            return None;
        }
        let pos = byte_pos as usize;
        if pos < self.buffer.text.len() {
            Some(self.buffer.text.byte_at(pos))
        } else {
            None
        }
    }

    /// Get the buffer's narrowed beginning (begv) as byte position.
    pub fn begv(&self) -> i64 {
        self.buffer.begv as i64
    }

    /// Get the buffer's narrowed end (zv) as byte position.
    pub fn zv(&self) -> i64 {
        self.buffer.zv as i64
    }

    /// Get point (cursor) byte position.
    pub fn point(&self) -> i64 {
        self.buffer.pt as i64
    }

    /// Whether the buffer has been modified.
    pub fn modified(&self) -> bool {
        self.buffer.modified
    }

    /// Buffer name.
    pub fn name(&self) -> &str {
        &self.buffer.name
    }

    /// Buffer file name, if any.
    pub fn file_name(&self) -> Option<&str> {
        self.buffer.file_name.as_deref()
    }

    /// Get the underlying neovm-core Buffer reference (for text property
    /// and overlay access in later tasks).
    pub fn inner(&self) -> &'a Buffer {
        self.buffer
    }
}

/// Text property and overlay accessor for the layout engine.
///
/// Wraps a reference to a neovm-core `Buffer` and provides query methods
/// for invisible text, display properties, overlay strings, and other
/// text property-based features.
pub struct RustTextPropAccess<'a> {
    buffer: &'a Buffer,
}

impl<'a> RustTextPropAccess<'a> {
    /// Create a new text property accessor.
    pub fn new(buffer: &'a Buffer) -> Self {
        Self { buffer }
    }

    /// Check if text at `charpos` is invisible.
    ///
    /// Returns `(is_invisible, next_visible_pos)`.
    /// `next_visible_pos` is the next char position where visibility might change.
    /// If no change is found, returns `buffer.zv` as the next boundary.
    pub fn check_invisible(&self, charpos: i64) -> (bool, i64) {
        let pos = charpos as usize;
        let invis = self.buffer.text_props.get_property(pos, "invisible");

        let is_invisible = match invis {
            Some(Value::Nil) | None => false,
            Some(_) => true, // Any non-nil value means invisible
        };

        // Find the next position where the invisible property changes
        let next_change = self
            .buffer
            .text_props
            .next_property_change(pos)
            .unwrap_or(self.buffer.zv);

        (is_invisible, next_change as i64)
    }

    /// Check for a display text property at `charpos`.
    ///
    /// Returns the display property value if present, along with the
    /// next position where display properties change.
    pub fn check_display_prop(&self, charpos: i64) -> (Option<&Value>, i64) {
        let pos = charpos as usize;
        let display = self.buffer.text_props.get_property(pos, "display");

        let next_change = self
            .buffer
            .text_props
            .next_property_change(pos)
            .unwrap_or(self.buffer.zv);

        (display, next_change as i64)
    }

    /// Check for line-spacing text property at `charpos`.
    ///
    /// Returns extra line spacing in pixels (0.0 if no property).
    pub fn check_line_spacing(&self, charpos: i64, base_height: f32) -> f32 {
        let pos = charpos as usize;
        match self.buffer.text_props.get_property(pos, "line-spacing") {
            Some(Value::Int(n)) => *n as f32,
            Some(Value::Float(f, _)) => {
                if *f < 1.0 {
                    // Fraction of base height
                    base_height * (*f as f32)
                } else {
                    *f as f32
                }
            }
            _ => 0.0,
        }
    }

    /// Collect overlay before-string and after-string at `charpos`.
    ///
    /// Before-strings come from overlays starting at charpos.
    /// After-strings come from overlays ending at charpos.
    ///
    /// Returns `(before_strings, after_strings)` where each is a Vec of
    /// (string_bytes, overlay_id) pairs.
    pub fn overlay_strings_at(&self, charpos: i64) -> (Vec<(Vec<u8>, u64)>, Vec<(Vec<u8>, u64)>) {
        let pos = charpos as usize;
        let mut before = Vec::new();
        let mut after = Vec::new();

        // Get all overlays covering this position
        let overlay_ids = self.buffer.overlays.overlays_at(pos);
        for oid in &overlay_ids {
            let oid = *oid;
            // Before-string: from overlays that START at this position
            if let Some(start) = self.buffer.overlays.overlay_start(oid) {
                if start == pos {
                    if let Some(val) = self.buffer.overlays.overlay_get(oid, "before-string") {
                        if let Some(s) = value_as_string(val) {
                            before.push((s.as_bytes().to_vec(), oid));
                        }
                    }
                }
            }

            // After-string: from overlays that END at this position
            if let Some(end) = self.buffer.overlays.overlay_end(oid) {
                if end == pos {
                    if let Some(val) = self.buffer.overlays.overlay_get(oid, "after-string") {
                        if let Some(s) = value_as_string(val) {
                            after.push((s.as_bytes().to_vec(), oid));
                        }
                    }
                }
            }
        }

        // Also check overlays_in for overlays that end exactly at this position
        // (overlays_at only returns overlays that CONTAIN pos, not those ending at pos)
        // The range [pos, pos+1) covers overlays ending at pos
        // Actually, overlays_at covers [start, end) so overlays ending at pos won't be included.
        // We need a broader search for after-strings.
        if pos > 0 {
            let nearby_ids = self
                .buffer
                .overlays
                .overlays_in(pos.saturating_sub(1), pos + 1);
            for oid in &nearby_ids {
                let oid = *oid;
                if let Some(end) = self.buffer.overlays.overlay_end(oid) {
                    if end == pos {
                        // Check we haven't already processed this overlay
                        if !overlay_ids.contains(&oid) {
                            if let Some(val) = self.buffer.overlays.overlay_get(oid, "after-string")
                            {
                                if let Some(s) = value_as_string(val) {
                                    after.push((s.as_bytes().to_vec(), oid));
                                }
                            }
                        }
                    }
                }
            }
        }

        (before, after)
    }

    /// Get the next position where any text property changes.
    ///
    /// This is useful for the layout engine's "next_check" optimization
    /// to avoid per-character property lookups.
    pub fn next_property_change(&self, charpos: i64) -> i64 {
        let pos = charpos as usize;
        self.buffer
            .text_props
            .next_property_change(pos)
            .unwrap_or(self.buffer.zv) as i64
    }

    /// Get a specific text property at a position.
    pub fn get_property(&self, charpos: i64, name: &str) -> Option<&Value> {
        let pos = charpos as usize;
        self.buffer.text_props.get_property(pos, name)
    }

    /// Get a text property at `charpos` as a string.
    ///
    /// Returns `Some(String)` if the property exists and is a string value,
    /// `None` otherwise.
    pub fn get_text_prop_string(&self, charpos: i64, prop_name: &str) -> Option<String> {
        self.get_property(charpos, prop_name)
            .and_then(|v| v.as_str_owned())
    }

    /// Get the underlying neovm-core Buffer reference.
    pub fn inner(&self) -> &'a Buffer {
        self.buffer
    }
}

/// Helper: extract a string from a Value.
///
/// For `Value::Str`, resolves through the heap to get the string content.
/// For other Value types, returns None.
fn value_as_string(val: &Value) -> Option<String> {
    // Value::Str uses ObjId -- need to resolve through the heap.
    // For now, use the display format as a fallback.
    // TODO: When the heap is accessible, use with_heap(|h| h.get_str(id))
    match val {
        Value::Nil => None,
        _ => {
            // Try to get the string representation.
            // This is a temporary approach -- proper string extraction
            // needs heap access which isn't available through a &Buffer reference.
            // For overlay/text prop strings, they're typically stored as
            // interned symbols or heap strings.
            None // TODO: implement proper string extraction with heap access
        }
    }
}

// ---------------------------------------------------------------------------
// ResolvedFace — pure-Rust equivalent of FaceDataFFI
// ---------------------------------------------------------------------------

/// Convert a neovm-core `Color` to a packed sRGB pixel (0x00RRGGBB).
fn color_to_pixel(c: &NeoColor) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | (c.b as u32)
}

/// Resolved face attributes ready for the layout engine.
///
/// This is the neovm-core equivalent of `FaceDataFFI`.  All attributes are
/// fully realized (no `Option`s) so the layout engine can use them directly.
#[derive(Clone, Debug)]
pub struct ResolvedFace {
    /// Foreground color (sRGB pixel: 0x00RRGGBB).
    pub fg: u32,
    /// Background color (sRGB pixel: 0x00RRGGBB).
    pub bg: u32,
    /// Font family name.
    pub font_family: String,
    /// Font weight (CSS 100-900).
    pub font_weight: u16,
    /// Italic flag.
    pub italic: bool,
    /// Font size in pixels.
    pub font_size: f32,
    /// Underline style (0=none, 1=line, 2=wave, 3=double, 4=dotted, 5=dashed).
    pub underline_style: u8,
    /// Underline color (sRGB pixel, 0 = use foreground).
    pub underline_color: u32,
    /// Strike-through enabled.
    pub strike_through: bool,
    /// Strike-through color (sRGB pixel, 0 = use foreground).
    pub strike_through_color: u32,
    /// Overline enabled.
    pub overline: bool,
    /// Overline color (sRGB pixel, 0 = use foreground).
    pub overline_color: u32,
    /// Box type (0=none, 1=line).
    pub box_type: u8,
    /// Box color (sRGB pixel).
    pub box_color: u32,
    /// Box line width.
    pub box_line_width: i32,
    /// Extend background to end of line.
    pub extend: bool,
    /// Simulate bold by drawing twice at x and x+1.
    pub overstrike: bool,
    /// Per-face character advance width (from FontMetricsService, 0.0 = use default).
    pub font_char_width: f32,
    /// Per-face font ascent (from FontMetricsService, 0.0 = use default).
    pub font_ascent: f32,
    /// Per-face line height (from FontMetricsService, 0.0 = use default).
    pub font_line_height: f32,
}

impl Default for ResolvedFace {
    fn default() -> Self {
        Self {
            fg: 0x00000000,
            bg: 0x00FFFFFF,
            font_family: String::new(),
            font_weight: 400,
            italic: false,
            font_size: 14.0,
            underline_style: 0,
            underline_color: 0,
            strike_through: false,
            strike_through_color: 0,
            overline: false,
            overline_color: 0,
            box_type: 0,
            box_color: 0,
            box_line_width: 0,
            extend: false,
            overstrike: false,
            font_char_width: 0.0,
            font_ascent: 0.0,
            font_line_height: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// FaceResolver
// ---------------------------------------------------------------------------

/// Resolves face attributes at buffer positions using the neovm-core
/// `FaceTable`, text properties, and overlays.
///
/// Replaces the C FFI `face_at_buffer_position()` path for the pure-Rust
/// backend.
pub struct FaceResolver {
    face_table: FaceTable,
    default_face: ResolvedFace,
}

impl FaceResolver {
    /// Create a new `FaceResolver`.
    ///
    /// Clones the `FaceTable` so the resolver owns its data and does not
    /// borrow from the `Evaluator`.  This allows `layout_window_rust` to
    /// take `&mut Evaluator` for `format-mode-line` evaluation while
    /// still using the `FaceResolver`.
    pub fn new(
        face_table: &FaceTable,
        default_fg: u32,
        default_bg: u32,
        default_font_size: f32,
    ) -> Self {
        let neo_default = face_table.resolve("default");
        let mut df = ResolvedFace::default();
        df.fg = neo_default
            .foreground
            .as_ref()
            .map(color_to_pixel)
            .unwrap_or(default_fg);
        df.bg = neo_default
            .background
            .as_ref()
            .map(color_to_pixel)
            .unwrap_or(default_bg);
        df.font_family = neo_default.family.clone().unwrap_or_default();
        df.font_weight = neo_default
            .weight
            .map(|w| w.0)
            .unwrap_or(FontWeight::NORMAL.0);
        df.italic = neo_default.slant.map(|s| s.is_italic()).unwrap_or(false);
        df.font_size = match &neo_default.height {
            Some(FaceHeight::Absolute(tenths)) => *tenths as f32 / 10.0 * (96.0 / 72.0),
            _ => default_font_size,
        };
        df.extend = neo_default.extend.unwrap_or(false);
        df.overstrike = neo_default.overstrike;

        // Underline
        if let Some(ul) = &neo_default.underline {
            df.underline_style = underline_style_to_u8(&ul.style);
            df.underline_color = ul.color.as_ref().map(color_to_pixel).unwrap_or(0);
        }
        // Overline
        if neo_default.overline == Some(true) {
            df.overline = true;
        }
        // Strike-through
        if neo_default.strike_through == Some(true) {
            df.strike_through = true;
        }
        // Box
        if let Some(bb) = &neo_default.box_border {
            df.box_type = 1;
            df.box_color = bb.color.as_ref().map(color_to_pixel).unwrap_or(0);
            df.box_line_width = bb.width;
        }

        Self {
            face_table: face_table.clone(),
            default_face: df,
        }
    }

    /// Return a reference to the resolved default face.
    pub fn default_face(&self) -> &ResolvedFace {
        &self.default_face
    }

    /// Resolve a named face from the face table.
    ///
    /// Looks up the named face, resolves inheritance, and realizes all
    /// attributes against the default face.  Returns the default face
    /// if the name is not found.
    pub fn resolve_named_face(&self, name: &str) -> ResolvedFace {
        let face = self.face_table.resolve(name);
        self.realize_face(&face)
    }

    /// Resolve face attributes at a buffer position.
    ///
    /// Reads "face" and "font-lock-face" text properties, collects overlay
    /// faces (sorted by priority), merges them via `FaceTable`, and produces
    /// a fully-realized `ResolvedFace`.
    ///
    /// `next_check` is set to the minimum of all property change positions
    /// so the caller can skip per-character lookups until that boundary.
    pub fn face_at_pos(
        &self,
        buffer: &Buffer,
        charpos: usize,
        next_check: &mut usize,
    ) -> ResolvedFace {
        let mut face_names: Vec<String> = Vec::new();
        let mut min_next = buffer.zv;

        // 1. "face" text property
        let mut plist_face: Option<NeoFace> = None;
        if let Some(val) = buffer.text_props.get_property(charpos, "face") {
            let names = Self::resolve_face_value(val);
            if names.len() == 1 && names[0] == "--plist-face--" {
                // Inline plist face — parse directly into a Face object.
                plist_face = Self::face_from_plist(val);
            } else {
                face_names.extend(names);
            }
        }
        // Update next_check from text property boundaries
        if let Some(nc) = buffer.text_props.next_property_change(charpos) {
            min_next = min_next.min(nc);
        }

        // 2. "font-lock-face" text property
        if let Some(val) = buffer.text_props.get_property(charpos, "font-lock-face") {
            let names = Self::resolve_face_value(val);
            face_names.extend(names);
        }

        // 3. Overlay faces (sorted by priority, lowest first)
        let overlay_ids = buffer.overlays.overlays_at(charpos);
        if !overlay_ids.is_empty() {
            // Collect (priority, face_names) pairs
            let mut overlay_faces: Vec<(i64, Vec<String>)> = Vec::new();
            for oid in &overlay_ids {
                let oid = *oid;
                // Update next_check from overlay boundaries
                if let Some(end) = buffer.overlays.overlay_end(oid) {
                    if end > charpos {
                        min_next = min_next.min(end);
                    }
                }
                // Get priority (default 0)
                let priority = buffer
                    .overlays
                    .overlay_get(oid, "priority")
                    .and_then(|v| v.as_int())
                    .unwrap_or(0);
                // Get face
                if let Some(val) = buffer.overlays.overlay_get(oid, "face") {
                    let names = Self::resolve_face_value(val);
                    if !names.is_empty() {
                        overlay_faces.push((priority, names));
                    }
                }
            }
            // Sort by priority (ascending), so higher priority overlays
            // are merged later and override earlier ones.
            overlay_faces.sort_by_key(|(pri, _)| *pri);
            for (_pri, names) in overlay_faces {
                face_names.extend(names);
            }
        }

        // Also consider overlay boundaries so next_check doesn't skip past
        // positions where an overlay starts or ends.
        if let Some(nb) = buffer.overlays.next_boundary_after(charpos) {
            min_next = min_next.min(nb);
        }

        *next_check = min_next;

        // 4. If we have a plist face (and no other faces), realize it directly.
        if let Some(pface) = plist_face {
            if face_names.is_empty() {
                return self.realize_face(&pface);
            }
            // Merge named faces first, then overlay the plist attributes.
            let name_refs: Vec<&str> = face_names.iter().map(|s| s.as_str()).collect();
            let merged = self.face_table.merge_faces(&name_refs);
            let merged = merged.merge(&pface);
            return self.realize_face(&merged);
        }

        // 5. If no faces found, return the default face.
        if face_names.is_empty() {
            return self.default_face.clone();
        }

        // 6. Merge all collected face names and realize.
        let name_refs: Vec<&str> = face_names.iter().map(|s| s.as_str()).collect();
        let merged = self.face_table.merge_faces(&name_refs);
        self.realize_face(&merged)
    }

    /// Extract face name(s) from a Lisp Value.
    ///
    /// Face property values can be:
    /// - A symbol naming a face: `Value::Symbol(id)` -> `vec!["face-name"]`
    /// - A list of symbols: each element is a face name
    /// - Nil: no face
    /// - Otherwise: empty vec (unrecognized format)
    pub fn resolve_face_value(val: &Value) -> Vec<String> {
        match val {
            Value::Nil => Vec::new(),
            Value::Symbol(_) | Value::Keyword(_) => {
                if let Some(name) = val.as_symbol_name() {
                    if name == "nil" {
                        Vec::new()
                    } else {
                        vec![name.to_string()]
                    }
                } else {
                    Vec::new()
                }
            }
            Value::Cons(_) => {
                // Could be a list of face names, or a plist of face attributes.
                if let Some(items) = list_to_vec(val) {
                    // Check if first item is a keyword (plist like :foreground "red")
                    if items
                        .first()
                        .map(|v| matches!(v, Value::Keyword(_)))
                        .unwrap_or(false)
                    {
                        // Plist face — handled by face_at_pos via face_from_plist().
                        // Return a sentinel that face_at_pos recognizes.
                        vec!["--plist-face--".to_string()]
                    } else {
                        // List of face name symbols.
                        items
                            .iter()
                            .filter_map(|item| {
                                item.as_symbol_name()
                                    .filter(|n| *n != "nil")
                                    .map(|n| n.to_string())
                            })
                            .collect()
                    }
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    /// Parse an inline face plist like `(:foreground "red" :weight bold)` into
    /// a `Face` object.  Handles the same keywords as GNU Emacs face specs.
    pub fn face_from_plist(val: &Value) -> Option<NeoFace> {
        use neovm_core::face::FontSlant;

        let items = list_to_vec(val)?;
        let mut face = NeoFace::new("--inline--");
        let mut i = 0;
        while i < items.len() {
            let key = items[i].as_symbol_name().unwrap_or("");
            let val_item = items.get(i + 1);
            match key {
                ":foreground" => {
                    if let Some(s) = val_item.and_then(|v| v.as_str()) {
                        if let Some(c) = NeoColor::from_name(s).or_else(|| NeoColor::from_hex(s)) {
                            face.foreground = Some(c);
                        }
                    }
                }
                ":background" => {
                    if let Some(s) = val_item.and_then(|v| v.as_str()) {
                        if let Some(c) = NeoColor::from_name(s).or_else(|| NeoColor::from_hex(s)) {
                            face.background = Some(c);
                        }
                    }
                }
                ":weight" => {
                    if let Some(name) = val_item.and_then(|v| v.as_symbol_name()) {
                        face.weight = FontWeight::from_symbol(name);
                    }
                }
                ":slant" => {
                    if let Some(name) = val_item.and_then(|v| v.as_symbol_name()) {
                        face.slant = Some(match name {
                            "italic" => FontSlant::Italic,
                            "oblique" => FontSlant::Oblique,
                            _ => FontSlant::Normal,
                        });
                    }
                }
                ":height" => {
                    if let Some(v) = val_item {
                        match v {
                            Value::Int(n) => {
                                face.height = Some(FaceHeight::Absolute(*n as i32));
                            }
                            Value::Float(f, _) => {
                                face.height = Some(FaceHeight::Relative(*f));
                            }
                            _ => {}
                        }
                    }
                }
                ":family" => {
                    if let Some(s) = val_item.and_then(|v| v.as_str()) {
                        face.family = Some(s.to_string());
                    }
                }
                ":underline" => {
                    if let Some(v) = val_item {
                        face.underline = Self::parse_underline_value(v);
                    }
                }
                ":overline" => {
                    if let Some(v) = val_item {
                        if let Some(s) = v.as_str() {
                            // Color string
                            face.overline = Some(true);
                            face.overline_color =
                                NeoColor::from_name(s).or_else(|| NeoColor::from_hex(s));
                        } else {
                            face.overline = Some(v.is_truthy());
                        }
                    }
                }
                ":strike-through" => {
                    if let Some(v) = val_item {
                        if let Some(s) = v.as_str() {
                            // Color string
                            face.strike_through = Some(true);
                            face.strike_through_color =
                                NeoColor::from_name(s).or_else(|| NeoColor::from_hex(s));
                        } else {
                            face.strike_through = Some(v.is_truthy());
                        }
                    }
                }
                ":box" => {
                    if let Some(v) = val_item {
                        face.box_border = Self::parse_box_value(v);
                    }
                }
                ":inverse-video" => {
                    if let Some(v) = val_item {
                        face.inverse_video = Some(v.is_truthy());
                    }
                }
                ":extend" => {
                    if let Some(v) = val_item {
                        face.extend = Some(v.is_truthy());
                    }
                }
                ":inherit" => {
                    if let Some(v) = val_item {
                        if let Some(name) = v.as_symbol_name() {
                            if name != "nil" {
                                face.inherit.push(name.to_string());
                            }
                        } else if let Some(names) = list_to_vec(v) {
                            for n in &names {
                                if let Some(name) = n.as_symbol_name() {
                                    if name != "nil" {
                                        face.inherit.push(name.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
                ":distant-foreground" => {
                    if let Some(s) = val_item.and_then(|v| v.as_str()) {
                        if let Some(c) = NeoColor::from_name(s).or_else(|| NeoColor::from_hex(s)) {
                            face.distant_foreground = Some(c);
                        }
                    }
                }
                ":width" => {
                    if let Some(name) = val_item.and_then(|v| v.as_symbol_name()) {
                        face.width = neovm_core::face::FontWidth::from_symbol(name);
                    }
                }
                ":foundry" => {
                    if let Some(s) = val_item.and_then(|v| v.as_str()) {
                        face.foundry = Some(s.to_string());
                    }
                }
                _ => {}
            }
            i += 2;
        }
        Some(face)
    }

    /// Parse an `:underline` attribute value.
    ///
    /// GNU Emacs supports: `t`, a color string, or a plist
    /// `(:color COLOR :style STYLE :position POS)`.
    fn parse_underline_value(val: &Value) -> Option<neovm_core::face::Underline> {
        use neovm_core::face::Underline;
        match val {
            Value::True => Some(Underline {
                style: NeoUnderlineStyle::Line,
                color: None,
                position: None,
            }),
            Value::Nil => None,
            _ if val.as_str().is_some() => {
                // Color string
                let s = val.as_str().unwrap();
                Some(Underline {
                    style: NeoUnderlineStyle::Line,
                    color: NeoColor::from_name(s).or_else(|| NeoColor::from_hex(s)),
                    position: None,
                })
            }
            Value::Cons(_) => {
                // Plist: (:color "red" :style wave :position t)
                let items = list_to_vec(val)?;
                let mut style = NeoUnderlineStyle::Line;
                let mut color = None;
                let mut position = None;
                let mut i = 0;
                while i < items.len() {
                    let key = items[i].as_symbol_name().unwrap_or("");
                    let v = items.get(i + 1);
                    match key {
                        ":color" => {
                            if let Some(s) = v.and_then(|v| v.as_str()) {
                                color = NeoColor::from_name(s).or_else(|| NeoColor::from_hex(s));
                            }
                        }
                        ":style" => {
                            if let Some(name) = v.and_then(|v| v.as_symbol_name()) {
                                style = match name {
                                    "wave" => NeoUnderlineStyle::Wave,
                                    "double-line" => NeoUnderlineStyle::DoubleLine,
                                    "dots" => NeoUnderlineStyle::Dot,
                                    "dashes" => NeoUnderlineStyle::Dash,
                                    _ => NeoUnderlineStyle::Line,
                                };
                            }
                        }
                        ":position" => {
                            if let Some(v) = v {
                                if let Value::Int(n) = v {
                                    position = Some(*n as i32);
                                }
                            }
                        }
                        _ => {}
                    }
                    i += 2;
                }
                Some(Underline {
                    style,
                    color,
                    position,
                })
            }
            _ => {
                if val.is_truthy() {
                    Some(Underline {
                        style: NeoUnderlineStyle::Line,
                        color: None,
                        position: None,
                    })
                } else {
                    None
                }
            }
        }
    }

    /// Parse a `:box` attribute value.
    ///
    /// GNU Emacs supports: `t`, a color string, an integer (line width),
    /// or a plist `(:line-width WIDTH :color COLOR :style STYLE)`.
    fn parse_box_value(val: &Value) -> Option<neovm_core::face::BoxBorder> {
        use neovm_core::face::{BoxBorder, BoxStyle};
        match val {
            Value::True => Some(BoxBorder {
                color: None,
                width: 1,
                style: BoxStyle::Flat,
            }),
            Value::Nil => None,
            Value::Int(n) => Some(BoxBorder {
                color: None,
                width: *n as i32,
                style: BoxStyle::Flat,
            }),
            _ if val.as_str().is_some() => {
                let s = val.as_str().unwrap();
                Some(BoxBorder {
                    color: NeoColor::from_name(s).or_else(|| NeoColor::from_hex(s)),
                    width: 1,
                    style: BoxStyle::Flat,
                })
            }
            Value::Cons(_) => {
                let items = list_to_vec(val)?;
                let mut color = None;
                let mut width = 1i32;
                let mut style = BoxStyle::Flat;
                let mut i = 0;
                while i < items.len() {
                    let key = items[i].as_symbol_name().unwrap_or("");
                    let v = items.get(i + 1);
                    match key {
                        ":line-width" => {
                            if let Some(v) = v {
                                match v {
                                    Value::Int(n) => width = *n as i32,
                                    Value::Cons(cell) => {
                                        // (H . V) pair — use H
                                        let pair = neovm_core::emacs_core::value::read_cons(*cell);
                                        if let Value::Int(n) = pair.car {
                                            width = n as i32;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        ":color" => {
                            if let Some(s) = v.and_then(|v| v.as_str()) {
                                color = NeoColor::from_name(s).or_else(|| NeoColor::from_hex(s));
                            }
                        }
                        ":style" => {
                            if let Some(name) = v.and_then(|v| v.as_symbol_name()) {
                                style = match name {
                                    "released-button" => BoxStyle::Raised,
                                    "pressed-button" => BoxStyle::Pressed,
                                    _ => BoxStyle::Flat,
                                };
                            }
                        }
                        _ => {}
                    }
                    i += 2;
                }
                Some(BoxBorder {
                    color,
                    width,
                    style,
                })
            }
            _ => None,
        }
    }

    /// Convert a neovm-core `Face` into a fully-realized `ResolvedFace`.
    ///
    /// Unset fields fall back to the default face.  Handles `inverse_video`,
    /// `FaceHeight` (absolute/relative), underline, overline, strike-through,
    /// box, overstrike, and extend.
    pub fn realize_face(&self, face: &NeoFace) -> ResolvedFace {
        let mut rf = self.default_face.clone();

        // Foreground
        if let Some(c) = &face.foreground {
            rf.fg = color_to_pixel(c);
        }
        // Background
        if let Some(c) = &face.background {
            rf.bg = color_to_pixel(c);
        }
        // Inverse video: swap fg and bg
        if face.inverse_video == Some(true) {
            std::mem::swap(&mut rf.fg, &mut rf.bg);
        }

        // Font family
        if let Some(family) = &face.family {
            rf.font_family = family.clone();
        }
        // Font weight
        if let Some(w) = &face.weight {
            rf.font_weight = w.0;
        }
        // Font slant
        if let Some(s) = &face.slant {
            rf.italic = s.is_italic();
        }
        // Font height
        if let Some(h) = &face.height {
            match h {
                FaceHeight::Absolute(tenths) => {
                    // 1/10 pt -> pixels at 96 DPI: (tenths/10) * (96/72)
                    rf.font_size = *tenths as f32 / 10.0 * (96.0 / 72.0);
                }
                FaceHeight::Relative(factor) => {
                    rf.font_size = self.default_face.font_size * (*factor as f32);
                }
            }
        }

        // Underline
        if let Some(ul) = &face.underline {
            rf.underline_style = underline_style_to_u8(&ul.style);
            rf.underline_color = ul.color.as_ref().map(color_to_pixel).unwrap_or(0);
        }
        // Overline
        if let Some(over) = face.overline {
            rf.overline = over;
        }
        if let Some(c) = &face.overline_color {
            rf.overline_color = color_to_pixel(c);
        }
        // Strike-through
        if let Some(st) = face.strike_through {
            rf.strike_through = st;
        }
        if let Some(c) = &face.strike_through_color {
            rf.strike_through_color = color_to_pixel(c);
        }
        // Box border
        if let Some(bb) = &face.box_border {
            rf.box_type = 1;
            rf.box_color = bb.color.as_ref().map(color_to_pixel).unwrap_or(rf.fg);
            rf.box_line_width = bb.width;
        }
        // Extend
        if let Some(ext) = face.extend {
            rf.extend = ext;
        }
        // Overstrike
        if face.overstrike {
            rf.overstrike = true;
        }

        rf
    }

    /// Resolve a face from a Lisp Value (as found in overlay "face" property).
    ///
    /// Returns None if the value doesn't specify any known face names.
    pub fn resolve_face_from_value(&self, val: &Value) -> Option<ResolvedFace> {
        let names = Self::resolve_face_value(val);
        if names.is_empty() {
            return None;
        }
        let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let merged = self.face_table.merge_faces(&name_refs);
        Some(self.realize_face(&merged))
    }
}

/// Map `NeoUnderlineStyle` to the numeric code used by the layout engine.
/// Codes: 1=Line, 2=Wave, 3=Double, 4=Dotted, 5=Dashed
fn underline_style_to_u8(style: &NeoUnderlineStyle) -> u8 {
    match style {
        NeoUnderlineStyle::Line => 1,
        NeoUnderlineStyle::Wave => 2,
        NeoUnderlineStyle::DoubleLine => 3,
        NeoUnderlineStyle::Dot => 4,
        NeoUnderlineStyle::Dash => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neovm_core::buffer::BufferManager;
    use neovm_core::window::{FrameManager, Rect as NeoRect, WindowId};

    /// Create a minimal Evaluator-like test fixture (FrameManager + BufferManager)
    /// and verify `collect_layout_params` produces correct output.
    #[test]
    fn test_collect_layout_params_basic() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();

        // Create a buffer.
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*test*");

        // Create a frame with that buffer.
        let frame_id = evaluator
            .frame_manager_mut()
            .create_frame("test-frame", 800, 600, buf_id);

        // Set some frame font metrics.
        if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
            frame.font_pixel_size = 14.0;
            frame.char_width = 7.0;
            frame.char_height = 14.0;
        }

        let (fp, wps) = collect_layout_params(&evaluator, frame_id)
            .expect("collect_layout_params should succeed");

        // Check FrameParams.
        assert_eq!(fp.width, 800.0);
        assert_eq!(fp.height, 600.0);
        assert_eq!(fp.char_width, 7.0);
        assert_eq!(fp.char_height, 14.0);
        assert_eq!(fp.font_pixel_size, 14.0);

        // Should have 1 root leaf + 1 minibuffer = 2 windows.
        assert_eq!(wps.len(), 2, "expected root leaf + minibuffer");

        // First window: root leaf (not minibuffer).
        let root_wp = &wps[0];
        assert!(!root_wp.is_minibuffer);
        assert!(root_wp.selected); // first window is selected by default
        assert_eq!(root_wp.char_width, 7.0);
        assert_eq!(root_wp.char_height, 14.0);
        assert_eq!(root_wp.mode_line_height, 14.0); // non-minibuffer gets mode-line

        // Second window: minibuffer.
        let mini_wp = &wps[1];
        assert!(mini_wp.is_minibuffer);
        assert!(!mini_wp.selected);
        assert_eq!(mini_wp.mode_line_height, 0.0); // minibuffer has no mode-line
    }

    #[test]
    fn test_frame_params_from_neovm() {
        let mut buf_mgr = BufferManager::new();
        let buf_id = buf_mgr.create_buffer("*scratch*");
        let mut frame_mgr = FrameManager::new();
        let fid = frame_mgr.create_frame("test", 1024, 768, buf_id);
        let frame = frame_mgr.get(fid).unwrap();

        let face_table = FaceTable::new();
        let fp = frame_params_from_neovm(frame, &face_table);
        assert_eq!(fp.width, 1024.0);
        assert_eq!(fp.height, 768.0);
    }

    #[test]
    fn test_window_params_from_neovm_internal_returns_none() {
        use neovm_core::window::SplitDirection;

        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*test*");
        let frame_id = evaluator
            .frame_manager_mut()
            .create_frame("test", 800, 600, buf_id);
        let internal = Window::Internal {
            id: WindowId(99),
            direction: SplitDirection::Vertical,
            children: vec![],
            bounds: NeoRect::new(0.0, 0.0, 100.0, 100.0),
            combination_limit: false,
        };
        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let frame = evaluator.frame_manager().get(frame_id).unwrap();

        let result = window_params_from_neovm(
            &internal,
            &buf,
            frame,
            evaluator.face_table(),
            false,
            false,
            Value::True,
        );
        assert!(result.is_none(), "Internal windows should return None");
    }

    #[test]
    fn test_effective_cursor_spec_prefers_window_cursor_type() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*cursor*");
        let frame_id = evaluator
            .frame_manager_mut()
            .create_frame("test", 800, 600, buf_id);
        let frame = evaluator.frame_manager().get(frame_id).unwrap();
        let buffer = evaluator.buffer_manager().get(buf_id).unwrap();

        let spec = effective_cursor_spec(
            frame,
            buffer,
            true,
            false,
            Value::cons(Value::symbol("bar"), Value::Int(5)),
        )
        .unwrap();

        assert_eq!(spec.cursor_type, 1);
        assert_eq!(spec.bar_width, 5);
    }

    #[test]
    fn test_effective_cursor_spec_nonselected_box_becomes_hollow() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*cursor*");
        let frame_id = evaluator
            .frame_manager_mut()
            .create_frame("test", 800, 600, buf_id);
        let frame = evaluator.frame_manager().get(frame_id).unwrap();
        let buffer = evaluator.buffer_manager().get(buf_id).unwrap();

        let spec = effective_cursor_spec(frame, buffer, false, false, Value::True).unwrap();

        assert_eq!(spec.cursor_type, 3);
    }

    #[test]
    fn test_frame_cursor_color_uses_cursor_face_background() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator
            .buffer_manager_mut()
            .create_buffer("*cursor-color*");
        let frame_id = evaluator
            .frame_manager_mut()
            .create_frame("test", 800, 600, buf_id);
        let frame = evaluator.frame_manager().get(frame_id).unwrap();

        let cursor_color = frame_cursor_color_pixel(frame, evaluator.face_table());
        let expected = evaluator
            .face_table()
            .resolve("cursor")
            .background
            .map(|color| color_to_pixel(&color))
            .unwrap();

        assert_eq!(cursor_color, expected);
    }

    #[test]
    fn test_window_params_buffer_locals() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*locals*");

        // Set buffer-local variables.
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.properties.insert(
                "truncate-lines".to_string(),
                neovm_core::emacs_core::value::RuntimeBindingValue::Bound(Value::True),
            );
            buf.properties.insert(
                "tab-width".to_string(),
                neovm_core::emacs_core::value::RuntimeBindingValue::Bound(Value::Int(4)),
            );
            buf.properties.insert(
                "word-wrap".to_string(),
                neovm_core::emacs_core::value::RuntimeBindingValue::Bound(Value::Nil),
            );
        }

        let frame_id = evaluator
            .frame_manager_mut()
            .create_frame("test", 800, 600, buf_id);

        let (_, wps) = collect_layout_params(&evaluator, frame_id).unwrap();

        // The root window should pick up the buffer-local vars.
        let wp = &wps[0];
        assert!(wp.truncate_lines);
        assert!(!wp.word_wrap);
        assert_eq!(wp.tab_width, 4);
    }

    #[test]
    fn test_window_params_fringes_and_margins() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*fringe*");
        let frame_id = evaluator
            .frame_manager_mut()
            .create_frame("test", 800, 600, buf_id);

        // Set fringes and margins on the root window.
        if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
            frame.char_width = 8.0;
            if let Some(win) = frame.selected_window_mut() {
                if let Window::Leaf {
                    fringes, margins, ..
                } = win
                {
                    *fringes = (10, 12);
                    *margins = (2, 3);
                }
            }
        }

        let (_, wps) = collect_layout_params(&evaluator, frame_id).unwrap();
        let wp = &wps[0];

        assert_eq!(wp.left_fringe_width, 10.0);
        assert_eq!(wp.right_fringe_width, 12.0);
        assert_eq!(wp.left_margin_width, 16.0); // 2 * 8.0
        assert_eq!(wp.right_margin_width, 24.0); // 3 * 8.0

        // text_bounds should be narrower by fringes + margins.
        let expected_text_x = wp.bounds.x + 10.0 + 16.0;
        assert!((wp.text_bounds.x - expected_text_x).abs() < 0.01);
    }

    #[test]
    fn test_collect_nonexistent_frame() {
        let evaluator = neovm_core::emacs_core::Evaluator::new();
        let result = collect_layout_params(&evaluator, FrameId(999999));
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // RustBufferAccess tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_rust_buffer_access_copy_text() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*test-copy*");
        // Insert some text
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.text.insert_str(0, "Hello, world!");
            buf.zv = buf.text.len();
        }

        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let access = RustBufferAccess::new(buf);

        let mut out = Vec::new();
        access.copy_text(0, 5, &mut out);
        assert_eq!(&out, b"Hello");

        access.copy_text(7, 13, &mut out);
        assert_eq!(&out, b"world!");
    }

    #[test]
    fn test_rust_buffer_access_charpos_to_bytepos() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*test-pos*");
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.text.insert_str(0, "abc");
            buf.zv = buf.text.len();
        }

        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let access = RustBufferAccess::new(buf);

        assert_eq!(access.charpos_to_bytepos(0), 0);
        assert_eq!(access.charpos_to_bytepos(1), 1);
        assert_eq!(access.charpos_to_bytepos(2), 2);
        assert_eq!(access.charpos_to_bytepos(3), 3);
        assert_eq!(access.charpos_to_bytepos(4), 3);
    }

    #[test]
    fn test_rust_buffer_access_lisp_charpos_to_bytepos() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator
            .buffer_manager_mut()
            .create_buffer("*test-lisp-pos*");
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.text.insert_str(0, "abc");
            buf.zv = buf.text.len();
        }

        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let access = RustBufferAccess::new(buf);

        assert_eq!(access.lisp_charpos_to_bytepos(0), 0);
        assert_eq!(access.lisp_charpos_to_bytepos(1), 0);
        assert_eq!(access.lisp_charpos_to_bytepos(2), 1);
        assert_eq!(access.lisp_charpos_to_bytepos(3), 2);
        assert_eq!(access.lisp_charpos_to_bytepos(4), 3);
    }

    #[test]
    fn test_rust_buffer_access_count_lines() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*test-lines*");
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.text.insert_str(0, "line1\nline2\nline3");
            buf.zv = buf.text.len();
        }

        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let access = RustBufferAccess::new(buf);

        assert_eq!(access.count_lines(0, 17), 2); // 2 newlines
        assert_eq!(access.count_lines(0, 6), 1); // 1 newline in "line1\n"
        assert_eq!(access.count_lines(0, 5), 0); // no newline in "line1"
    }

    #[test]
    fn test_rust_buffer_access_metadata() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*meta*");
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.text.insert_str(0, "content");
            buf.zv = buf.text.len();
            buf.modified = true;
            buf.file_name = Some("/tmp/test.el".to_string());
        }

        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let access = RustBufferAccess::new(buf);

        assert_eq!(access.name(), "*meta*");
        assert!(access.modified());
        assert_eq!(access.file_name(), Some("/tmp/test.el"));
        assert_eq!(access.zv(), 7);
    }

    // -----------------------------------------------------------------------
    // RustTextPropAccess tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_text_prop_check_invisible() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*invis*");
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.text.insert_str(0, "visible hidden visible");
            buf.zv = buf.text.len();
            // Mark "hidden" (positions 8..14) as invisible
            buf.text_props.put_property(8, 14, "invisible", Value::True);
        }

        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let access = RustTextPropAccess::new(buf);

        // Position 0: not invisible
        let (invis, _next) = access.check_invisible(0);
        assert!(!invis);

        // Position 8: invisible
        let (invis, _next) = access.check_invisible(8);
        assert!(invis);

        // Position 14: visible again
        let (invis, _next) = access.check_invisible(14);
        assert!(!invis);
    }

    #[test]
    fn test_text_prop_check_display() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*display*");
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.text.insert_str(0, "abcdef");
            buf.zv = buf.text.len();
            // Set a display property on positions 2..4
            buf.text_props.put_property(2, 4, "display", Value::Int(42));
        }

        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let access = RustTextPropAccess::new(buf);

        // Position 0: no display prop
        let (dp, _next) = access.check_display_prop(0);
        assert!(dp.is_none());

        // Position 2: has display prop
        let (dp, _next) = access.check_display_prop(2);
        assert!(dp.is_some());
        assert!(matches!(dp, Some(Value::Int(42))));
    }

    #[test]
    fn test_text_prop_line_spacing() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*spacing*");
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.text.insert_str(0, "line1\nline2");
            buf.zv = buf.text.len();
            // Set line-spacing on "line2" area
            buf.text_props
                .put_property(6, 11, "line-spacing", Value::Int(4));
        }

        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let access = RustTextPropAccess::new(buf);

        // Position 0: no line-spacing
        assert_eq!(access.check_line_spacing(0, 16.0), 0.0);

        // Position 6: line-spacing = 4
        assert_eq!(access.check_line_spacing(6, 16.0), 4.0);
    }

    #[test]
    fn test_text_prop_next_change() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*next*");
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.text.insert_str(0, "aabbcc");
            buf.zv = buf.text.len();
            buf.text_props.put_property(2, 4, "face", Value::True);
        }

        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let access = RustTextPropAccess::new(buf);

        // At position 0, next change should be at 2 (where face starts)
        let next = access.next_property_change(0);
        assert_eq!(next, 2);

        // At position 2, next change should be at 4 (where face ends)
        let next = access.next_property_change(2);
        assert_eq!(next, 4);
    }

    #[test]
    fn test_text_prop_get_property() {
        let mut evaluator = neovm_core::emacs_core::Evaluator::new();
        let buf_id = evaluator.buffer_manager_mut().create_buffer("*prop*");
        if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
            buf.text.insert_str(0, "test");
            buf.zv = buf.text.len();
            buf.text_props.put_property(0, 4, "face", Value::Int(5));
        }

        let buf = evaluator.buffer_manager().get(buf_id).unwrap();
        let access = RustTextPropAccess::new(buf);

        let face = access.get_property(0, "face");
        assert!(matches!(face, Some(Value::Int(5))));

        let none = access.get_property(0, "nonexistent");
        assert!(none.is_none());
    }

    // -----------------------------------------------------------------------
    // FaceResolver tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_color_to_pixel() {
        let c = NeoColor::rgb(255, 128, 0);
        assert_eq!(color_to_pixel(&c), 0x00FF8000);

        let black = NeoColor::rgb(0, 0, 0);
        assert_eq!(color_to_pixel(&black), 0x00000000);

        let white = NeoColor::rgb(255, 255, 255);
        assert_eq!(color_to_pixel(&white), 0x00FFFFFF);
    }

    #[test]
    fn test_face_resolver_default() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let table = FaceTable::new();

        let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);
        let df = resolver.default_face();

        // The standard "default" face has foreground black (0,0,0) and
        // background white (255,255,255).
        assert_eq!(df.fg, 0x00000000); // black
        assert_eq!(df.bg, 0x00FFFFFF); // white
        assert_eq!(df.font_weight, FontWeight::NORMAL.0); // 400
        assert!(!df.italic);
        assert!(!df.overstrike);
        assert!(!df.extend);
        assert_eq!(df.underline_style, 0);
        assert!(!df.strike_through);
        assert!(!df.overline);
        assert_eq!(df.box_type, 0);
    }

    #[test]
    fn test_face_resolver_with_text_property() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let table = FaceTable::new();
        let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

        // Create a buffer and set "face" text property to bold.
        let mut buf =
            neovm_core::buffer::Buffer::new(neovm_core::buffer::BufferId(1), "*test*".to_string());
        buf.text.insert_str(0, "hello world");
        buf.zv = buf.text.len();
        // Set "face" to the symbol "bold" on positions 0..5.
        buf.text_props
            .put_property(0, 5, "face", Value::symbol("bold"));

        let mut next_check = buf.zv;
        let resolved = resolver.face_at_pos(&buf, 0, &mut next_check);

        // Bold face should have weight 700.
        assert_eq!(resolved.font_weight, FontWeight::BOLD.0);
        // next_check should be 5 (where the property changes).
        assert_eq!(next_check, 5);

        // Position 6 should have default weight.
        let mut nc2 = buf.zv;
        let resolved2 = resolver.face_at_pos(&buf, 6, &mut nc2);
        assert_eq!(resolved2.font_weight, FontWeight::NORMAL.0);
    }

    #[test]
    fn test_face_resolver_with_font_lock_face() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let table = FaceTable::new();
        let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

        let mut buf = neovm_core::buffer::Buffer::new(
            neovm_core::buffer::BufferId(2),
            "*fontlock*".to_string(),
        );
        buf.text.insert_str(0, "defun myfunction");
        buf.zv = buf.text.len();
        // Set "font-lock-face" to "font-lock-keyword-face" on "defun".
        buf.text_props.put_property(
            0,
            5,
            "font-lock-face",
            Value::symbol("font-lock-keyword-face"),
        );

        let mut next_check = buf.zv;
        let resolved = resolver.face_at_pos(&buf, 2, &mut next_check);

        // font-lock-keyword-face has foreground purple (128, 0, 128).
        let expected_fg = color_to_pixel(&NeoColor::rgb(128, 0, 128));
        assert_eq!(resolved.fg, expected_fg);
    }

    #[test]
    fn test_face_resolver_next_check() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let table = FaceTable::new();
        let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

        let mut buf = neovm_core::buffer::Buffer::new(
            neovm_core::buffer::BufferId(3),
            "*nextcheck*".to_string(),
        );
        buf.text.insert_str(0, "aabbccdd");
        buf.zv = buf.text.len();
        // Face property on [2, 4)
        buf.text_props
            .put_property(2, 4, "face", Value::symbol("bold"));
        // Another property on [4, 6)
        buf.text_props
            .put_property(4, 6, "face", Value::symbol("italic"));

        // At position 0, next_check should be 2 (first property boundary).
        let mut nc = buf.zv;
        let _ = resolver.face_at_pos(&buf, 0, &mut nc);
        assert_eq!(nc, 2);

        // At position 2, next_check should be 4 (end of bold range).
        let mut nc = buf.zv;
        let _ = resolver.face_at_pos(&buf, 2, &mut nc);
        assert_eq!(nc, 4);

        // At position 4, next_check should be 6 (end of italic range).
        let mut nc = buf.zv;
        let _ = resolver.face_at_pos(&buf, 4, &mut nc);
        assert_eq!(nc, 6);
    }

    #[test]
    fn test_face_resolver_overlay_face() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let table = FaceTable::new();
        let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

        let mut buf = neovm_core::buffer::Buffer::new(
            neovm_core::buffer::BufferId(4),
            "*overlay*".to_string(),
        );
        buf.text.insert_str(0, "overlay text here");
        buf.zv = buf.text.len();

        // Create an overlay with "face" = "bold" covering [0, 7).
        let oid = buf.overlays.make_overlay(0, 7);
        buf.overlays.overlay_put(oid, "face", Value::symbol("bold"));

        let mut nc = buf.zv;
        let resolved = resolver.face_at_pos(&buf, 3, &mut nc);
        assert_eq!(resolved.font_weight, FontWeight::BOLD.0);
        // next_check should be at most 7 (end of overlay).
        assert!(nc <= 7);
    }

    #[test]
    fn test_face_resolver_overlay_priority() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let mut table = FaceTable::new();

        // Define two custom faces with different foreground colors.
        let mut face_a = NeoFace::new("face-a");
        face_a.foreground = Some(NeoColor::rgb(255, 0, 0)); // red
        table.define(face_a);

        let mut face_b = NeoFace::new("face-b");
        face_b.foreground = Some(NeoColor::rgb(0, 0, 255)); // blue
        table.define(face_b);

        let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

        let mut buf = neovm_core::buffer::Buffer::new(
            neovm_core::buffer::BufferId(5),
            "*priority*".to_string(),
        );
        buf.text.insert_str(0, "priority test");
        buf.zv = buf.text.len();

        // Overlay A: priority 10, face-a (red)
        let oid_a = buf.overlays.make_overlay(0, 10);
        buf.overlays
            .overlay_put(oid_a, "face", Value::symbol("face-a"));
        buf.overlays.overlay_put(oid_a, "priority", Value::Int(10));

        // Overlay B: priority 20, face-b (blue) — should win
        let oid_b = buf.overlays.make_overlay(0, 10);
        buf.overlays
            .overlay_put(oid_b, "face", Value::symbol("face-b"));
        buf.overlays.overlay_put(oid_b, "priority", Value::Int(20));

        let mut nc = buf.zv;
        let resolved = resolver.face_at_pos(&buf, 5, &mut nc);
        // face-b (blue, priority 20) should override face-a (red, priority 10).
        assert_eq!(resolved.fg, color_to_pixel(&NeoColor::rgb(0, 0, 255)));
    }

    #[test]
    fn test_face_resolver_inverse_video() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let mut table = FaceTable::new();

        let mut inv_face = NeoFace::new("inverse-test");
        inv_face.foreground = Some(NeoColor::rgb(255, 255, 255)); // white
        inv_face.background = Some(NeoColor::rgb(0, 0, 0)); // black
        inv_face.inverse_video = Some(true);
        table.define(inv_face);

        let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

        let mut buf = neovm_core::buffer::Buffer::new(
            neovm_core::buffer::BufferId(6),
            "*inverse*".to_string(),
        );
        buf.text.insert_str(0, "inverted");
        buf.zv = buf.text.len();
        buf.text_props
            .put_property(0, 8, "face", Value::symbol("inverse-test"));

        let mut nc = buf.zv;
        let resolved = resolver.face_at_pos(&buf, 0, &mut nc);
        // Inverse: fg and bg should be swapped.
        assert_eq!(resolved.fg, 0x00000000); // was white, now black
        assert_eq!(resolved.bg, 0x00FFFFFF); // was black, now white
    }

    #[test]
    fn test_resolve_face_value_symbol() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let names = FaceResolver::resolve_face_value(&Value::symbol("bold"));
        assert_eq!(names, vec!["bold"]);
    }

    #[test]
    fn test_resolve_face_value_nil() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let names = FaceResolver::resolve_face_value(&Value::Nil);
        assert!(names.is_empty());
    }

    #[test]
    fn test_resolve_face_value_list() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let list = Value::list(vec![Value::symbol("bold"), Value::symbol("italic")]);
        let names = FaceResolver::resolve_face_value(&list);
        assert_eq!(names, vec!["bold", "italic"]);
    }

    #[test]
    fn test_realize_face_height_absolute() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let table = FaceTable::new();
        let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

        let mut face = NeoFace::new("tall");
        face.height = Some(FaceHeight::Absolute(240)); // 24pt
        let realized = resolver.realize_face(&face);
        // 240/10 * 96/72 = 24 * 1.333... = 32.0
        assert!((realized.font_size - 32.0).abs() < 0.1);
    }

    #[test]
    fn test_realize_face_height_relative() {
        let _evaluator = neovm_core::emacs_core::Evaluator::new();
        let table = FaceTable::new();
        let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

        let mut face = NeoFace::new("scaled");
        face.height = Some(FaceHeight::Relative(2.0));
        let realized = resolver.realize_face(&face);
        // 2.0 * default_font_size
        let expected = resolver.default_face().font_size * 2.0;
        assert!((realized.font_size - expected).abs() < 0.1);
    }
}
