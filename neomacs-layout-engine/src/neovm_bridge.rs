//! Bridge between neovm-core data types and the layout engine.
//!
//! Provides functions to build `WindowParams` and `FrameParams` from
//! the Rust Context's state, replacing C FFI data sources.

use neovm_core::buffer::{
    Buffer,
    buffer::{BUFFER_SLOT_COUNT, lookup_buffer_slot},
    buffer_text::BufferText,
    overlay::OverlayList,
};
use neovm_core::emacs_core::intern;
use neovm_core::emacs_core::symbol::Obarray;
use neovm_core::emacs_core::value::{ValueKind, eq_value, list_to_vec};
use neovm_core::emacs_core::{Context, Value};
use neovm_core::face::{
    Color as NeoColor, Face as NeoFace, FaceHeight, FaceTable, FontWeight,
    UnderlineStyle as NeoUnderlineStyle,
};
use neovm_core::window::{Frame, FrameId, Window};

use super::types::{FrameParams, WindowParams};
use crate::fontconfig::face_height_to_pixels;
use neomacs_display_protocol::types::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisplayLineNumbersMode {
    Off,
    Absolute,
    Relative,
    Visual,
}

pub(crate) trait LayoutBufferView {
    fn layout_get_buffer_local(&self, name: &str) -> Option<Value>;
    fn layout_buffer_local_value(&self, name: &str) -> Option<Value>;
    fn layout_point_min_byte(&self) -> usize;
    fn layout_point_max_byte(&self) -> usize;
    fn layout_point_max_char(&self) -> usize;
    fn layout_text(&self) -> &BufferText;
    fn layout_overlays(&self) -> &OverlayList;
}

#[derive(Clone)]
pub(crate) struct LayoutBufferSnapshot {
    pub name: String,
    pub text: BufferText,
    pub begv: usize,
    pub zv: usize,
    pub zv_char: usize,
    pub local_var_alist: Value,
    pub slots: [Value; BUFFER_SLOT_COUNT],
    pub local_flags: u64,
    pub overlays: OverlayList,
}

impl LayoutBufferSnapshot {
    pub fn from_buffer(buffer: &Buffer) -> Self {
        Self {
            name: buffer.name.clone(),
            text: buffer.text.clone(),
            begv: buffer.begv,
            zv: buffer.zv,
            zv_char: buffer.zv,
            local_var_alist: buffer.local_var_alist,
            slots: buffer.slots,
            local_flags: buffer.local_flags,
            overlays: buffer.overlays.clone(),
        }
    }

    fn slot_local_flag(&self, offset: usize) -> bool {
        debug_assert!(offset < BUFFER_SLOT_COUNT);
        (self.local_flags & (1u64 << offset)) != 0
    }
}

fn find_layout_local_var_alist_entry(alist: Value, key: Value) -> Option<Value> {
    let mut cursor = alist;
    while cursor.is_cons() {
        let entry = cursor.cons_car();
        cursor = cursor.cons_cdr();
        if entry.is_cons() && eq_value(&entry.cons_car(), &key) {
            return Some(entry.cons_cdr());
        }
    }
    None
}

impl LayoutBufferView for Buffer {
    fn layout_get_buffer_local(&self, name: &str) -> Option<Value> {
        self.get_buffer_local(name)
    }

    fn layout_buffer_local_value(&self, name: &str) -> Option<Value> {
        self.buffer_local_value(name)
    }

    fn layout_point_min_byte(&self) -> usize {
        self.point_min_byte()
    }

    fn layout_point_max_byte(&self) -> usize {
        self.point_max_byte()
    }

    fn layout_point_max_char(&self) -> usize {
        self.point_max_char()
    }

    fn layout_text(&self) -> &BufferText {
        &self.text
    }

    fn layout_overlays(&self) -> &OverlayList {
        &self.overlays
    }
}

impl LayoutBufferView for LayoutBufferSnapshot {
    fn layout_get_buffer_local(&self, name: &str) -> Option<Value> {
        if let Some(info) = lookup_buffer_slot(name) {
            if info.local_flags_idx >= 0 && !self.slot_local_flag(info.offset) {
                return None;
            }
            return Some(self.slots[info.offset]);
        }
        let key = Value::from_sym_id(intern::intern(name));
        find_layout_local_var_alist_entry(self.local_var_alist, key).filter(|v| !v.is_unbound())
    }

    fn layout_buffer_local_value(&self, name: &str) -> Option<Value> {
        if let Some(info) = lookup_buffer_slot(name) {
            return Some(self.slots[info.offset]);
        }
        let key = Value::from_sym_id(intern::intern(name));
        find_layout_local_var_alist_entry(self.local_var_alist, key)
            .and_then(|v| (!v.is_unbound()).then_some(v))
    }

    fn layout_point_min_byte(&self) -> usize {
        self.begv
    }

    fn layout_point_max_byte(&self) -> usize {
        self.zv
    }

    fn layout_point_max_char(&self) -> usize {
        self.zv_char
    }

    fn layout_text(&self) -> &BufferText {
        &self.text
    }

    fn layout_overlays(&self) -> &OverlayList {
        &self.overlays
    }
}

pub(crate) fn buffer_local_value<B: LayoutBufferView>(buffer: &B, name: &str) -> Option<Value> {
    // `Buffer::get_buffer_local` returns `Option<Value>` (by value)
    // since the Qunbound-sentinel refactor in commit 4d34fbde3 (void
    // buffer-local bindings): the value may come from an alist cons
    // cell that the caller can no longer borrow a stable reference
    // into. `Value` is `Copy` so this is zero-cost.
    buffer.layout_get_buffer_local(name)
}

fn effective_buffer_value(buffer: &Buffer, obarray: &Obarray, name: &str) -> Option<Value> {
    // Phase 10D: BUFFER_OBJFWD slots (always-local AND conditional)
    // store the live value in `buffer.slots[offset]`. After
    // `set-default` propagation, conditional slots whose
    // local-flags bit is clear still reflect the latest global
    // default in their per-buffer slot, so reading the slot
    // directly is correct in both cases. The legacy
    // `obarray.symbol_value` reader returns None for FORWARDED,
    // which would otherwise treat every slot as void here and
    // collapse `effective_buffer_bool` to false.
    if let Some(info) = neovm_core::buffer::buffer::lookup_buffer_slot(name) {
        return Some(buffer.slots[info.offset]);
    }
    buffer
        .get_buffer_local_binding(name)
        .and_then(|binding| binding.as_value())
        .or_else(|| obarray.symbol_value(name).copied())
}

fn frame_parameter_int(frame: &Frame, name: &str, default: i64) -> i64 {
    frame
        .parameters
        .get(name)
        .and_then(|v| v.as_int())
        .unwrap_or(default)
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
        menu_bar_height: frame.menu_bar_height as f32,
        tool_bar_height: frame.tool_bar_height as f32,
        tab_bar_height: frame.tab_bar_height as f32,
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
pub(crate) fn buffer_local_int<B: LayoutBufferView>(buffer: &B, name: &str, default: i64) -> i64 {
    match buffer_local_value(buffer, name) {
        Some(v) if v.is_fixnum() => v.as_fixnum().unwrap(),
        _ => default,
    }
}

fn effective_buffer_int(buffer: &Buffer, obarray: &Obarray, name: &str, default: i64) -> i64 {
    match effective_buffer_value(buffer, obarray, name) {
        Some(v) if v.is_fixnum() => v.as_fixnum().unwrap(),
        _ => default,
    }
}

/// Helper: extract a boolean buffer-local variable (nil = false, anything else = true).
pub(crate) fn buffer_local_bool<B: LayoutBufferView>(buffer: &B, name: &str) -> bool {
    match buffer_local_value(buffer, name) {
        Some(v) if v.is_nil() => false,
        None => false,
        Some(_) => true,
    }
}

fn effective_buffer_bool(buffer: &Buffer, obarray: &Obarray, name: &str) -> bool {
    match effective_buffer_value(buffer, obarray, name) {
        Some(v) if v.is_nil() => false,
        None => false,
        Some(_) => true,
    }
}

fn global_bool(obarray: &Obarray, name: &str) -> bool {
    obarray
        .symbol_value(name)
        .is_some_and(|value| !value.is_nil())
}

fn frame_total_cols(frame: &Frame) -> i64 {
    frame
        .parameters
        .get("width")
        .and_then(|value| value.as_int())
        .unwrap_or(frame.columns() as i64)
}

fn window_total_cols(window: &Window, char_width: f32) -> i64 {
    let width = window.bounds().width;
    if char_width > 0.0 {
        (width / char_width) as i64
    } else {
        0
    }
}

fn effective_truncate_lines(
    window: &Window,
    buffer: &Buffer,
    frame: &Frame,
    obarray: &Obarray,
    hscroll: usize,
) -> bool {
    if effective_buffer_bool(buffer, obarray, "truncate-lines") {
        return true;
    }

    // GNU `xdisp.c:init_iterator` only enables wrapping when the
    // window is not horizontally scrolled.
    if hscroll != 0 {
        return true;
    }

    let total_cols = window_total_cols(window, frame.char_width);
    let frame_cols = frame_total_cols(frame);

    if total_cols >= frame_cols {
        return false;
    }

    match effective_buffer_value(buffer, obarray, "truncate-partial-width-windows") {
        Some(value) if value.is_nil() => false,
        Some(value) if value.is_fixnum() => total_cols < value.as_fixnum().unwrap(),
        Some(_) => true,
        None => false,
    }
}

pub(crate) fn buffer_local_string_owned<B: LayoutBufferView>(
    buffer: &B,
    name: &str,
) -> Option<String> {
    buffer_local_value(buffer, name).and_then(|v| v.as_runtime_string_owned())
}

fn chrome_face_pixel_height(face: &ResolvedFace, fallback_char_height: f32) -> f32 {
    // GNU Emacs frame.c:1184-1185 — non-window (TTY) frames have
    //   f->column_width = 1;
    //   f->line_height  = 1;
    // and chrome rows (mode-line, header-line, tab-line) are exactly
    // one character cell tall. Face font_line_height is a GUI pixel
    // measurement and must not contribute to row sizing on a TTY
    // frame: `fallback_char_height` is set to 1.0 by
    // `bootstrap_buffers` (main.rs:1691-1694) when the frame is a
    // TTY, so detect the TTY context by the 1.0-cell marker and
    // return the cell height directly.
    //
    // Without this early return, a mode-line face with a non-zero
    // `font_line_height` (e.g. 3 from the realized Hack font under
    // cosmic-text) produced a 3-row-tall mode-line region on TTY.
    // The mode-line text painted on the first row and the remaining
    // two rows rendered as blank padding, which looked like the
    // echo area having "3 lines" instead of GNU's single row.
    if fallback_char_height <= 1.0 {
        return fallback_char_height.max(1.0);
    }
    let line_height = if face.font_line_height > 0.0 {
        face.font_line_height.ceil()
    } else {
        fallback_char_height.ceil()
    };
    let box_pixels = if face.box_type != 0 && face.box_line_width != 0 {
        2.0 * face.box_line_width.unsigned_abs() as f32
    } else {
        0.0
    };
    let minimum_row_height = fallback_char_height.ceil().max(1.0);
    (line_height + box_pixels).max(minimum_row_height)
}

pub(crate) fn buffer_local_list_values<B: LayoutBufferView>(buffer: &B, name: &str) -> Vec<Value> {
    // `list_to_vec' takes `&Value'; feed the borrowed form since
    // `buffer_local_value' returns the `Copy' `Value' by value.
    buffer_local_value(buffer, name)
        .and_then(|v| list_to_vec(&v))
        .unwrap_or_default()
}

pub(crate) fn buffer_display_line_numbers_mode<B: LayoutBufferView>(
    buffer: &B,
) -> DisplayLineNumbersMode {
    match buffer_local_value(buffer, "display-line-numbers") {
        Some(v) if v.bits() == Value::T.bits() => DisplayLineNumbersMode::Absolute,
        Some(value) if value.is_symbol_named("relative") => DisplayLineNumbersMode::Relative,
        Some(value) if value.is_symbol_named("visual") => DisplayLineNumbersMode::Visual,
        _ => DisplayLineNumbersMode::Off,
    }
}

pub(crate) fn buffer_selective_display<B: LayoutBufferView>(buffer: &B) -> i32 {
    match buffer_local_value(buffer, "selective-display") {
        Some(v) if v.is_fixnum() => v.as_fixnum().unwrap() as i32,
        Some(v) if v.bits() == Value::T.bits() => i32::MAX,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CursorSpec {
    /// GNU `enum text_cursor_kinds` discriminant. Use the
    /// `CursorKind` enum from `neomacs-display-protocol` rather
    /// than the raw byte: the slot ordering matches GNU exactly
    /// (FilledBox=0, HollowBox=1, Bar=2, Hbar=3, NoCursor=-1,
    /// Default=-2). See cursor audit Finding 1 in
    /// `drafts/cursor-audit.md`.
    cursor_kind: neomacs_display_protocol::frame_glyphs::CursorKind,
    bar_width: i32,
}

fn parse_color_pixel(value: &Value) -> Option<u32> {
    value
        .as_runtime_string_owned()
        .or_else(|| value.as_symbol_name().map(str::to_string))
        .and_then(|spec| NeoColor::parse(&spec))
        .map(|color| color_to_pixel(&color))
}

fn parse_cursor_spec(value: &Value) -> Option<CursorSpec> {
    use neomacs_display_protocol::frame_glyphs::CursorKind;

    if value.is_nil() {
        return None;
    }

    if value.bits() == Value::T.bits() || value.is_symbol_named("box") {
        return Some(CursorSpec {
            cursor_kind: CursorKind::FilledBox,
            bar_width: 1,
        });
    }
    if value.is_symbol_named("hollow") {
        return Some(CursorSpec {
            cursor_kind: CursorKind::HollowBox,
            bar_width: 1,
        });
    }
    if value.is_symbol_named("bar") {
        return Some(CursorSpec {
            cursor_kind: CursorKind::Bar,
            bar_width: 2,
        });
    }
    if value.is_symbol_named("hbar") {
        return Some(CursorSpec {
            cursor_kind: CursorKind::Hbar,
            bar_width: 2,
        });
    }
    if value.is_cons() {
        let car = value.cons_car();
        let cdr = value.cons_cdr();
        let bar_width = cdr.as_int().unwrap_or(1).max(0) as i32;
        if car.is_symbol_named("box") {
            return Some(CursorSpec {
                cursor_kind: CursorKind::FilledBox,
                bar_width,
            });
        }
        if car.is_symbol_named("bar") {
            return Some(CursorSpec {
                cursor_kind: CursorKind::Bar,
                bar_width,
            });
        }
        if car.is_symbol_named("hbar") {
            return Some(CursorSpec {
                cursor_kind: CursorKind::Hbar,
                bar_width,
            });
        }
    }

    Some(CursorSpec {
        cursor_kind: CursorKind::HollowBox,
        bar_width: 1,
    })
}

fn frame_cursor_spec(frame: &Frame) -> CursorSpec {
    use neomacs_display_protocol::frame_glyphs::CursorKind;
    frame
        .parameters
        .get("cursor-type")
        .and_then(parse_cursor_spec)
        .unwrap_or(CursorSpec {
            cursor_kind: CursorKind::FilledBox,
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
    let base = if window_cursor_type.bits() != Value::T.bits() {
        parse_cursor_spec(&window_cursor_type)
    } else if let Some(buffer_cursor_type) = buffer_local_value(buffer, "cursor-type") {
        if buffer_cursor_type.bits() == Value::T.bits() {
            Some(frame_cursor_spec(frame))
        } else {
            parse_cursor_spec(&buffer_cursor_type)
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
        && value.bits() != Value::T.bits()
    {
        return parse_cursor_spec(&value);
    }

    use neomacs_display_protocol::frame_glyphs::CursorKind;

    // GNU `xdisp.c::get_window_cursor_type` applies the non-selected
    // fallback after resolving the base cursor kind: FilledBox becomes
    // HollowBox, explicit alternate cursor types win, and BAR cursors
    // narrow by one pixel when `cursor-in-non-selected-windows` is `t`.
    let mut adjusted = base;
    if adjusted.cursor_kind == CursorKind::FilledBox {
        adjusted.cursor_kind = CursorKind::HollowBox;
    } else if adjusted.cursor_kind == CursorKind::Bar && adjusted.bar_width > 1 {
        adjusted.bar_width -= 1;
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
    obarray: &Obarray,
    face_table: &FaceTable,
    default_font_ascent: Option<f32>,
    is_selected: bool,
    is_minibuffer: bool,
    window_cursor_type: Value,
) -> Option<WindowParams> {
    // Only leaf windows can be laid out.
    let (
        win_id,
        _buf_id,
        bounds,
        window_start,
        window_end_pos,
        window_end_valid,
        point,
        hscroll,
        margins,
        left_fringe_width,
        right_fringe_width,
    ) = match window {
        Window::Leaf {
            id,
            buffer_id,
            bounds,
            window_start,
            window_end_pos,
            window_end_valid,
            point,
            hscroll,
            margins,
            display,
            ..
        } => (
            *id,
            *buffer_id,
            bounds,
            *window_start,
            *window_end_pos,
            *window_end_valid,
            *point,
            *hscroll,
            *margins,
            if display.left_fringe_width >= 0 {
                display.left_fringe_width
            } else {
                // GNU Emacs: TTY frames have 0 fringes (window-fringes → (0 0 nil nil)).
                // GUI frames default to 8 pixels.
                let gui_default = if frame.window_system.is_some() { 8 } else { 0 };
                frame_parameter_int(frame, "left-fringe", gui_default) as i32
            },
            if display.right_fringe_width >= 0 {
                display.right_fringe_width
            } else {
                let gui_default = if frame.window_system.is_some() { 8 } else { 0 };
                frame_parameter_int(frame, "right-fringe", gui_default) as i32
            },
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
    let face_resolver =
        FaceResolver::new(face_table, default_fg, default_bg, frame.font_pixel_size);

    // Convert neovm-core Rect to display Rect (same fields, different types).
    let display_bounds = Rect::new(bounds.x, bounds.y, bounds.width, bounds.height);

    // Compute text bounds (bounds minus fringes and margins).
    let left_fringe = left_fringe_width.max(0) as f32;
    let right_fringe = right_fringe_width.max(0) as f32;
    let left_margin = margins.0 as f32 * char_width;
    let right_margin = margins.1 as f32 * char_width;
    let text_x = bounds.x + left_fringe + left_margin;
    let text_width =
        (bounds.width - left_fringe - right_fringe - left_margin - right_margin).max(0.0);
    let text_bounds = Rect::new(text_x, bounds.y, text_width, bounds.height);

    // Read buffer-local variables.
    let truncate_lines = effective_truncate_lines(window, buffer, frame, obarray, hscroll);
    let word_wrap = effective_buffer_bool(buffer, obarray, "word-wrap");
    let tab_width = effective_buffer_int(buffer, obarray, "tab-width", 8) as i32;

    // GNU xdisp.c's estimate_mode_line_height starts from the frame line
    // height and lets realized face metrics grow from there.
    let mode_line_height = if is_minibuffer {
        0.0
    } else {
        let mode_line_face_name = if is_selected {
            "mode-line"
        } else {
            "mode-line-inactive"
        };
        chrome_face_pixel_height(
            &face_resolver.resolve_named_face(mode_line_face_name),
            char_height,
        )
    };

    let cursor_spec = effective_cursor_spec(
        frame,
        buffer,
        is_selected,
        is_minibuffer,
        window_cursor_type,
    )
    .unwrap_or(CursorSpec {
        cursor_kind: neomacs_display_protocol::frame_glyphs::CursorKind::NoCursor,
        bar_width: 1,
    });
    let cursor_color = frame_cursor_color_pixel(frame, face_table);
    let x_stretch_cursor = global_bool(obarray, "x-stretch-cursor");

    // Header-line: show if header-line-format is non-nil
    let header_line_height = if effective_buffer_bool(buffer, obarray, "header-line-format") {
        let header_line_face_name = if is_selected {
            "header-line-active"
        } else {
            "header-line-inactive"
        };
        chrome_face_pixel_height(
            &face_resolver.resolve_named_face(header_line_face_name),
            char_height,
        )
    } else {
        0.0
    };

    // Tab-line: show if tab-line-format is non-nil
    let tab_line_height = if effective_buffer_bool(buffer, obarray, "tab-line-format") {
        chrome_face_pixel_height(&face_resolver.resolve_named_face("tab-line"), char_height)
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
        // Previous visible end converted back to the layout engine's internal
        // 0-based char position space.  GNU stores this as an offset from Z.
        window_end: if window_end_valid {
            buffer
                .point_max_char()
                .saturating_add(1)
                .saturating_sub(window_end_pos)
                .saturating_sub(1) as i64
        } else {
            0
        },
        // Mirror GNU `window.c:window_point` (around line 1782):
        //
        //   return (w == XWINDOW (selected_window)
        //           ? BUF_PT (XBUFFER (w->contents))
        //           : XMARKER (w->pointm)->charpos);
        //
        // For the selected window, the authoritative point lives in the
        // buffer (`BUF_PT`), because editing commands like
        // self-insert-command advance `buf->pt` but do not touch
        // `w->pointm` until the window is later deselected (via
        // `select_window`, which saves the live buffer point into the
        // outgoing window's pointm marker).  Reading `Window::point` here
        // would see a stale pre-command value and place the cursor one
        // character behind where typing just landed.  For non-selected
        // windows, `Window::point` is the right source (it was snapshotted
        // from `buf->pt` the last time the window was deselected).
        //
        // `buffer.pt` is already 0-based (matches the layout engine's
        // internal coordinate system); `Window::point` is GNU/Lisp 1-based
        // and gets normalized with the usual `-1`.
        point: if is_selected {
            buffer.pt as i64
        } else {
            point.saturating_sub(1) as i64
        },
        buffer_size: buffer.point_max_char() as i64,
        buffer_begv: buffer.point_min_char() as i64,
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
        font_ascent: default_font_ascent
            .filter(|ascent| *ascent > 0.0)
            .unwrap_or(frame.font_pixel_size * 0.8),
        mode_line_height,
        header_line_height,
        tab_line_height,
        cursor_kind: cursor_spec.cursor_kind,
        cursor_bar_width: cursor_spec.bar_width,
        x_stretch_cursor,
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
            Some(v) if v.is_fixnum() => v.as_fixnum().unwrap() as f32,
            Some(v) if v.is_float() => v.xfloat() as f32,
            _ => 0.0,
        },
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
    evaluator: &Context,
    frame_id: FrameId,
    default_font_ascent: Option<f32>,
) -> Option<(FrameParams, Vec<WindowParams>)> {
    let frame = evaluator.frame_manager().get(frame_id)?;
    let frame_is_selected = evaluator
        .frame_manager()
        .selected_frame()
        .is_some_and(|selected| selected.id == frame_id);
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
        let is_selected = frame_is_selected && frame.selected_window == *win_id;
        let window_cursor_type = evaluator.frame_manager().window_cursor_type(*win_id);
        if let Some(wp) = window_params_from_neovm(
            window,
            buffer,
            frame,
            evaluator.obarray(),
            evaluator.face_table(),
            default_font_ascent,
            is_selected,
            false,
            window_cursor_type,
        ) {
            tracing::debug!(
                "layout window cursor: win={} selected={} minibuffer=false kind={:?} width={} color=#{:06x} window-cursor-type={:?}",
                wp.window_id,
                wp.selected,
                wp.cursor_kind,
                wp.cursor_bar_width,
                wp.cursor_color,
                window_cursor_type,
            );
            window_params.push(wp);
        }
    }

    if window_params.len() > 1 {
        tracing::debug!(
            "collect_layout_params: {} leaf windows, root bounds=({},{} {}x{})",
            window_params.len(),
            frame.root_window.bounds().x,
            frame.root_window.bounds().y,
            frame.root_window.bounds().width,
            frame.root_window.bounds().height,
        );
    }

    // Add minibuffer window if present.
    if let Some(mini_leaf) = &frame.minibuffer_leaf {
        let buf_id = mini_leaf.buffer_id();
        let buffer = buf_id.and_then(|id| evaluator.buffer_manager().get(id));
        if let Some(buffer) = buffer {
            let is_selected = frame_is_selected && frame.selected_window == mini_leaf.id();
            let window_cursor_type = evaluator.frame_manager().window_cursor_type(mini_leaf.id());
            if let Some(wp) = window_params_from_neovm(
                mini_leaf,
                buffer,
                frame,
                evaluator.obarray(),
                evaluator.face_table(),
                default_font_ascent,
                is_selected,
                true,
                window_cursor_type,
            ) {
                tracing::debug!(
                    "layout window cursor: win={} selected={} minibuffer=true kind={:?} width={} color=#{:06x} window-cursor-type={:?}",
                    wp.window_id,
                    wp.selected,
                    wp.cursor_kind,
                    wp.cursor_bar_width,
                    wp.cursor_color,
                    window_cursor_type,
                );
                tracing::debug!(
                    "  minibuffer id={} bounds=({},{} {}x{})",
                    wp.window_id,
                    wp.bounds.x,
                    wp.bounds.y,
                    wp.bounds.width,
                    wp.bounds.height,
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
pub(crate) struct RustBufferAccess<'a, B: LayoutBufferView> {
    buffer: &'a B,
}

impl<'a, B: LayoutBufferView> RustBufferAccess<'a, B> {
    /// Create a new buffer accessor.
    pub fn new(buffer: &'a B) -> Self {
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
        buffer_charpos_to_bytepos(self.buffer, charpos as usize) as i64
    }

    /// Convert a GNU Lisp-visible buffer position to a byte position.
    ///
    /// GNU Lisp positions are 1-based, so this is only appropriate for
    /// values coming from Lisp APIs such as `minibuffer-prompt-end`.
    pub fn lisp_charpos_to_bytepos(&self, charpos: i64) -> i64 {
        if charpos <= 1 {
            return 0;
        }
        buffer_charpos_to_bytepos(self.buffer, (charpos - 1) as usize) as i64
    }

    /// Copy buffer text bytes in the range `[byte_from, byte_to)` into `out`.
    ///
    /// Uses the efficient `copy_bytes_to` method on the gap buffer.
    pub fn copy_text(&self, byte_from: i64, byte_to: i64, out: &mut Vec<u8>) {
        let from = (byte_from as usize).min(self.buffer.layout_text().len());
        let to = (byte_to as usize).min(self.buffer.layout_text().len());
        if from >= to {
            out.clear();
            return;
        }
        self.buffer.layout_text().copy_bytes_to(from, to, out);
    }

    /// Count the number of newlines in `[byte_from, byte_to)`.
    ///
    /// Used for line number display.
    pub fn count_lines(&self, byte_from: i64, byte_to: i64) -> i64 {
        let from = (byte_from as usize).min(self.buffer.layout_text().len());
        let to = (byte_to as usize).min(self.buffer.layout_text().len());
        if from >= to {
            return 0;
        }
        // Count newlines by iterating byte by byte
        let mut count: i64 = 0;
        for pos in from..to {
            if self.buffer.layout_text().byte_at(pos) == b'\n' {
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
        if pos < self.buffer.layout_text().len() {
            Some(self.buffer.layout_text().byte_at(pos))
        } else {
            None
        }
    }

    /// Get the buffer's narrowed beginning (begv) as byte position.
    pub fn begv(&self) -> i64 {
        self.buffer.layout_point_min_byte() as i64
    }

    /// Convert an absolute byte position to the layout engine's internal
    /// 0-based char position space.
    pub fn bytepos_to_charpos(&self, bytepos: i64) -> i64 {
        if bytepos <= 0 {
            return 0;
        }
        buffer_bytepos_to_charpos(self.buffer, bytepos as usize) as i64
    }

    /// Get the buffer's narrowed end (zv) as byte position.
    pub fn zv(&self) -> i64 {
        self.buffer.layout_point_max_byte() as i64
    }
}

/// Text property and overlay accessor for the layout engine.
///
/// Wraps a reference to a neovm-core `Buffer` and provides query methods
/// for invisible text, display properties, overlay strings, and other
/// text property-based features.
pub(crate) struct RustTextPropAccess<'a, B: LayoutBufferView> {
    buffer: &'a B,
}

fn buffer_charpos_to_bytepos<B: LayoutBufferView>(buffer: &B, charpos: usize) -> usize {
    buffer
        .layout_text()
        .char_to_byte(charpos.min(buffer.layout_point_max_char()))
}

fn buffer_bytepos_to_charpos<B: LayoutBufferView>(buffer: &B, bytepos: usize) -> usize {
    buffer
        .layout_text()
        .byte_to_char(bytepos.min(buffer.layout_point_max_byte()))
        .min(buffer.layout_point_max_char())
}

impl<'a, B: LayoutBufferView> RustTextPropAccess<'a, B> {
    /// Create a new text property accessor.
    pub fn new(buffer: &'a B) -> Self {
        Self { buffer }
    }

    /// Check if text at `charpos` is invisible.
    ///
    /// Returns `(is_invisible, next_visible_pos)`.
    /// `next_visible_pos` is the next char position where visibility might change.
    /// If no change is found, returns `buffer.zv` as the next boundary.
    pub fn check_invisible(&self, charpos: i64) -> (bool, i64) {
        let bytepos = buffer_charpos_to_bytepos(self.buffer, charpos.max(0) as usize);
        let invis = self
            .buffer
            .layout_text()
            .text_props_get_property(bytepos, "invisible");

        let is_invisible = match invis {
            Some(v) if v.is_nil() => false,
            None => false,
            Some(_) => true, // Any non-nil value means invisible
        };

        // Find the next position where the invisible property changes
        let next_change = self
            .buffer
            .layout_text()
            .text_props_next_change(bytepos)
            .map(|next| buffer_bytepos_to_charpos(self.buffer, next))
            .unwrap_or(self.buffer.layout_point_max_char());

        (is_invisible, next_change as i64)
    }

    /// Check for a display text property at `charpos`.
    ///
    /// Returns the display property value if present, along with the
    /// next position where display properties change.
    pub fn check_display_prop(&self, charpos: i64) -> (Option<Value>, i64) {
        let bytepos = buffer_charpos_to_bytepos(self.buffer, charpos.max(0) as usize);
        let display = self
            .buffer
            .layout_text()
            .text_props_get_property(bytepos, "display");

        let next_change = self
            .buffer
            .layout_text()
            .text_props_next_change(bytepos)
            .map(|next| buffer_bytepos_to_charpos(self.buffer, next))
            .unwrap_or(self.buffer.layout_point_max_char());

        (display, next_change as i64)
    }

    /// Check for line-spacing text property at `charpos`.
    ///
    /// Returns extra line spacing in pixels (0.0 if no property).
    pub fn check_line_spacing(&self, charpos: i64, base_height: f32) -> f32 {
        let bytepos = buffer_charpos_to_bytepos(self.buffer, charpos.max(0) as usize);
        match self
            .buffer
            .layout_text()
            .text_props_get_property(bytepos, "line-spacing")
        {
            Some(v) if v.is_fixnum() => v.as_fixnum().unwrap() as f32,
            Some(v) if v.is_float() => {
                let f = v.xfloat();
                if f < 1.0 {
                    // Fraction of base height
                    base_height * (f as f32)
                } else {
                    f as f32
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
    pub fn overlay_strings_at(
        &self,
        charpos: i64,
    ) -> (
        Vec<(Vec<u8>, neovm_core::emacs_core::value::Value)>,
        Vec<(Vec<u8>, neovm_core::emacs_core::value::Value)>,
    ) {
        let bytepos = buffer_charpos_to_bytepos(self.buffer, charpos.max(0) as usize);
        let mut before = Vec::new();
        let mut after = Vec::new();

        // Get all overlays covering this position
        let overlay_ids = self.buffer.layout_overlays().overlays_at(bytepos);
        for oid in &overlay_ids {
            let oid = *oid;
            // Before-string: from overlays that START at this position
            if let Some(start) = self.buffer.layout_overlays().overlay_start(oid) {
                if start == bytepos {
                    if let Some(val) = self
                        .buffer
                        .layout_overlays()
                        .overlay_get_named(oid, "before-string")
                    {
                        if let Some(s) = value_as_string(&val) {
                            before.push((s.as_bytes().to_vec(), oid));
                        }
                    }
                }
            }

            // After-string: from overlays that END at this position
            if let Some(end) = self.buffer.layout_overlays().overlay_end(oid) {
                if end == bytepos {
                    if let Some(val) = self
                        .buffer
                        .layout_overlays()
                        .overlay_get_named(oid, "after-string")
                    {
                        if let Some(s) = value_as_string(&val) {
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
        if bytepos > 0 {
            let nearby_ids = self
                .buffer
                .layout_overlays()
                .overlays_in(bytepos.saturating_sub(1), bytepos + 1);
            for oid in &nearby_ids {
                let oid = *oid;
                if let Some(end) = self.buffer.layout_overlays().overlay_end(oid) {
                    if end == bytepos {
                        // Check we haven't already processed this overlay
                        if !overlay_ids.contains(&oid) {
                            if let Some(val) = self
                                .buffer
                                .layout_overlays()
                                .overlay_get_named(oid, "after-string")
                            {
                                if let Some(s) = value_as_string(&val) {
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
    /// Test-only helper for direct property-table regression coverage.
    #[cfg(test)]
    pub fn next_property_change(&self, charpos: i64) -> i64 {
        let bytepos = buffer_charpos_to_bytepos(self.buffer, charpos.max(0) as usize);
        self.buffer
            .layout_text()
            .text_props_next_change(bytepos)
            .map(|next| buffer_bytepos_to_charpos(self.buffer, next))
            .unwrap_or(self.buffer.layout_point_max_char()) as i64
    }

    /// Get a specific text property at a position.
    pub fn get_property(&self, charpos: i64, name: &str) -> Option<Value> {
        let bytepos = buffer_charpos_to_bytepos(self.buffer, charpos.max(0) as usize);
        self.buffer
            .layout_text()
            .text_props_get_property(bytepos, name)
    }

    /// Get a text property at `charpos` as a string.
    ///
    /// Returns `Some(String)` if the property exists and is a string value,
    /// `None` otherwise.
    pub fn get_text_prop_string(&self, charpos: i64, prop_name: &str) -> Option<String> {
        self.get_property(charpos, prop_name)
            .and_then(|v| v.as_runtime_string_owned())
    }
}

/// Helper: extract a string from a Value.
///
/// Tagged strings can be read directly from the Value. Other types return None.
fn value_as_string(val: &Value) -> Option<String> {
    match val.kind() {
        ValueKind::String => val.as_runtime_string_owned(),
        ValueKind::Nil => None,
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// ResolvedFace — pure-Rust equivalent of FaceDataFFI
// ---------------------------------------------------------------------------

/// Convert a neovm-core `Color` to a packed sRGB pixel (0x00RRGGBB).
fn color_to_pixel(c: &NeoColor) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | (c.b as u32)
}

/// Check if two colors are perceptually close.
///
/// GNU Emacs uses this for `:distant-foreground`: when the foreground
/// is too similar to the background, swap to the distant foreground
/// for readability.  Uses simple RGB distance threshold.
fn colors_close(a: u32, b: u32) -> bool {
    let ar = (a >> 16) & 0xFF;
    let ag = (a >> 8) & 0xFF;
    let ab = a & 0xFF;
    let br = (b >> 16) & 0xFF;
    let bg = (b >> 8) & 0xFF;
    let bb = b & 0xFF;
    let dr = ar.abs_diff(br) as u32;
    let dg = ag.abs_diff(bg) as u32;
    let db = ab.abs_diff(bb) as u32;
    // Weighted Euclidean distance (human perception weights R more than B)
    // Threshold ~30 in each channel ≈ 2700 squared distance
    (dr * dr * 3 + dg * dg * 4 + db * db * 2) < 3000
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
    fn face_spec_is_plist(items: &[Value]) -> bool {
        match items.first() {
            Some(v) if v.is_keyword() => true,
            Some(item) => item
                .as_symbol_name()
                .is_some_and(|name| name.starts_with(':')),
            None => false,
        }
    }

    /// Create a new `FaceResolver`.
    ///
    /// Clones the `FaceTable` so the resolver owns its data and does not
    /// borrow from the `Context`.  This allows `layout_window_rust` to
    /// take `&mut Context` for `format-mode-line` evaluation while
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
            Some(FaceHeight::Absolute(tenths)) => face_height_to_pixels(*tenths),
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

    fn apply_inline_face_over(&self, base: &ResolvedFace, face: &NeoFace) -> ResolvedFace {
        let mut rf = base.clone();

        if let Some(c) = &face.foreground {
            rf.fg = color_to_pixel(c);
        }
        if let Some(c) = &face.background {
            rf.bg = color_to_pixel(c);
        }
        if face.inverse_video == Some(true) {
            std::mem::swap(&mut rf.fg, &mut rf.bg);
        }

        if let Some(family) = &face.family {
            rf.font_family = family.clone();
        }
        if let Some(weight) = face.weight {
            rf.font_weight = weight.0;
        }
        if let Some(slant) = face.slant {
            rf.italic = slant.is_italic();
        }
        if let Some(height) = &face.height {
            match height {
                FaceHeight::Absolute(tenths) => {
                    rf.font_size = face_height_to_pixels(*tenths);
                }
                FaceHeight::Relative(factor) => {
                    rf.font_size = (rf.font_size * *factor as f32).max(1.0);
                }
            }
        }

        if let Some(underline) = &face.underline {
            rf.underline_style = underline_style_to_u8(&underline.style);
            rf.underline_color = underline.color.as_ref().map(color_to_pixel).unwrap_or(0);
        }
        if let Some(overline) = face.overline {
            rf.overline = overline;
        }
        if let Some(color) = &face.overline_color {
            rf.overline_color = color_to_pixel(color);
        }
        if let Some(strike) = face.strike_through {
            rf.strike_through = strike;
        }
        if let Some(color) = &face.strike_through_color {
            rf.strike_through_color = color_to_pixel(color);
        }
        if let Some(box_border) = &face.box_border {
            rf.box_type = 1;
            rf.box_color = box_border
                .color
                .as_ref()
                .map(color_to_pixel)
                .unwrap_or(rf.fg);
            rf.box_line_width = box_border.width;
        }
        if let Some(extend) = face.extend {
            rf.extend = extend;
        }
        if face.overstrike {
            rf.overstrike = true;
        }

        // Distant-foreground: swap fg when too close to bg
        if let Some(dfg) = &face.distant_foreground {
            if colors_close(rf.fg, rf.bg) {
                rf.fg = color_to_pixel(dfg);
            }
        }

        rf
    }

    fn apply_named_face_over(&self, base: &ResolvedFace, name: &str) -> ResolvedFace {
        let resolved = self.resolve_named_face(name);
        let default = self.default_face();
        let mut merged = base.clone();

        if resolved.fg != default.fg {
            merged.fg = resolved.fg;
        }
        if resolved.bg != default.bg {
            merged.bg = resolved.bg;
        }
        if !resolved.font_family.is_empty() && resolved.font_family != default.font_family {
            merged.font_family = resolved.font_family;
        }
        if resolved.font_weight != default.font_weight {
            merged.font_weight = resolved.font_weight;
        }
        if resolved.italic != default.italic {
            merged.italic = resolved.italic;
        }
        if (resolved.font_size - default.font_size).abs() > f32::EPSILON {
            merged.font_size = resolved.font_size;
        }
        if resolved.underline_style != default.underline_style {
            merged.underline_style = resolved.underline_style;
            merged.underline_color = resolved.underline_color;
        }
        if resolved.strike_through != default.strike_through {
            merged.strike_through = resolved.strike_through;
            merged.strike_through_color = resolved.strike_through_color;
        }
        if resolved.overline != default.overline {
            merged.overline = resolved.overline;
            merged.overline_color = resolved.overline_color;
        }
        if resolved.box_type != default.box_type {
            merged.box_type = resolved.box_type;
            merged.box_color = resolved.box_color;
            merged.box_line_width = resolved.box_line_width;
        }
        if resolved.extend != default.extend {
            merged.extend = resolved.extend;
        }
        if resolved.overstrike != default.overstrike {
            merged.overstrike = resolved.overstrike;
        }

        merged
    }

    fn face_name_from_value<'a>(value: &'a Value) -> Option<&'a str> {
        match value.kind() {
            ValueKind::Symbol(_) => value.as_symbol_name(),
            ValueKind::String => value.as_str(),
            _ => None,
        }
    }

    fn is_filtered_face_spec(items: &[Value]) -> bool {
        match items.first() {
            Some(v) if v.is_keyword() => items
                .first()
                .and_then(|v| v.as_symbol_name())
                .is_some_and(|name| name == "filtered"),
            Some(item) => item
                .as_symbol_name()
                .is_some_and(|name| name == ":filtered"),
            None => false,
        }
    }

    fn buffer_face_remapping_specs<B: LayoutBufferView>(
        buffer: &B,
        face_name: &str,
    ) -> Option<Value> {
        let mut cursor = buffer.layout_buffer_local_value("face-remapping-alist")?;
        loop {
            if !cursor.is_cons() {
                return None;
            }
            let entry_car = cursor.cons_car();
            let entry_cdr = cursor.cons_cdr();
            if entry_car.is_cons() {
                let mapping_car = entry_car.cons_car();
                let mapping_cdr = entry_car.cons_cdr();
                if Self::face_name_from_value(&mapping_car).is_some_and(|name| name == face_name) {
                    return Some(mapping_cdr);
                }
            }
            cursor = entry_cdr;
        }
    }

    fn resolve_buffer_face_value_over<B: LayoutBufferView>(
        &self,
        buffer: &B,
        base: &ResolvedFace,
        val: &Value,
        remap_stack: &mut Vec<String>,
    ) -> Option<ResolvedFace> {
        match val.kind() {
            ValueKind::Nil => None,
            ValueKind::Symbol(_) | ValueKind::String => {
                let name = Self::face_name_from_value(val)?;
                if name == "nil" {
                    return None;
                }

                if !remap_stack.iter().any(|active| active == name)
                    && let Some(specs) = Self::buffer_face_remapping_specs(buffer, name)
                {
                    remap_stack.push(name.to_string());
                    let remapped =
                        self.resolve_buffer_face_value_over(buffer, base, &specs, remap_stack);
                    remap_stack.pop();
                    if remapped.is_some() {
                        return remapped;
                    }
                }

                Some(self.apply_named_face_over(base, name))
            }
            ValueKind::Cons => {
                let items = list_to_vec(val)?;
                if items.is_empty() {
                    return None;
                }
                if Self::is_filtered_face_spec(&items) {
                    return None;
                }
                if Self::face_spec_is_plist(&items) {
                    let inline = NeoFace::from_plist("--inline--", &items);
                    return Some(self.apply_inline_face_over(base, &inline));
                }

                let mut current = base.clone();
                let mut changed = false;
                for item in items.iter().rev() {
                    if let Some(next) =
                        self.resolve_buffer_face_value_over(buffer, &current, item, remap_stack)
                    {
                        current = next;
                        changed = true;
                    }
                }
                changed.then_some(current)
            }
            _ => None,
        }
    }

    fn resolve_buffer_default_face<B: LayoutBufferView>(&self, buffer: &B) -> ResolvedFace {
        let mut remap_stack = Vec::new();
        self.resolve_buffer_face_value_over(
            buffer,
            &self.default_face,
            &Value::symbol("default"),
            &mut remap_stack,
        )
        .unwrap_or_else(|| self.default_face.clone())
    }

    pub fn resolve_face_value_over(
        &self,
        base: &ResolvedFace,
        val: &Value,
    ) -> Option<ResolvedFace> {
        match val.kind() {
            ValueKind::Nil => None,
            ValueKind::Symbol(_) => {
                let name = val.as_symbol_name()?;
                (name != "nil").then(|| self.apply_named_face_over(base, name))
            }
            ValueKind::Cons => {
                let items = list_to_vec(val)?;
                if items.is_empty() {
                    return None;
                }
                if Self::is_filtered_face_spec(&items) {
                    return None;
                }
                if Self::face_spec_is_plist(&items) {
                    let inline = NeoFace::from_plist("--inline--", &items);
                    return Some(self.apply_inline_face_over(base, &inline));
                }

                let mut current = base.clone();
                let mut changed = false;
                for item in items.iter().rev() {
                    if let Some(next) = self.resolve_face_value_over(&current, item) {
                        current = next;
                        changed = true;
                    }
                }
                changed.then_some(current)
            }
            _ => None,
        }
    }

    /// Resolve face attributes at a buffer position.
    ///
    /// Reads "face" and "font-lock-face" text properties, collects overlay
    /// faces (sorted by priority), merges them via `FaceTable`, and produces
    /// a fully-realized `ResolvedFace`.
    ///
    /// `next_check` is set to the minimum of all property change positions
    /// so the caller can skip per-character lookups until that boundary.
    pub(crate) fn face_at_pos<B: LayoutBufferView>(
        &self,
        buffer: &B,
        charpos: usize,
        next_check: &mut usize,
    ) -> ResolvedFace {
        let bytepos = buffer_charpos_to_bytepos(buffer, charpos);
        let mut min_next = buffer.layout_point_max_char();
        let mut resolved = self.resolve_buffer_default_face(buffer);
        let mut remap_stack = Vec::new();

        // 1. "face" text property
        if let Some(val) = buffer
            .layout_text()
            .text_props_get_property(bytepos, "face")
        {
            if let Some(next) =
                self.resolve_buffer_face_value_over(buffer, &resolved, &val, &mut remap_stack)
            {
                resolved = next;
            }
        }
        // Update next_check from text property boundaries
        if let Some(nc) = buffer.layout_text().text_props_next_change(bytepos) {
            min_next = min_next.min(buffer_bytepos_to_charpos(buffer, nc));
        }

        // 2. "font-lock-face" text property
        if let Some(val) = buffer
            .layout_text()
            .text_props_get_property(bytepos, "font-lock-face")
        {
            if let Some(next) =
                self.resolve_buffer_face_value_over(buffer, &resolved, &val, &mut remap_stack)
            {
                resolved = next;
            }
        }

        // 3. Overlay faces (sorted by priority, lowest first)
        let overlay_ids = buffer.layout_overlays().overlays_at(bytepos);
        if !overlay_ids.is_empty() {
            let mut overlay_faces: Vec<(i64, Value)> = Vec::new();
            for oid in &overlay_ids {
                let oid = *oid;
                // Update next_check from overlay boundaries
                if let Some(end) = buffer.layout_overlays().overlay_end(oid) {
                    if end > bytepos {
                        min_next = min_next.min(buffer_bytepos_to_charpos(buffer, end));
                    }
                }
                // Get priority (default 0)
                let priority = buffer
                    .layout_overlays()
                    .overlay_get_named(oid, "priority")
                    .and_then(|v| v.as_int())
                    .unwrap_or(0);
                // Get face
                if let Some(val) = buffer.layout_overlays().overlay_get_named(oid, "face") {
                    overlay_faces.push((priority, val));
                }
            }
            // Sort by priority (ascending), so higher priority overlays
            // are merged later and override earlier ones.
            overlay_faces.sort_by_key(|(pri, _)| *pri);
            for (_pri, face_value) in overlay_faces {
                if let Some(next) = self.resolve_buffer_face_value_over(
                    buffer,
                    &resolved,
                    &face_value,
                    &mut remap_stack,
                ) {
                    resolved = next;
                }
            }
        }

        // Also consider overlay boundaries so next_check doesn't skip past
        // positions where an overlay starts or ends.
        if let Some(nb) = buffer.layout_overlays().next_boundary_after(bytepos) {
            min_next = min_next.min(buffer_bytepos_to_charpos(buffer, nb));
        }

        *next_check = min_next;
        resolved
    }

    /// Extract face name(s) from a Lisp Value.
    ///
    /// Face property values can be:
    /// - A symbol naming a face: `Value::Symbol(id)` -> `vec!["face-name"]`
    /// - A list of symbols: each element is a face name
    /// - Nil: no face
    /// - Otherwise: empty vec (unrecognized format)
    pub fn resolve_face_value(val: &Value) -> Vec<String> {
        match val.kind() {
            ValueKind::Nil => Vec::new(),
            ValueKind::Symbol(_) => {
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
            ValueKind::Cons => {
                // Could be a list of face names, or a plist of face attributes.
                if let Some(items) = list_to_vec(val) {
                    // Check if first item is a keyword (plist like :foreground "red")
                    if Self::face_spec_is_plist(&items) {
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
        let items = list_to_vec(val)?;
        Some(NeoFace::from_plist("--inline--", &items))
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
                    rf.font_size = face_height_to_pixels(*tenths);
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

        // Distant-foreground: GNU Emacs (xfaces.c) uses this when the
        // foreground is too close to the background, improving readability.
        // Check if fg ≈ bg and substitute distant-foreground if available.
        if let Some(dfg) = &face.distant_foreground {
            let dfg_pixel = color_to_pixel(dfg);
            if colors_close(rf.fg, rf.bg) {
                rf.fg = dfg_pixel;
            }
        }

        rf
    }

    /// Resolve a face from a Lisp Value (as found in overlay "face" property).
    ///
    /// Returns None if the value doesn't specify any known face names.
    pub fn resolve_face_from_value(&self, val: &Value) -> Option<ResolvedFace> {
        self.resolve_face_value_over(&self.default_face, val)
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
#[path = "neovm_bridge_test.rs"]
mod tests;
