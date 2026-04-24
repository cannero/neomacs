//! The Rust layout engine — Phase 1+2: Monospace layout with face resolution.
//!
//! Reads buffer text and display state from neovm-core, resolves faces per
//! character position, computes line breaks, positions glyphs on a fixed-width
//! grid, and publishes `FrameDisplayState` snapshots for render backends.

use super::display_status_line::*;
use super::font_metrics::{FontMetrics, FontMetricsService};
use super::gui_chrome::{collect_gui_menu_bar_items, collect_gui_tool_bar_items};
use super::hit_test::*;
use super::types::*;
use super::unicode::*;
use super::window_output::{RowMetricsSnapshot, WindowOutputEmitter};
use neomacs_display_protocol::frame_glyphs::{
    CursorStyle, DisplaySlotId, FrameGlyphBuffer, GlyphRowRole, PhysCursor, WindowEffectHint,
    WindowInfo, WindowTransitionHint, WindowTransitionKind,
};
use neomacs_display_protocol::types::{Color, Rect};
use neovm_core::buffer::BufferId;
use neovm_core::emacs_core::eval::{ImageResolveRequest, ImageResolveSource};
use neovm_core::emacs_core::keymap::is_list_keymap;
use neovm_core::emacs_core::value::list_to_vec;
use neovm_core::emacs_core::{Context, Value};
use neovm_core::window::{
    DisplayRowSnapshot, WindowCursorKind, WindowCursorPos, WindowCursorSnapshot,
    WindowDisplaySnapshot,
};

/// Maximum number of characters in a ligature run before forced flush.
const MAX_LIGATURE_RUN_LEN: usize = 64;
/// Bound redisplay convergence work when point begins outside the visible span.
const MAX_WINDOW_VISIBILITY_RETRIES: usize = 128;

/// Buffer for accumulating same-face text runs for ligature shaping.
struct LigatureRunBuffer {
    chars: Vec<char>,
    advances: Vec<f32>,
    start_x: f32,
    start_y: f32,
    face_h: f32,
    face_ascent: f32,
    face_id: u32,
    total_advance: f32,
    is_overlay: bool,
    height_scale: f32,
}

#[derive(Clone, Copy, Debug)]
struct CapturedCursorInfo {
    x: f32,
    y: f32,
    face_w: f32,
    face_h: f32,
    face_ascent: f32,
    bg: Color,
    byte_idx: usize,
    col: usize,
    face_id: u32,
    face_space_w: f32,
    matrix_row: usize,
    slot_width: Option<f32>,
    stretch_like: bool,
}

#[derive(Clone, Copy, Debug)]
struct ResolvedCursorGeometry {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    ascent: f32,
    row: usize,
    col: usize,
    style: CursorStyle,
    color: Color,
    cursor_fg: Color,
}

fn window_cursor_kind(style: CursorStyle) -> WindowCursorKind {
    match style {
        CursorStyle::FilledBox => WindowCursorKind::FilledBox,
        CursorStyle::Hollow => WindowCursorKind::HollowBox,
        CursorStyle::Bar(_) => WindowCursorKind::Bar,
        CursorStyle::Hbar(_) => WindowCursorKind::Hbar,
    }
}

fn capture_cursor_info(target: &mut Option<CapturedCursorInfo>, info: CapturedCursorInfo) {
    if target.is_none() {
        *target = Some(info);
    }
}

fn update_cursor_info_for_main_char(
    target: &mut Option<CapturedCursorInfo>,
    byte_idx: usize,
    advance: f32,
) {
    let Some(cursor) = target.as_mut() else {
        return;
    };
    if cursor.byte_idx != byte_idx {
        return;
    }
    cursor.slot_width = Some(advance.max(1.0));
}

fn slot_char_width(ch: char, face_char_w: f32) -> f32 {
    if is_cluster_extender(ch) {
        0.0
    } else if is_wide_char(ch) {
        2.0 * face_char_w
    } else {
        face_char_w
    }
}

fn control_char_display_pair(ch: char, ctl_arrow: bool) -> Option<(char, char)> {
    if !ctl_arrow {
        return None;
    }

    let code = ch as u32;
    if code <= 0x1f || code == 0x7f {
        let suffix = char::from_u32(code ^ 0x40).unwrap_or('?');
        Some(('^', suffix))
    } else {
        None
    }
}

fn finish_text_row(
    builder: &mut crate::matrix_builder::GlyphMatrixBuilder,
    output_emitter: &mut WindowOutputEmitter,
    row_y: f32,
    row_height: f32,
    row_ascent: f32,
) {
    builder.set_current_row_metrics(row_y, row_height, row_ascent);
    output_emitter.push_text_row(row_y, row_height, row_ascent);
}

#[allow(dead_code)]
fn eval_status_line_format(
    evaluator: &mut neovm_core::emacs_core::Context,
    format_symbol: &str,
    window_id: i64,
    buffer_id: u64,
    target_cols: usize,
) -> Option<String> {
    eval_status_line_format_value(evaluator, format_symbol, window_id, buffer_id, target_cols)
        .and_then(|val| val.as_runtime_string_owned())
        .filter(|s| !s.is_empty())
}

fn eval_status_line_format_value(
    evaluator: &mut neovm_core::emacs_core::Context,
    format_symbol: &str,
    window_id: i64,
    buffer_id: u64,
    target_cols: usize,
) -> Option<Value> {
    evaluator.setup_thread_locals();
    // GNU Emacs (xdisp.c:28187): format-mode-line reads the format
    // variable from the TARGET buffer, not the caller's current
    // buffer. We must read the buffer-local value of mode-line-format
    // from the specified buffer BEFORE calling the walker.
    let format_value = evaluator
        .buffer_manager()
        .get(BufferId(buffer_id))
        .and_then(|buf| buf.buffer_local_value(format_symbol))
        .unwrap_or_else(|| {
            // Fall back to the global default
            evaluator
                .obarray()
                .symbol_value(format_symbol)
                .copied()
                .unwrap_or(Value::NIL)
        });
    // GNU `display_mode_line` (xdisp.c:27911) runs the mode-line
    // walker in `MODE_LINE_DISPLAY` mode, which makes `%-` expand to
    // dashes filling the remaining row width. Our layout engine is the
    // equivalent redisplay path, so we call
    // `format_mode_line_for_display` directly rather than going
    // through the Lisp-facing `format-mode-line` builtin (which uses
    // `MODE_LINE_STRING` and returns `"--"` for `%-`).
    //
    // `target_cols` is the window's width in character cells, which
    // the DISPLAY walker uses to size the dash fill for `%-`.
    let rendered = neovm_core::emacs_core::xdisp::format_mode_line_for_display(
        evaluator,
        format_value,
        Value::make_window(window_id as u64),
        Value::make_buffer(BufferId(buffer_id)),
        target_cols,
    );
    if rendered
        .as_runtime_string_owned()
        .is_some_and(|s| !s.is_empty())
    {
        Some(rendered)
    } else {
        None
    }
}

fn tab_bar_menu_item_caption(entry: Value) -> Option<String> {
    if let Some(items) = list_to_vec(&entry) {
        if items.get(1).and_then(|v| v.as_symbol_name()) == Some("menu-item") {
            return items.get(2)?.as_runtime_string_owned();
        }
    }

    if !entry.is_cons() {
        return None;
    }
    let pair_cdr = entry.cons_cdr();
    let items = list_to_vec(&pair_cdr)?;
    if items.first().and_then(|v| v.as_symbol_name()) != Some("menu-item") {
        return None;
    }
    items.get(1)?.as_runtime_string_owned()
}

struct BuiltTabBar {
    text: String,
    items: Vec<neomacs_display_protocol::ui_types::TabBarItem>,
}

fn build_tab_bar_display(
    evaluator: &mut neovm_core::emacs_core::Context,
    frame_id: u64,
) -> Option<BuiltTabBar> {
    evaluator.setup_thread_locals();
    if !evaluator.obarray().fboundp("tab-bar-make-keymap-1") {
        return None;
    }

    let saved_frame = evaluator
        .eval_form(Value::list(vec![Value::symbol("selected-frame")]))
        .ok();
    let saved_window = evaluator
        .eval_form(Value::list(vec![Value::symbol("selected-window")]))
        .ok();
    let saved_buffer = evaluator
        .buffer_manager()
        .current_buffer()
        .map(|buffer| buffer.id);

    evaluator
        .eval_form(Value::list(vec![
            Value::symbol("select-frame"),
            Value::make_frame(frame_id),
            Value::NIL,
        ]))
        .ok()?;

    let result = evaluator
        .eval_form(Value::list(vec![Value::symbol("tab-bar-make-keymap-1")]))
        .ok()
        .and_then(|keymap| list_to_vec(&keymap))
        .and_then(|entries| {
            let mut text = String::new();
            let mut items = Vec::new();
            for (index, entry) in entries.iter().enumerate() {
                if index == 0 && entry.is_symbol_named("keymap") {
                    continue;
                }

                if is_list_keymap(entry) {
                    break;
                }

                if let Some(caption) = tab_bar_menu_item_caption(*entry) {
                    text.push_str(&caption);
                    items.push(neomacs_display_protocol::ui_types::TabBarItem {
                        index: items.len() as u32,
                        label: caption,
                        help: String::new(),
                        enabled: true,
                        selected: false,
                        is_separator: false,
                    });
                }
            }

            (!text.is_empty()).then_some(BuiltTabBar { text, items })
        });

    if let Some(frame) = saved_frame {
        let _ = evaluator.eval_form(Value::list(vec![
            Value::symbol("select-frame"),
            frame,
            Value::NIL,
        ]));
    }
    if let Some(window) = saved_window {
        let _ = evaluator.eval_form(Value::list(vec![
            Value::symbol("select-window"),
            window,
            Value::NIL,
        ]));
    }
    if let Some(buffer_id) = saved_buffer {
        if evaluator.buffer_manager().get(buffer_id).is_some() {
            evaluator.buffer_manager_mut().set_current(buffer_id);
        }
    }

    result
}

impl LigatureRunBuffer {
    fn new() -> Self {
        Self {
            chars: Vec::with_capacity(MAX_LIGATURE_RUN_LEN),
            advances: Vec::with_capacity(MAX_LIGATURE_RUN_LEN),
            start_x: 0.0,
            start_y: 0.0,
            face_h: 0.0,
            face_ascent: 0.0,
            face_id: 0,
            total_advance: 0.0,
            is_overlay: false,
            height_scale: 0.0,
        }
    }

    fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    fn len(&self) -> usize {
        self.chars.len()
    }

    fn clear(&mut self) {
        self.chars.clear();
        self.advances.clear();
        self.total_advance = 0.0;
    }

    /// Push a character and its advance width into the run.
    fn push(&mut self, ch: char, advance: f32) {
        self.chars.push(ch);
        self.advances.push(advance);
        self.total_advance += advance;
    }

    /// Start a new run at the given position with the given face parameters.
    fn start(
        &mut self,
        x: f32,
        y: f32,
        face_h: f32,
        face_ascent: f32,
        face_id: u32,
        is_overlay: bool,
        height_scale: f32,
    ) {
        self.clear();
        self.start_x = x;
        self.start_y = y;
        self.face_h = face_h;
        self.face_ascent = face_ascent;
        self.face_id = face_id;
        self.is_overlay = is_overlay;
        self.height_scale = height_scale;
    }
}

/// Check if a character is a ligature-eligible symbol/punctuation.
/// Programming font ligatures only form between these characters.
#[inline]
#[allow(dead_code)]
fn is_ligature_char(ch: char) -> bool {
    matches!(
        ch,
        '!' | '#'
            | '$'
            | '%'
            | '&'
            | '*'
            | '+'
            | '-'
            | '.'
            | '/'
            | ':'
            | ';'
            | '<'
            | '='
            | '>'
            | '?'
            | '@'
            | '\\'
            | '^'
            | '|'
            | '~'
    )
}

/// Check if a run consists entirely of ligature-eligible characters.
/// Mixed runs (e.g., "arrow:" or "Font:") should NOT be composed,
/// only pure symbol runs (e.g., "->", "!=", "===").
#[inline]
#[allow(dead_code)]
fn run_is_pure_ligature(run: &LigatureRunBuffer) -> bool {
    run.chars.iter().all(|&ch| is_ligature_char(ch))
}

/// Flush the accumulated ligature run as either individual chars or a composed glyph.
///
/// NOTE: Glyph output has been migrated to `GlyphMatrixBuilder`. This function is now
/// a no-op retained only to keep call-sites compiling during the migration.
fn flush_run(_run: &LigatureRunBuffer, _ligatures: bool) {}

#[inline]
fn skip_to_newline(text: &[u8], byte_idx: &mut usize, charpos: &mut i64) -> bool {
    while *byte_idx < text.len() {
        let (ch, ch_len) = decode_utf8(&text[*byte_idx..]);
        if ch_len == 0 {
            break;
        }
        *byte_idx += ch_len;
        *charpos += 1;
        if ch == '\n' {
            return true;
        }
    }
    false
}

fn row_metrics_for_cursor(
    row_metrics: &[RowMetricsSnapshot],
    cursor_row: usize,
    current_row: usize,
    current_row_y: f32,
    current_row_height: f32,
    current_row_ascent: f32,
) -> RowMetricsSnapshot {
    row_metrics
        .iter()
        .find(|metric| metric.row == cursor_row)
        .copied()
        .unwrap_or(RowMetricsSnapshot {
            row: current_row,
            pixel_y: current_row_y,
            height: current_row_height.max(1.0),
            ascent: current_row_ascent.max(0.0).min(current_row_height.max(1.0)),
        })
}

fn resolve_cursor_vertical_metrics(
    cursor_y: f32,
    face_h: f32,
    face_ascent: f32,
    row_height: f32,
    row_ascent: f32,
    default_line_height: f32,
    ends_at_visible_eob: bool,
) -> (f32, f32, f32) {
    let row_height = row_height.max(1.0);
    let glyph_ascent = face_ascent.max(0.0).min(face_h.max(1.0));
    let glyph_descent = (face_h - glyph_ascent).max(0.0);
    let mut y = cursor_y;
    let mut ascent = row_ascent.max(0.0).min(row_height);

    // GNU's physical cursor follows the row baseline, but if the glyph under
    // point rises above that baseline, the cursor origin shifts upward to keep
    // the box aligned with the displayed glyph. End-of-buffer rows are the
    // exception because point can sit on an empty visual slot there.
    if !ends_at_visible_eob && ascent < glyph_ascent {
        y -= glyph_ascent - ascent;
        ascent = glyph_ascent.min(row_height);
    }

    let minimum_height = default_line_height.max(1.0).min(row_height);
    let height = (ascent + glyph_descent).max(minimum_height).min(row_height);
    (y, height, ascent.min(height))
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct AsciiWidthCacheKey {
    family: String,
    weight: u16,
    italic: bool,
    font_size: i32,
}

impl AsciiWidthCacheKey {
    fn new(family: &str, weight: u16, italic: bool, font_size: i32) -> Self {
        Self {
            family: family.to_string(),
            weight,
            italic,
            font_size,
        }
    }
}

fn next_window_start_from_visible_rows(
    rows: &[DisplayRowSnapshot],
    current_start: i64,
) -> Option<i64> {
    if rows.is_empty() {
        return None;
    }

    rows.iter()
        .rev()
        .filter_map(row_next_window_start_charpos)
        .find(|&pos| pos > current_start)
}

#[inline]
fn lisp_buffer_pos_to_charpos(pos: usize) -> i64 {
    pos.saturating_sub(1) as i64
}

#[inline]
fn row_start_charpos(row: &DisplayRowSnapshot) -> Option<i64> {
    row.start_buffer_pos.map(lisp_buffer_pos_to_charpos)
}

#[inline]
fn row_end_charpos(row: &DisplayRowSnapshot) -> Option<i64> {
    row.end_buffer_pos.map(lisp_buffer_pos_to_charpos)
}

#[inline]
fn row_next_window_start_charpos(row: &DisplayRowSnapshot) -> Option<i64> {
    row.end_buffer_pos
        .map(|pos| pos as i64)
        .or_else(|| row_start_charpos(row))
}

fn next_window_start_for_partially_visible_point_row(
    rows: &[DisplayRowSnapshot],
    point: i64,
    text_area_top: i64,
    text_area_bottom: i64,
    current_start: i64,
) -> Option<i64> {
    let text_area_height = text_area_bottom.saturating_sub(text_area_top);
    let point_row_index = rows.iter().position(|row| {
        let start = row_start_charpos(row).unwrap_or(i64::MAX);
        let end = row_end_charpos(row).unwrap_or(i64::MIN);
        start <= point && point <= end
    })?;
    let point_row = &rows[point_row_index];
    if point_row.height > text_area_height {
        return None;
    }

    let row_top = point_row.y;
    let row_bottom = point_row.y.saturating_add(point_row.height);
    if row_top >= text_area_top && row_bottom <= text_area_bottom {
        return None;
    }

    if row_bottom > text_area_bottom {
        let overflow = row_bottom.saturating_sub(text_area_bottom);
        let mut lifted = 0i64;
        for row in rows.iter().take(point_row_index) {
            lifted = lifted.saturating_add(row.height.max(1));
            let candidate = row_next_window_start_charpos(row);
            if lifted >= overflow
                && let Some(pos) = candidate
                && pos > current_start
            {
                return Some(pos);
            }
        }
    }

    None
}

fn next_window_start_for_point_line_continuation<B: super::neovm_bridge::LayoutBufferView>(
    rows: &[DisplayRowSnapshot],
    point: i64,
    current_start: i64,
    buf_access: &super::neovm_bridge::RustBufferAccess<'_, B>,
    buffer_size: i64,
) -> Option<i64> {
    let point_row_index = rows.iter().position(|row| {
        let start = row_start_charpos(row).unwrap_or(i64::MAX);
        let end = row_end_charpos(row).unwrap_or(i64::MIN);
        start <= point && point <= end
    })?;
    let point_row = rows.get(point_row_index)?;
    let point_is_visible_row_start =
        row_start_charpos(point_row).is_some_and(|start| start == point);

    for row in rows.iter().skip(point_row_index) {
        let end_pos = row.end_buffer_pos? as i64;
        let end_byte = buf_access.lisp_charpos_to_bytepos(end_pos);
        if matches!(buf_access.byte_at(end_byte), Some(b'\n')) {
            return None;
        }
        let next_pos = end_pos.saturating_add(1);
        if next_pos > buffer_size {
            return None;
        }

        let next_byte = buf_access.lisp_charpos_to_bytepos(next_pos);
        match buf_access.byte_at(next_byte) {
            Some(b'\n') | None => return None,
            Some(_) if std::ptr::eq(row, rows.last()?) => {
                if point_is_visible_row_start {
                    return point
                        .checked_sub(1)
                        .filter(|&new_start| new_start > current_start);
                }
                break;
            }
            Some(_) => {}
        }
    }

    if point_row_index + 1 < rows.len() {
        return None;
    }

    rows.iter()
        .skip(1)
        .find_map(row_next_window_start_charpos)
        .filter(|&pos| pos > current_start)
}

// ---------------------------------------------------------------------------
// Display property helpers
// ---------------------------------------------------------------------------

/// Check if a Value is a space display spec: a cons whose car is the symbol `space`.
/// e.g., `(space :width 5)` or `(space :align-to 40)`
fn is_display_space_spec(val: &neovm_core::emacs_core::Value) -> bool {
    if val.is_cons() {
        return val.cons_car().is_symbol_named("space");
    }
    false
}

/// Evaluate a `(space :width …)` or `(space :align-to …)` display
/// spec into a pixel width relative to `current_x`.
///
/// Replaces the old `parse_display_space_width` helper. Delegates the
/// actual expression evaluation to
/// [`crate::display_pixel_calc::calc_pixel_width_or_height`], the
/// faithful port of GNU `xdisp.c:30102`. Supports the full GNU
/// expression grammar: fixnum/float, symbols (`right`, `text`,
/// `left-fringe`, etc.), arithmetic forms `(+ …)`/`(- …)`,
/// pixel-literal `(NUM)`, and unit-scaled `(NUM . UNIT)`.
///
/// GNU's xdisp.c uses canonical frame column width for these numeric
/// units, not the currently scaled face width of the covered buffer
/// position.
///
/// Returns the canonical frame column width as a conservative default
/// when the spec is invalid or the evaluator can't resolve it.
fn eval_display_space_as_width(
    spec: &neovm_core::emacs_core::Value,
    current_x: f32,
    content_x: f32,
    face_char_w: f32,
    params: &WindowParams,
) -> f32 {
    use crate::display_pixel_calc::{PixelCalcContext, calc_pixel_width_or_height};

    let Some(items) = neovm_core::emacs_core::value::list_to_vec(spec) else {
        return face_char_w;
    };

    let pctx = PixelCalcContext {
        frame_column_width: params.char_width.max(1.0) as f64,
        frame_line_height: params.char_height.max(1.0) as f64,
        frame_res_x: 96.0,
        frame_res_y: 96.0,
        face_font_height: params.char_height.max(1.0) as f64,
        face_font_width: face_char_w as f64,
        text_area_left: params.text_bounds.x as f64,
        text_area_right: (params.text_bounds.x + params.text_bounds.width) as f64,
        text_area_width: params.text_bounds.width as f64,
        left_margin_left: (params.text_bounds.x
            - params.left_fringe_width
            - params.left_margin_width) as f64,
        left_margin_width: params.left_margin_width as f64,
        right_margin_left: (params.text_bounds.x
            + params.text_bounds.width
            + params.right_fringe_width) as f64,
        right_margin_width: params.right_margin_width as f64,
        left_fringe_width: params.left_fringe_width as f64,
        right_fringe_width: params.right_fringe_width as f64,
        fringes_outside_margins: false,
        scroll_bar_width: 0.0,
        scroll_bar_on_left: false,
        line_number_pixel_width: 0.0,
        symbol_values: std::collections::HashMap::new(),
    };

    // items[0] is the `space` symbol; walk the keyword-value plist
    // starting at index 1.
    let mut i = 1;
    while i + 1 < items.len() {
        let key = items[i];
        let val = items[i + 1];
        if key.is_symbol_named(":width") {
            if let Some(pixels) = calc_pixel_width_or_height(&pctx, &val, true, None) {
                return pixels as f32;
            }
            return params.char_width.max(1.0);
        }
        if key.is_symbol_named(":align-to") {
            let mut align_to: i32 = -1;
            if let Some(pixels) = calc_pixel_width_or_height(&pctx, &val, true, Some(&mut align_to))
            {
                // If the expression contained a symbol like `right`,
                // `align_to` was updated to that position and `pixels`
                // is the offset from it. Otherwise (numeric-only
                // :align-to N), `align_to` is still -1 and `pixels`
                // is a column-relative offset from `content_x`.
                let target_x = if align_to >= 0 {
                    align_to as f32 + pixels as f32
                } else {
                    content_x + pixels as f32
                };
                return (target_x - current_x).max(0.0);
            }
            return params.char_width.max(1.0);
        }
        i += 2;
    }
    params.char_width.max(1.0)
}

/// Check if a Value is an image display spec: a cons whose car is the symbol `image`.
/// e.g., `(image :type png :file "/path/to/image.png")`
fn is_display_image_spec(val: &neovm_core::emacs_core::Value) -> bool {
    if val.is_cons() {
        return val.cons_car().is_symbol_named("image");
    }
    false
}

#[derive(Clone, Debug)]
struct DisplayImageLayout {
    request: ImageResolveRequest,
    scale: f32,
}

fn parse_image_dimension(value: Value) -> Option<u32> {
    match value.kind() {
        neovm_core::emacs_core::value::ValueKind::Fixnum(_) => Some(value.as_int()?.max(0) as u32),
        neovm_core::emacs_core::value::ValueKind::Float => {
            Some(value.as_float()?.max(0.0).round() as u32)
        }
        _ => None,
    }
}

fn parse_image_scale(value: Value) -> Option<f32> {
    if value.is_symbol_named("default") {
        return None;
    }
    match value.kind() {
        neovm_core::emacs_core::value::ValueKind::Fixnum(_) => Some(value.as_int()?.max(0) as f32),
        neovm_core::emacs_core::value::ValueKind::Float => Some(value.as_float()?.max(0.0) as f32),
        _ => None,
    }
}

fn max_mini_window_lines(evaluator: &Context, frame_rows: f32) -> f32 {
    let raw = evaluator
        .obarray()
        .symbol_value("max-mini-window-height")
        .copied()
        .unwrap_or_else(|| Value::make_float(0.25));
    match raw.kind() {
        neovm_core::emacs_core::value::ValueKind::Float => {
            (frame_rows * raw.as_float().unwrap_or(0.25) as f32).max(1.0)
        }
        neovm_core::emacs_core::value::ValueKind::Fixnum(_) => raw.as_int().unwrap_or(1) as f32,
        _ => 1.0,
    }
}

fn parse_display_image_layout(prop_val: &Value) -> Option<DisplayImageLayout> {
    let items = list_to_vec(prop_val)?;
    if items.first()?.as_symbol_name() != Some("image") {
        return None;
    }

    let mut source = None;
    let mut max_width = 0u32;
    let mut max_height = 0u32;
    let mut scale = 1.0f32;

    let mut i = 1usize;
    while i + 1 < items.len() {
        let key = items[i].as_symbol_name();
        let value = items[i + 1];
        match key {
            Some(":file") => {
                source = value
                    .as_lisp_string()
                    .cloned()
                    .map(ImageResolveSource::File);
            }
            Some(":data") => {
                source = value
                    .as_lisp_string()
                    .map(|data| ImageResolveSource::Data(data.as_bytes().to_vec()));
            }
            Some(":width") => {
                max_width = parse_image_dimension(value).unwrap_or(max_width);
            }
            Some(":height") => {
                max_height = parse_image_dimension(value).unwrap_or(max_height);
            }
            Some(":scale") => {
                scale = parse_image_scale(value).unwrap_or(scale);
            }
            _ => {}
        }
        i += 2;
    }

    Some(DisplayImageLayout {
        request: ImageResolveRequest {
            source: source?,
            max_width,
            max_height,
            fg_color: 0,
            bg_color: 0,
        },
        scale,
    })
}

#[inline]
fn next_tab_stop_col(current_col: usize, tab_width: i32, tab_stop_list: &[i32]) -> usize {
    if !tab_stop_list.is_empty() {
        if let Some(&stop) = tab_stop_list
            .iter()
            .find(|&&stop| (stop as usize) > current_col)
        {
            return stop as usize;
        }
        let last = *tab_stop_list.last().unwrap() as usize;
        let tab_w = tab_width.max(1) as usize;
        if current_col >= last {
            return last + ((current_col - last) / tab_w + 1) * tab_w;
        }
        return last;
    }

    let tab_w = tab_width.max(1) as usize;
    ((current_col / tab_w) + 1) * tab_w
}

#[inline]
fn is_word_wrap_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t')
}

#[inline]
fn char_can_wrap_before_basic(ch: char) -> bool {
    !matches!(ch, ' ' | '\t' | '\n' | '\r')
}

#[inline]
fn char_can_wrap_after_basic(ch: char) -> bool {
    is_word_wrap_whitespace(ch)
}

#[inline]
fn cursor_point_columns(text: &[u8], byte_idx: usize, col: i32, params: &WindowParams) -> usize {
    if byte_idx >= text.len() {
        return 1;
    }

    let (ch, _) = decode_utf8(&text[byte_idx..]);
    match ch {
        '\t' => {
            let col_usize = col.max(0) as usize;
            let next_tab = next_tab_stop_col(col_usize, params.tab_width, &params.tab_stop_list)
                .max(col_usize + 1);
            next_tab - col_usize
        }
        '\n' | '\r' => 1,
        _ if is_cluster_extender(ch) => 0,
        _ if is_wide_char(ch) => 2,
        _ => 1,
    }
}

#[inline]
fn cursor_width_for_style(
    style: CursorStyle,
    text: &[u8],
    byte_idx: usize,
    col: i32,
    params: &WindowParams,
    face_char_w: f32,
) -> f32 {
    match style {
        CursorStyle::Bar(w) => w,
        CursorStyle::Hbar(_) => {
            cursor_point_columns(text, byte_idx, col, params) as f32 * face_char_w
        }
        _ => {
            if !params.x_stretch_cursor && byte_idx < text.len() {
                let (ch, _) = decode_utf8(&text[byte_idx..]);
                if ch == '\t' {
                    return params.char_width.max(1.0);
                }
            }
            cursor_point_columns(text, byte_idx, col, params) as f32 * face_char_w
        }
    }
}

#[inline]
unsafe fn cursor_point_advance(
    text: &[u8],
    byte_idx: usize,
    col: i32,
    params: &WindowParams,
    face_char_w: f32,
    face_space_w: f32,
    char_w: f32,
    font_size: i32,
    font_family: &str,
    font_weight: u16,
    font_italic: bool,
    ascii_width_cache: &mut std::collections::HashMap<AsciiWidthCacheKey, [f32; 128]>,
    font_metrics_svc: &mut Option<FontMetricsService>,
) -> Option<f32> {
    if byte_idx >= text.len() {
        return None;
    }

    let face_w = if face_char_w > 0.0 {
        face_char_w
    } else {
        char_w
    };
    let (ch, _) = decode_utf8(&text[byte_idx..]);
    match ch {
        '\n' | '\r' => Some(face_w),
        '\t' => {
            let col_usize = col.max(0) as usize;
            let next_tab = next_tab_stop_col(col_usize, params.tab_width, &params.tab_stop_list)
                .max(col_usize + 1);
            let tab_cols = next_tab.saturating_sub(col_usize).max(1);
            let space_w = if face_space_w > 0.0 {
                face_space_w
            } else {
                face_w
            };
            Some(tab_cols as f32 * space_w)
        }
        _ if ch < ' ' || ch == '\x7F' => Some(face_w),
        _ => {
            // Cluster extenders (combining marks, ZWJ, variation
            // selectors) share the preceding base character's cell
            // and contribute no advance of their own.
            if is_cluster_extender(ch) {
                return Some(0.0);
            }
            let char_cols = if is_wide_char(ch) { 2 } else { 1 };
            Some(char_advance(
                ascii_width_cache,
                font_metrics_svc,
                ch,
                char_cols,
                char_w,
                font_size,
                face_char_w,
                font_family,
                font_weight,
                font_italic,
            ))
        }
    }
}

#[inline]
fn cursor_style_for_window(params: &WindowParams) -> Option<CursorStyle> {
    use neomacs_display_protocol::frame_glyphs::CursorKind;

    if params.cursor_kind == CursorKind::NoCursor {
        return None;
    }

    CursorStyle::from_kind(params.cursor_kind, params.cursor_bar_width)
}

/// Parse `:raise` factor from a display property value.
///
/// Handles two forms:
/// 1. `(raise FACTOR)` — a list whose car is the symbol `raise`
/// 2. A plist containing `:raise FACTOR` (e.g., `(space :raise 0.3 :width 5)`)
///
/// Returns the raise factor as f32, or None if not a raise spec.
fn parse_display_raise_factor(prop_val: &neovm_core::emacs_core::Value) -> Option<f32> {
    // Form 1: (raise FACTOR)
    if prop_val.is_cons() {
        let car = prop_val.cons_car();
        let cdr = prop_val.cons_cdr();
        if car.is_symbol_named("raise") {
            // cdr should be (FACTOR . nil) or FACTOR
            if cdr.is_cons() {
                let cdr_car = cdr.cons_car();
                if let Some(f) = cdr_car.as_number_f64() {
                    return Some(f as f32);
                }
            } else if let Some(f) = cdr.as_number_f64() {
                return Some(f as f32);
            }
        }
    }

    // Form 2: plist with :raise key
    if let Some(items) = neovm_core::emacs_core::value::list_to_vec(prop_val) {
        let mut i = 0;
        while i + 1 < items.len() {
            if items[i].is_symbol_named(":raise") {
                if let Some(f) = items[i + 1].as_number_f64() {
                    return Some(f as f32);
                }
            }
            i += 1;
        }
    }
    None
}

/// Parse `:height` factor from a display property value.
///
/// Handles two forms:
/// 1. `(height FACTOR)` — a list whose car is the symbol `height`
/// 2. A plist containing `:height FACTOR` (e.g., `(space :height 1.5)`)
///
/// Returns the height scale factor as f32, or None if not a height spec.
fn parse_display_height_factor(prop_val: &neovm_core::emacs_core::Value) -> Option<f32> {
    // Form 1: (height FACTOR)
    if prop_val.is_cons() {
        let car = prop_val.cons_car();
        let cdr = prop_val.cons_cdr();
        if car.is_symbol_named("height") {
            // cdr should be (FACTOR . nil) or FACTOR
            if cdr.is_cons() {
                let cdr_car = cdr.cons_car();
                if let Some(f) = cdr_car.as_number_f64() {
                    return Some(f as f32);
                }
            } else if let Some(f) = cdr.as_number_f64() {
                return Some(f as f32);
            }
        }
    }

    // Form 2: plist with :height key
    if let Some(items) = neovm_core::emacs_core::value::list_to_vec(prop_val) {
        let mut i = 0;
        while i + 1 < items.len() {
            if items[i].is_symbol_named(":height") {
                if let Some(f) = items[i + 1].as_number_f64() {
                    return Some(f as f32);
                }
            }
            i += 1;
        }
    }
    None
}

/// Check if a character should be displayed as a glyphless character.
/// Returns: 0=normal, 1=thin_space, 2=empty_box, 3=hex_code, 5=zero_width
fn check_glyphless_char(ch: char) -> u8 {
    let cp = ch as u32;
    // C1 control characters: U+0080 to U+009F — show as hex code
    if cp >= 0x80 && cp <= 0x9F {
        return 3;
    }
    // Byte-order marks and zero-width chars
    if cp == 0xFEFF {
        return 5;
    } // BOM / ZWNBSP
    if cp == 0x200B {
        return 5;
    } // zero-width space
    if cp == 0x200C || cp == 0x200D {
        return 5;
    } // ZWNJ, ZWJ
    if cp == 0x200E || cp == 0x200F {
        return 5;
    } // LRM, RLM
    if cp == 0x2028 {
        return 5;
    } // line separator (in buffer text)
    if cp == 0x2029 {
        return 5;
    } // paragraph separator
    // Unicode specials block: U+FFF0-U+FFF8 (not assigned)
    if cp >= 0xFFF0 && cp <= 0xFFF8 {
        return 3;
    }
    // Object replacement character
    if cp == 0xFFFC {
        return 2;
    } // empty box
    // Language tags block U+E0001-U+E007F: zero-width
    if cp >= 0xE0001 && cp <= 0xE007F {
        return 5;
    }
    // Variation selectors supplement: zero-width
    if cp >= 0xE0100 && cp <= 0xE01EF {
        return 5;
    }
    // Basic variation selectors: zero-width
    if cp >= 0xFE00 && cp <= 0xFE0F {
        return 5;
    }
    0 // normal display
}

/// Render overlay string bytes into the layout.
///
/// On `\n`: ends the current glyph row, advances `row`/`y`, begins a new row,
/// and resets `x`/`col` — matching GNU `display_line()` behaviour for overlay
/// strings that contain newlines (e.g. fido-vertical-mode completions).
fn render_overlay_string(
    evaluator: &mut Context,
    output_emitter: &mut WindowOutputEmitter,
    text_bytes: &[u8],
    x: &mut f32,
    y: &mut f32,
    col: &mut usize,
    row: &mut usize,
    hit_rows: &mut Vec<HitRow>,
    hit_row_charpos_start: &mut i64,
    anchor_charpos: i64,
    row_y_positions: &mut Vec<f32>,
    row_max_height: &mut f32,
    row_max_ascent: &mut f32,
    face_char_w: f32,
    char_h: f32,
    default_row_ascent: f32,
    max_x: f32,
    content_x: f32,
    text_y: f32,
    row_extra_y: f32,
    row_base: usize,
    max_rows: usize,
    overlay_face: Option<&super::neovm_bridge::ResolvedFace>,
    current_face_id: &mut u32,
    builder: &mut crate::matrix_builder::GlyphMatrixBuilder,
) {
    // Overlay face is now handled by the builder; just track the face_id bump.
    let face_id = if overlay_face.is_some() {
        *current_face_id += 1;
        current_face_id.saturating_sub(1)
    } else {
        current_face_id.saturating_sub(1)
    };

    let mut idx = 0;
    while idx < text_bytes.len() {
        if *row >= max_rows {
            break;
        }
        let (ch, ch_len) = decode_utf8(&text_bytes[idx..]);
        idx += ch_len;

        if ch == '\n' {
            // End current row, start a new one — mirrors the main text loop.
            hit_rows.push(HitRow {
                y_start: *y,
                y_end: *y + *row_max_height,
                charpos_start: *hit_row_charpos_start,
                charpos_end: anchor_charpos,
            });
            *hit_row_charpos_start = anchor_charpos;
            finish_text_row(
                builder,
                output_emitter,
                *y,
                *row_max_height,
                *row_max_ascent,
            );
            builder.end_row();
            *row += 1;
            if *row >= max_rows {
                break;
            }
            *y = text_y + *row as f32 * char_h + row_extra_y;
            *row_max_height = char_h;
            *row_max_ascent = default_row_ascent;
            row_y_positions.push(*y);
            builder.begin_row(
                row_base + *row,
                neomacs_display_protocol::frame_glyphs::GlyphRowRole::Text,
            );
            *x = content_x;
            *col = 0;
            output_emitter.begin_text_row(evaluator, *row, *col, *y, *x);
            continue;
        }

        let is_extender = is_cluster_extender(ch);
        let ch_advance = if is_extender {
            0.0
        } else if is_wide_char(ch) {
            2.0 * face_char_w
        } else {
            face_char_w
        };
        if *x + ch_advance > max_x {
            break;
        }

        // Push glyph into the matrix builder (charpos=0 for overlay text).
        // Extenders merge into the preceding Char/Composite inside
        // push_char; emit via the regular entry point either way.
        if is_wide_char(ch) && !is_extender {
            builder.push_wide_char(ch, face_id, 0);
        } else {
            builder.push_char(ch, face_id, 0);
        }

        let glyph_start_x = *x;
        let glyph_start_col = *col;
        *x += ch_advance;
        *col += if is_extender {
            0
        } else if is_wide_char(ch) {
            2
        } else {
            1
        };
        output_emitter.emit_synthetic_text_span(
            evaluator,
            *row,
            *y,
            glyph_start_x,
            *x - glyph_start_x,
            glyph_start_col,
            *col,
        );
    }
}

fn measured_face_status_line_face(
    face_id: u32,
    face: &super::neovm_bridge::ResolvedFace,
    metrics: Option<FontMetrics>,
) -> StatusLineFace {
    let mut render_face = StatusLineFace::from_resolved(face_id, face);
    if let Some(metrics) = metrics {
        render_face.font_char_width = metrics.char_width;
        render_face.font_ascent = metrics.ascent;
        render_face.font_descent = metrics.descent.max(0.0).ceil() as i32;
    }
    render_face
}

fn apply_resolved_face(
    builder: &mut crate::matrix_builder::GlyphMatrixBuilder,
    face_id: u32,
    face: &super::neovm_bridge::ResolvedFace,
    metrics: Option<FontMetrics>,
) {
    let render_face = measured_face_status_line_face(face_id, face, metrics);
    let rendered = render_face.render_face();
    builder.insert_face(render_face.face_id, rendered);
}

/// The main Rust layout engine.
///
/// Called on the Emacs thread during redisplay. Reads buffer/state from
/// neovm-core, resolves faces, computes layout, and publishes immutable
/// display snapshots for the render thread and TTY backend.
pub struct LayoutEngine {
    /// Reusable text buffer to avoid allocation per frame
    text_buf: Vec<u8>,
    /// Per-font ASCII width cache: actual glyph widths via cosmic-text.
    /// Key: semantic font identity, Value: advance widths for chars 0-127.
    ascii_width_cache: std::collections::HashMap<AsciiWidthCacheKey, [f32; 128]>,
    /// Hit-test data being built for current frame
    hit_data: Vec<WindowHitData>,
    /// Authoritative visible glyph geometry published back into core state.
    display_snapshots: Vec<WindowDisplaySnapshot>,
    /// Reusable ligature run buffer
    run_buf: LigatureRunBuffer,
    /// Whether ligatures are enabled
    pub ligatures_enabled: bool,
    /// Resolved font family name for the current face.
    /// When a font_file_path is available and cosmic-text metrics are active,
    /// this holds the fontdb-registered family name. Otherwise it mirrors
    /// the Emacs font_family. Avoids per-character String allocation.
    current_resolved_family: String,
    /// Face ID for which current_resolved_family was computed.
    /// Used to avoid re-resolving on every character.
    resolved_family_face_id: u32,
    /// Cosmic-text font metrics service.
    ///
    /// Populated by `enable_cosmic_metrics()` at GUI startup. Left
    /// `None` for TTY mode, where all measurements go through the
    /// character-cell grid. Replaces the previous
    /// `use_cosmic_metrics: bool` runtime flag — the decision is
    /// now made once at startup by the binary that constructs the
    /// layout engine.
    pub(crate) font_metrics: Option<FontMetricsService>,
    /// Previous frame's per-window metadata for transition hint derivation.
    prev_window_infos: std::collections::HashMap<i64, WindowInfo>,
    /// Previous selected window id for switch-fade detection.
    prev_selected_window_id: i64,
    /// Previous frame background for theme-transition detection.
    prev_background: Option<(f32, f32, f32, f32)>,
    /// Authoritative glyph-matrix builder for the current frame layout pass.
    pub matrix_builder: crate::matrix_builder::GlyphMatrixBuilder,
    /// The last completed `FrameDisplayState`, produced by `layout_frame_rust()`.
    /// Used by the TTY redisplay path to drive `TtyRif` on the evaluator thread.
    pub last_frame_display_state: Option<neomacs_display_protocol::glyph_matrix::FrameDisplayState>,
    /// Monotonic face-id allocator, frame-scoped.
    ///
    /// Mirrors GNU's frame-wide `face_cache->used` counter in
    /// `src/xfaces.c::realize_face`, which grows within a frame and
    /// never resets per window: windows on the same frame share a
    /// single face cache so two windows referencing the same face
    /// end up with the same `face_id`, and two windows referencing
    /// DIFFERENT faces get different ids.
    ///
    /// Before this field existed, `layout_window_rust` used a
    /// function-local `let mut current_face_id: u32 = 1;` which
    /// reset to 1 for every window. That collided with the
    /// frame-wide `matrix_builder.faces` HashMap: the first window
    /// inserted `mode-line` at face_id=2, the second window then
    /// inserted `mode-line-inactive` ALSO at face_id=2 and
    /// overwrote the first entry, causing both mode lines to
    /// render with the inactive face after `C-x 2`.
    pub(crate) frame_face_id_counter: u32,
    /// Frame-level chrome rows built before leaf-window layout.
    ///
    /// GNU treats the tab bar as frame-level redisplay, not as a row owned by
    /// the first leaf window. Neomacs stages those rows here and attaches them
    /// to the finished frame snapshot.
    pending_frame_chrome_rows: Vec<neomacs_display_protocol::glyph_matrix::FrameChromeRow>,
    /// Frame-level tab bar metadata for render-thread hit-testing.
    pending_tab_bar: Option<neomacs_display_protocol::frame_glyphs::FrameTabBarState>,
}

impl LayoutEngine {
    /// Create a new layout engine with cosmic-text font metrics.
    ///
    /// Initializes the `FontMetricsService` eagerly (~500ms font
    /// database scan). Used by GUI mode and tests that need pixel-
    /// accurate font measurement. TTY binaries should use
    /// `new_without_font_metrics()` to skip the scan.
    pub fn new() -> Self {
        Self {
            text_buf: Vec::with_capacity(64 * 1024), // 64KB initial
            ascii_width_cache: std::collections::HashMap::new(),
            hit_data: Vec::new(),
            display_snapshots: Vec::new(),
            run_buf: LigatureRunBuffer::new(),
            ligatures_enabled: false,
            current_resolved_family: String::new(),
            resolved_family_face_id: u32::MAX,
            font_metrics: Some(FontMetricsService::new()),
            prev_window_infos: std::collections::HashMap::new(),
            prev_selected_window_id: 0,
            prev_background: None,
            matrix_builder: crate::matrix_builder::GlyphMatrixBuilder::new(),
            last_frame_display_state: None,
            frame_face_id_counter: 1,
            pending_frame_chrome_rows: Vec::new(),
            pending_tab_bar: None,
        }
    }

    /// Create a layout engine without font metrics (TTY mode).
    ///
    /// Skips the ~500ms cosmic-text font database scan. All
    /// measurements fall back to the character-cell grid (1x1 for
    /// TTY, matching GNU Emacs frame.c:1184-1185). GUI binaries
    /// should use `new()` instead.
    pub fn new_without_font_metrics() -> Self {
        Self {
            text_buf: Vec::with_capacity(64 * 1024),
            ascii_width_cache: std::collections::HashMap::new(),
            hit_data: Vec::new(),
            display_snapshots: Vec::new(),
            run_buf: LigatureRunBuffer::new(),
            ligatures_enabled: false,
            current_resolved_family: String::new(),
            resolved_family_face_id: u32::MAX,
            font_metrics: None,
            prev_window_infos: std::collections::HashMap::new(),
            prev_selected_window_id: 0,
            prev_background: None,
            matrix_builder: crate::matrix_builder::GlyphMatrixBuilder::new(),
            last_frame_display_state: None,
            frame_face_id_counter: 1,
            pending_frame_chrome_rows: Vec::new(),
            pending_tab_bar: None,
        }
    }

    /// Disable cosmic-text font measurement (TTY mode).
    ///
    /// Drops the `FontMetricsService` so all measurements fall back
    /// to the character-cell grid. Called once at TTY startup from
    /// the binary that constructs the layout engine.
    pub fn disable_cosmic_metrics(&mut self) {
        self.font_metrics = None;
    }

    /// Enable cosmic-text font measurement for GUI rendering.
    ///
    /// Constructs the `FontMetricsService` if it hasn't already been
    /// constructed. Called once at GUI startup from the binary that
    /// sets up the layout engine. TTY mode skips this call and
    /// leaves `font_metrics` as `None`, so all measurements fall
    /// back to the character-cell grid (GNU Emacs frame.c:1184-1185:
    /// TTY frames have column_width=1 and line_height=1).
    ///
    /// This replaces the previous `use_cosmic_metrics: bool` runtime
    /// flag. The decision of which measurement strategy to use is
    /// now made once at startup by which binary constructs the
    /// engine, matching GNU's per-frame redisplay_interface vtable
    /// dispatch.
    pub fn enable_cosmic_metrics(&mut self) {
        if self.font_metrics.is_none() {
            self.font_metrics = Some(FontMetricsService::new());
        }
    }

    fn record_transition_hint_from_latest_window_info(
        &mut self,
        curr_window_infos: &mut std::collections::HashMap<i64, WindowInfo>,
    ) {
        if let Some(curr) = self.matrix_builder.window_infos().last().cloned() {
            if let Some(prev) = self.prev_window_infos.get(&curr.window_id) {
                if let Some(hint) = FrameGlyphBuffer::derive_transition_hint(prev, &curr) {
                    self.matrix_builder.push_transition_hint(hint);
                }
            }
            curr_window_infos.insert(curr.window_id, curr);
        }
    }

    fn record_effect_hints_from_latest_window_info(&mut self) {
        let Some(curr) = self.matrix_builder.window_infos().last().cloned() else {
            return;
        };
        if curr.is_minibuffer {
            return;
        }

        let Some(prev) = self.prev_window_infos.get(&curr.window_id) else {
            return;
        };
        if prev.buffer_id == 0 || curr.buffer_id == 0 {
            return;
        }

        if prev.buffer_id != curr.buffer_id {
            let hint = WindowEffectHint::TextFadeIn {
                window_id: curr.window_id,
                bounds: curr.bounds,
            };
            self.matrix_builder.push_effect_hint(hint);
            return;
        }

        if prev.window_start != curr.window_start {
            let direction = if curr.window_start > prev.window_start {
                1
            } else {
                -1
            };
            let delta = (curr.window_start - prev.window_start).unsigned_abs() as f32;
            let h1 = WindowEffectHint::TextFadeIn {
                window_id: curr.window_id,
                bounds: curr.bounds,
            };
            self.matrix_builder.push_effect_hint(h1);
            let h2 = WindowEffectHint::ScrollLineSpacing {
                window_id: curr.window_id,
                bounds: curr.bounds,
                direction,
            };
            self.matrix_builder.push_effect_hint(h2);
            let h3 = WindowEffectHint::ScrollMomentum {
                window_id: curr.window_id,
                bounds: curr.bounds,
                direction,
            };
            self.matrix_builder.push_effect_hint(h3);
            let h4 = WindowEffectHint::ScrollVelocityFade {
                window_id: curr.window_id,
                bounds: curr.bounds,
                delta,
            };
            self.matrix_builder.push_effect_hint(h4);
        }
    }

    fn find_window_cursor_y_in_builder(
        builder: &crate::matrix_builder::GlyphMatrixBuilder,
        info: &WindowInfo,
    ) -> Option<f32> {
        for cursor in builder.cursors() {
            if cursor.x >= info.bounds.x
                && cursor.x < info.bounds.x + info.bounds.width
                && cursor.y >= info.bounds.y
                && cursor.y < info.bounds.y + info.bounds.height
                && !cursor.style.is_hollow()
            {
                return Some(cursor.y);
            }
        }
        None
    }

    fn add_line_animation_hints(
        &mut self,
        curr_window_infos: &std::collections::HashMap<i64, WindowInfo>,
    ) {
        for (window_id, curr) in curr_window_infos {
            if curr.is_minibuffer {
                continue;
            }
            let Some(prev) = self.prev_window_infos.get(window_id) else {
                continue;
            };
            if prev.buffer_id == 0 || curr.buffer_id == 0 {
                continue;
            }
            if prev.buffer_id == curr.buffer_id
                && prev.window_start == curr.window_start
                && prev.buffer_size != curr.buffer_size
            {
                if let Some(edit_y) =
                    Self::find_window_cursor_y_in_builder(&self.matrix_builder, curr)
                {
                    let offset = if curr.buffer_size > prev.buffer_size {
                        -curr.char_height
                    } else {
                        curr.char_height
                    };
                    let hint = WindowEffectHint::LineAnimation {
                        window_id: curr.window_id,
                        bounds: curr.bounds,
                        edit_y: edit_y + curr.char_height,
                        offset,
                    };
                    self.matrix_builder.push_effect_hint(hint);
                }
            }
        }
    }

    fn update_window_switch_hint(&mut self) {
        let new_selected = self
            .matrix_builder
            .window_infos()
            .iter()
            .find(|info| info.selected && !info.is_minibuffer)
            .map(|info| (info.window_id, info.bounds));
        if let Some((window_id, bounds)) = new_selected {
            if self.prev_selected_window_id != 0 && self.prev_selected_window_id != window_id {
                let hint = WindowEffectHint::WindowSwitchFade { window_id, bounds };
                self.matrix_builder.push_effect_hint(hint);
            }
            self.prev_selected_window_id = window_id;
        }
    }

    fn update_theme_transition_hint(&mut self, frame_width: f32, frame_height: f32) {
        let bg = self.matrix_builder.background_color();
        let new_bg = (bg.r, bg.g, bg.b, bg.a);
        if let Some(old_bg) = self.prev_background {
            let dr = (new_bg.0 - old_bg.0).abs();
            let dg = (new_bg.1 - old_bg.1).abs();
            let db = (new_bg.2 - old_bg.2).abs();
            if dr > 0.02 || dg > 0.02 || db > 0.02 {
                let full_h = self
                    .matrix_builder
                    .window_infos()
                    .iter()
                    .find(|w| w.is_minibuffer)
                    .map_or(frame_height, |w| w.bounds.y);
                let hint = WindowEffectHint::ThemeTransition {
                    bounds: Rect::new(0.0, 0.0, frame_width, full_h),
                };
                self.matrix_builder.push_effect_hint(hint);
            }
        }
        self.prev_background = Some(new_bg);
    }

    fn maybe_add_topology_transition_hint(
        &mut self,
        frame_width: f32,
        frame_height: f32,
        curr_window_infos: &std::collections::HashMap<i64, WindowInfo>,
    ) {
        if self.prev_window_infos.is_empty() {
            return;
        }

        let prev_non_mini: std::collections::HashSet<i64> = self
            .prev_window_infos
            .iter()
            .filter(|(_, info)| !info.is_minibuffer)
            .map(|(window_id, _)| *window_id)
            .collect();
        let curr_non_mini: std::collections::HashSet<i64> = curr_window_infos
            .iter()
            .filter(|(_, info)| !info.is_minibuffer)
            .map(|(window_id, _)| *window_id)
            .collect();

        if prev_non_mini.is_empty() || curr_non_mini.is_empty() || prev_non_mini == curr_non_mini {
            return;
        }

        if self
            .matrix_builder
            .transition_hints()
            .iter()
            .any(|hint| hint.window_id == 0 && matches!(hint.kind, WindowTransitionKind::Crossfade))
        {
            return;
        }

        let full_h = self
            .matrix_builder
            .window_infos()
            .iter()
            .find(|w| w.is_minibuffer)
            .map_or(frame_height, |w| w.bounds.y);

        let hint = WindowTransitionHint {
            window_id: 0,
            bounds: Rect::new(0.0, 0.0, frame_width, full_h),
            kind: WindowTransitionKind::Crossfade,
            effect: None,
            easing: None,
        };
        self.matrix_builder.push_transition_hint(hint);
    }

    // char_advance is a standalone function (below) to avoid borrow conflicts
    // with self.text_buf

    /// Perform layout for a frame using neovm-core data (Rust-authoritative path).
    ///
    /// This is the Rust-native alternative to `layout_frame()` which reads from
    /// C struct pointers. It reads buffer text, window geometry, and buffer-local
    /// variables directly from the Context's state.
    pub fn layout_frame_rust(
        &mut self,
        evaluator: &mut neovm_core::emacs_core::Context,
        frame_id: neovm_core::window::FrameId,
    ) {
        // FontMetricsService is set up once at startup via
        // `enable_cosmic_metrics()` (GUI mode) or left as `None`
        // (TTY mode). No per-frame flag check; the backend choice
        // is frame-invariant.

        let (bootstrap_bg, bootstrap_font_size) = {
            let Some(frame) = evaluator.frame_manager().get(frame_id) else {
                tracing::error!("layout_frame_rust: frame {:?} not found", frame_id);
                return;
            };
            let bootstrap =
                super::neovm_bridge::frame_params_from_neovm(frame, evaluator.face_table());
            (bootstrap.background, frame.font_pixel_size)
        };

        // Realize the default face before collecting window params so frame and
        // window geometry use the same default metrics GNU Emacs redisplay does.
        let face_resolver = super::neovm_bridge::FaceResolver::new(
            evaluator.face_table(),
            0x00FFFFFF,
            bootstrap_bg,
            bootstrap_font_size,
        );
        let default_resolved = face_resolver.default_face();
        let default_metrics = self.font_metrics.as_mut().map(|svc| {
            svc.font_metrics(
                &default_resolved.font_family,
                default_resolved.font_weight,
                default_resolved.italic,
                default_resolved.font_size,
            )
        });

        if let Some(metrics) = default_metrics {
            if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
                frame.char_width = metrics.char_width.max(1.0);
                frame.char_height = metrics.line_height.max(1.0);
                frame.font_pixel_size = default_resolved.font_size;
            }
        } else {
            // GNU Emacs TTY frames use 1x1 character cell metrics
            // (frame.c:1184-1185: column_width=1, line_height=1).
            // Ensure char_height is never zero to prevent cosmic-text
            // assertion "line height cannot be 0".
            if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
                if frame.char_height < 1.0 {
                    frame.char_height = 1.0;
                }
                if frame.char_width < 1.0 {
                    frame.char_width = 1.0;
                }
            }
        }

        // --- Minibuffer auto-resize retry loop (GNU xdisp.c:13161-13301) ---
        //
        // After laying out all windows we check whether the minibuffer
        // used more (or fewer) display rows than its allocated height.
        // If so we call grow_mini_window / shrink_mini_window and
        // re-layout the entire frame.  The `mini_resize_attempted` flag
        // limits this to a single retry to prevent infinite loops.
        let mut mini_resize_attempted = false;

        let (frame_params, curr_window_infos) = loop {
            // Collect window and frame params from neovm-core
            let (frame_params, window_params_list) =
                match super::neovm_bridge::collect_layout_params(
                    evaluator,
                    frame_id,
                    default_metrics.map(|metrics| metrics.ascent),
                ) {
                    Some(data) => data,
                    None => {
                        tracing::error!("layout_frame_rust: frame {:?} not found", frame_id);
                        return;
                    }
                };

            // --- Fontification pass ---
            // Run fontification for each window's visible region BEFORE the
            // read-only layout pass.  This triggers jit-lock / font-lock to set
            // font-lock-face text properties that the FaceResolver later reads.
            evaluator.setup_thread_locals();
            for params in &window_params_list {
                let buf_id = neovm_core::buffer::BufferId(params.buffer_id);
                let window_start = params.window_start.max(params.buffer_begv);
                let text_height = params.bounds.height - params.mode_line_height;
                let max_rows = if params.char_height > 0.0 {
                    (text_height / params.char_height).ceil() as i64
                } else {
                    50 // fallback
                };
                // Estimate the end of the visible region (generous: 200 chars/line).
                let fontify_end = (window_start + max_rows * 200).min(params.buffer_size);
                Self::ensure_fontified_rust(evaluator, buf_id, window_start, fontify_end);
            }

            // Reset builder for new frame
            self.matrix_builder.reset();
            self.frame_face_id_counter = 1;
            self.pending_frame_chrome_rows.clear();
            self.pending_tab_bar = None;
            let mut curr_window_infos: std::collections::HashMap<i64, WindowInfo> =
                std::collections::HashMap::new();

            // Set up frame dimensions in the builder
            self.matrix_builder
                .set_background_color(Color::from_pixel(frame_params.background));
            self.matrix_builder
                .set_font_pixel_size(frame_params.font_pixel_size);

            // Clear hit-test data for new frame
            self.hit_data.clear();
            self.display_snapshots.clear();
            let default_resolved = face_resolver.default_face();

            apply_resolved_face(
                &mut self.matrix_builder,
                0,
                default_resolved,
                default_metrics,
            );

            let tab_bar_height = frame_params.tab_bar_height;
            if tab_bar_height > 0.0 {
                self.render_frame_tab_bar_rust(
                    evaluator,
                    frame_id.0 as i64,
                    &face_resolver,
                    &frame_params,
                    tab_bar_height,
                );
            }

            tracing::debug!(
                "layout_frame_rust: {}x{} char={}x{} windows={}",
                frame_params.width,
                frame_params.height,
                frame_params.char_width,
                frame_params.char_height,
                window_params_list.len()
            );

            if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
                frame.begin_display_output_pass();
            }

            for params in &window_params_list {
                tracing::debug!(
                    "layout window: id={} buf={} bounds=({:.0},{:.0},{:.0},{:.0}) mini={} selected={} mode_line_h={:.0}",
                    params.window_id,
                    params.buffer_id,
                    params.bounds.x,
                    params.bounds.y,
                    params.bounds.width,
                    params.bounds.height,
                    params.is_minibuffer,
                    params.selected,
                    params.mode_line_height,
                );
                // Add window background
                self.matrix_builder
                    .push_background(params.bounds, Color::from_pixel(params.default_bg));

                // Add window info for animation detection
                let buffer_file_name = {
                    let buf_id = neovm_core::buffer::BufferId(params.buffer_id);
                    evaluator
                        .buffer_manager()
                        .get(buf_id)
                        .and_then(|b| b.file_name_runtime_string_owned())
                        .unwrap_or_default()
                };
                let modified = {
                    let buf_id = neovm_core::buffer::BufferId(params.buffer_id);
                    evaluator
                        .buffer_manager()
                        .get(buf_id)
                        .map(|b| b.is_modified())
                        .unwrap_or(false)
                };
                let window_info = neomacs_display_protocol::frame_glyphs::WindowInfo {
                    window_id: params.window_id,
                    buffer_id: params.buffer_id,
                    window_start: params.window_start,
                    window_end: 0, // filled after layout
                    buffer_size: params.buffer_size,
                    bounds: Rect::new(
                        params.bounds.x,
                        params.bounds.y,
                        params.bounds.width,
                        params.bounds.height,
                    ),
                    mode_line_height: params.mode_line_height,
                    header_line_height: params.header_line_height,
                    tab_line_height: params.tab_line_height,
                    selected: params.selected,
                    is_minibuffer: params.is_minibuffer,
                    char_height: params.char_height,
                    buffer_file_name,
                    modified,
                };
                self.matrix_builder.push_window_info(window_info);
                self.record_transition_hint_from_latest_window_info(&mut curr_window_infos);
                self.record_effect_hints_from_latest_window_info();

                let right_edge = params.bounds.x + params.bounds.width;
                let bottom_edge = params.bounds.y + params.bounds.height;
                let is_rightmost = right_edge >= frame_params.width - 1.0;
                let is_bottommost = bottom_edge >= frame_params.height - 1.0;
                let reserve_right_border_col =
                    frame_params.right_divider_width == 0 && !is_rightmost;

                // Simplified layout for this window (no face resolution, no overlays)
                self.layout_window_rust(
                    evaluator,
                    frame_id,
                    params,
                    &frame_params,
                    &face_resolver,
                    reserve_right_border_col,
                    MAX_WINDOW_VISIBILITY_RETRIES,
                );

                // Draw window dividers
                if frame_params.right_divider_width > 0 && !is_rightmost {
                    let dw = frame_params.right_divider_width as f32;
                    let _x0 = right_edge - dw;
                    let _y0 = params.bounds.y;
                    let _h = params.bounds.height
                        - if frame_params.bottom_divider_width > 0 && !is_bottommost {
                            frame_params.bottom_divider_width as f32
                        } else {
                            0.0
                        };
                    let _mid_fg = Color::from_pixel(frame_params.divider_fg);
                } else if !is_rightmost {
                    // TTY / GUI-without-divider vertical border.
                    //
                    // Mirrors GNU `src/dispnew.c:2568-2697`
                    // (`build_frame_matrix_from_leaf_window`) which,
                    // for every non-rightmost window, overwrites the
                    // LAST glyph of each enabled row with a `|`
                    // character in the `vertical-border` face before
                    // the frame matrix is written to the terminal:
                    //
                    //   if (!WINDOW_RIGHTMOST_P (w))
                    //     SET_GLYPH_FROM_CHAR (right_border_glyph, '|');
                    //   ...
                    //   if (GLYPH_FACE (right_border_glyph) <= 0)
                    //     SET_GLYPH_FACE (right_border_glyph,
                    //                     VERTICAL_BORDER_FACE_ID);
                    //
                    // Without this patch two horizontally-split
                    // windows in `neomacs -nw` rendered with no
                    // visible divider between them; the user could
                    // not tell where one window ended and the next
                    // began. The `vertical-border` face on TTY
                    // inherits from `mode-line-inactive` per
                    // `lisp/faces.el::vertical-border`.
                    let border_face = face_resolver.resolve_named_face("vertical-border");
                    let border_face_id = self.frame_face_id_counter;
                    self.frame_face_id_counter += 1;
                    let realized_face = crate::display_status_line::StatusLineFace::from_resolved(
                        border_face_id,
                        &border_face,
                    );
                    self.matrix_builder
                        .insert_face(border_face_id, realized_face.render_face());
                    self.matrix_builder
                        .overwrite_last_window_right_border('|', border_face_id);
                }

                if frame_params.bottom_divider_width > 0 && !is_bottommost {
                    let dw = frame_params.bottom_divider_width as f32;
                    let _x0 = params.bounds.x;
                    let _y0 = bottom_edge - dw;
                    let _w = params.bounds.width;
                    let _mid_fg = Color::from_pixel(frame_params.divider_fg);
                }
            }

            // --- Minibuffer auto-resize check (GNU xdisp.c:13161-13301) ---
            //
            // After laying out all windows, check if the minibuffer used
            // more display rows than its allocated height. If so, grow
            // the minibuffer and re-layout the entire frame (one retry).
            // Also shrink back when the minibuffer content fits in fewer
            // rows than currently allocated.
            if !mini_resize_attempted {
                if let Some(mini_entry) = self.matrix_builder.windows().last() {
                    if let Some(mini_params) = window_params_list.last() {
                        if mini_params.is_minibuffer {
                            let mini_rows_used =
                                mini_entry.matrix.rows.iter().filter(|r| r.enabled).count();
                            let char_h = frame_params.char_height.max(1.0);
                            let allocated_rows =
                                (mini_params.bounds.height / char_h).floor().max(1.0) as usize;
                            let frame_rows = frame_params.height / char_h;
                            let max_mini_lines = max_mini_window_lines(evaluator, frame_rows);

                            if mini_rows_used > allocated_rows {
                                // --- Grow ---
                                let delta = (mini_rows_used as i32) - (allocated_rows as i32);

                                // Check resize-mini-windows variable
                                let resize_policy = evaluator
                                    .obarray()
                                    .symbol_value("resize-mini-windows")
                                    .copied();
                                let should_resize = match resize_policy {
                                    Some(v) if v.is_nil() => false,
                                    _ => true, // grow-only or t
                                };

                                if should_resize {
                                    tracing::debug!(
                                        "minibuffer auto-resize: grow by {} rows \
                                         (used={}, allocated={})",
                                        delta,
                                        mini_rows_used,
                                        allocated_rows,
                                    );
                                    if let Some(frame) =
                                        evaluator.frame_manager_mut().get_mut(frame_id)
                                    {
                                        frame
                                            .grow_mini_window_with_max_lines(delta, max_mini_lines);
                                    }
                                    mini_resize_attempted = true;
                                    continue; // restart the layout loop
                                }
                            } else if mini_rows_used < allocated_rows && allocated_rows > 1 {
                                // --- Shrink ---
                                let resize_policy = evaluator
                                    .obarray()
                                    .symbol_value("resize-mini-windows")
                                    .copied();
                                let should_shrink = match resize_policy {
                                    Some(v) if v.is_symbol_named("grow-only") => {
                                        // GNU xdisp.c:13283: with grow-only,
                                        // shrink when BEGV == ZV (buffer
                                        // visible region empty). Approximate
                                        // with mini_rows_used <= 1: if the
                                        // content fits in 1 row, shrink.
                                        mini_rows_used <= 1
                                    }
                                    Some(v) if v.is_nil() => false,
                                    _ => true,
                                };

                                if should_shrink {
                                    tracing::debug!(
                                        "minibuffer auto-resize: shrink \
                                         (used={}, allocated={})",
                                        mini_rows_used,
                                        allocated_rows,
                                    );
                                    if let Some(frame) =
                                        evaluator.frame_manager_mut().get_mut(frame_id)
                                    {
                                        frame.shrink_mini_window();
                                    }
                                    mini_resize_attempted = true;
                                    continue; // restart the layout loop
                                }
                            }
                        }
                    }
                }
            }

            self.add_line_animation_hints(&curr_window_infos);
            self.update_window_switch_hint();
            self.update_theme_transition_hint(frame_params.width, frame_params.height);
            self.maybe_add_topology_transition_hint(
                frame_params.width,
                frame_params.height,
                &curr_window_infos,
            );

            break (frame_params, curr_window_infos);
        };

        // Build parallel GlyphMatrix output for validation
        let frame_cols = (frame_params.width / frame_params.char_width.max(1.0)) as usize;
        let frame_rows = (frame_params.height / frame_params.char_height.max(1.0)) as usize;
        let matrix_builder = std::mem::replace(
            &mut self.matrix_builder,
            crate::matrix_builder::GlyphMatrixBuilder::new(),
        );
        let mut frame_display_state = matrix_builder.finish(
            frame_cols,
            frame_rows,
            frame_params.char_width,
            frame_params.char_height,
        );
        frame_display_state
            .frame_chrome_rows
            .extend(std::mem::take(&mut self.pending_frame_chrome_rows));
        frame_display_state.tab_bar = self.pending_tab_bar.take();

        // NOTE: GlyphMatrix vs FrameGlyphBuffer character count validation removed.
        // FrameGlyphBuffer no longer receives glyph output; the GlyphMatrixBuilder
        // is now the sole output path.

        // Populate the frame-level TTY menu bar.  Mirrors GNU
        // `xdisp.c:prepare_menu_bars` -> `update_menu_bar` -> walking
        // the active maps' `[menu-bar]` prefix and stashing the result
        // in `f->menu_bar_items`.  We do the same walk via
        // `tty_menu_bar::collect_tty_menu_bar_items` and stash the
        // resulting items on the FrameDisplayState so the TTY rasterizer
        // (`tty_rif.rs`) can paint them at row 0.
        //
        // The GUI render runtime has its own menu-bar pipeline (see
        // `neomacs-display-runtime::render_thread`) and ignores this
        // field; we still populate it unconditionally because the
        // collection cost is small and any future TTY-via-display-state
        // path benefits.
        let menu_bar_lines_px = frame_params.menu_bar_height;
        let char_h = frame_params.char_height.max(1.0);
        let menu_bar_lines = (menu_bar_lines_px / char_h).round() as u16;
        if menu_bar_lines > 0 {
            let items = crate::tty_menu_bar::collect_tty_menu_bar_items(evaluator);
            // Resolve the GNU `menu` face once and pass its attributes
            // through to the TTY rasterizer.  Mirrors how
            // `display_menu_bar` (`xdisp.c:27444`) initialises its
            // iterator with `MENU_FACE_ID`: the per-cell face is the
            // `menu` face for every glyph in the menu-bar row.
            //
            // We resolve through `FaceResolver::resolve_named_face`
            // (the same path mode-line / header-line use), so any user
            // customisation of the `menu` face via `face-spec-set` is
            // honoured. The default `menu` face inherits :inverse-video
            // on TTYs, which gives the highlighted bar visible in GNU
            // Emacs `-nw`.
            let menu_face_resolver = crate::neovm_bridge::FaceResolver::new(
                evaluator.face_table(),
                0x00FFFFFF,
                0x00000000,
                frame_params.font_pixel_size,
            );
            let menu_face = menu_face_resolver.resolve_named_face("menu");
            frame_display_state.menu_bar =
                Some(neomacs_display_protocol::glyph_matrix::TtyMenuBarState {
                    items,
                    lines: menu_bar_lines,
                    fg: menu_face.fg,
                    bg: menu_face.bg,
                    bold: menu_face.font_weight >= 600,
                });
        }
        if frame_display_state.parent_id == 0 {
            let menu_face_resolver = crate::neovm_bridge::FaceResolver::new(
                evaluator.face_table(),
                0x00FFFFFF,
                0x00000000,
                frame_params.font_pixel_size,
            );
            let pixel_to_tuple = |pixel: u32| -> (f32, f32, f32) {
                (
                    ((pixel >> 16) & 0xFF) as f32 / 255.0,
                    ((pixel >> 8) & 0xFF) as f32 / 255.0,
                    (pixel & 0xFF) as f32 / 255.0,
                )
            };

            if frame_params.menu_bar_height > 0.0 {
                let menu_face = menu_face_resolver.resolve_named_face("menu");
                frame_display_state.gui_menu_bar =
                    Some(neomacs_display_protocol::glyph_matrix::GuiMenuBarState {
                        items: collect_gui_menu_bar_items(evaluator),
                        height: frame_params.menu_bar_height,
                        fg: pixel_to_tuple(menu_face.fg),
                        bg: pixel_to_tuple(menu_face.bg),
                    });
            }

            if frame_params.tool_bar_height > 0.0 {
                let tool_bar_face = menu_face_resolver.resolve_named_face("tool-bar");
                frame_display_state.gui_tool_bar =
                    Some(neomacs_display_protocol::glyph_matrix::GuiToolBarState {
                        items: collect_gui_tool_bar_items(evaluator),
                        height: frame_params.tool_bar_height,
                        fg: pixel_to_tuple(tool_bar_face.fg),
                        bg: pixel_to_tuple(tool_bar_face.bg),
                    });
            }
        }

        self.last_frame_display_state = Some(frame_display_state);
        self.prev_window_infos = curr_window_infos;

        let snapshots = std::mem::take(&mut self.display_snapshots);
        if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
            frame.set_display_snapshots(snapshots);
        }
        unsafe {
            *std::ptr::addr_of_mut!(FRAME_HIT_DATA) = Some(std::mem::take(&mut self.hit_data));
        }
    }

    /// Simplified window layout using neovm-core data.
    ///
    /// Renders buffer text as a monospace grid with face resolution.
    /// Queries FontMetricsService for per-face character metrics when available.
    /// Note: fontification (jit-lock / font-lock) is triggered by
    /// `layout_frame_rust()` before this function is called, so text
    /// properties are already up-to-date when we read them here.
    fn layout_window_rust(
        &mut self,
        evaluator: &mut neovm_core::emacs_core::Context,
        frame_id: neovm_core::window::FrameId,
        params: &WindowParams,
        _frame_params: &FrameParams,
        face_resolver: &super::neovm_bridge::FaceResolver,
        reserve_right_border_col: bool,
        remaining_visibility_retries: usize,
    ) {
        let buf_id = neovm_core::buffer::BufferId(params.buffer_id);
        let window_id = neovm_core::window::WindowId(params.window_id as u64);
        let layout_buffer = match evaluator.buffer_manager().get(buf_id) {
            Some(buffer) => super::neovm_bridge::LayoutBufferSnapshot::from_buffer(buffer),
            None => {
                tracing::debug!("layout_window_rust: buffer {} not found", params.buffer_id);
                return;
            }
        };
        let buffer = &layout_buffer;

        // Capture buffer name as owned String for use in mode-line fallback.
        // This avoids holding a borrow on `evaluator` through eval calls.
        let buffer_name = buffer.name.clone();
        let buffer_z_char = buffer.zv_char.saturating_add(1);
        let buffer_z_byte = buffer.zv;

        let buf_access = super::neovm_bridge::RustBufferAccess::new(buffer);

        let char_w = params.char_width;
        let char_h = params.char_height;
        let font_ascent = params.font_ascent;
        let echo_message = if params.is_minibuffer {
            evaluator
                .current_message_text()
                .filter(|message| !message.is_empty())
                .map(|message| message.to_string())
        } else {
            None
        };

        // Line number configuration from buffer-local variables
        let lnum_mode = match super::neovm_bridge::buffer_display_line_numbers_mode(buffer) {
            super::neovm_bridge::DisplayLineNumbersMode::Off => 0,
            super::neovm_bridge::DisplayLineNumbersMode::Absolute => 1,
            super::neovm_bridge::DisplayLineNumbersMode::Relative => 2,
            super::neovm_bridge::DisplayLineNumbersMode::Visual => 3,
        };
        let lnum_enabled = lnum_mode > 0;
        let lnum_offset =
            super::neovm_bridge::buffer_local_int(buffer, "display-line-numbers-offset", 0);
        let lnum_major_tick =
            super::neovm_bridge::buffer_local_int(buffer, "display-line-numbers-major-tick", 0)
                as i32;
        let _lnum_minor_tick =
            super::neovm_bridge::buffer_local_int(buffer, "display-line-numbers-minor-tick", 0)
                as i32;
        let lnum_current_absolute =
            super::neovm_bridge::buffer_local_bool(buffer, "display-line-numbers-current-absolute");
        let lnum_widen =
            super::neovm_bridge::buffer_local_bool(buffer, "display-line-numbers-widen");
        let lnum_min_width =
            super::neovm_bridge::buffer_local_int(buffer, "display-line-numbers-width", 0) as i32;

        // Selective display: integer N = hide lines with > N indent + CR hides rest of line;
        // t (True) = only CR hides rest of line (mapped to i32::MAX so indent check never triggers)
        let selective_display = super::neovm_bridge::buffer_selective_display(buffer);

        // Line/wrap prefix: read from buffer-local variables
        let line_prefix_str = super::neovm_bridge::buffer_local_string_owned(buffer, "line-prefix");
        let wrap_prefix_str = super::neovm_bridge::buffer_local_string_owned(buffer, "wrap-prefix");
        let has_prefix = line_prefix_str.is_some() || wrap_prefix_str.is_some();

        // Use face_resolver's default face for this window.
        // Chrome row reservation must use the same realized face metrics as
        // the final status-line renderer, otherwise rows drift from GNU
        // redisplay when faces override font size, ascent, or box widths.
        let default_resolved = face_resolver.default_face();
        let default_fg = Color::from_pixel(default_resolved.fg);
        let default_bg = Color::from_pixel(default_resolved.bg);

        let (default_face_char_w, default_face_h, default_face_ascent) =
            if let Some(ref mut svc) = self.font_metrics {
                let m = svc.font_metrics(
                    &default_resolved.font_family,
                    default_resolved.font_weight,
                    default_resolved.italic,
                    default_resolved.font_size,
                );
                (m.char_width, m.line_height, m.ascent)
            } else {
                (char_w, char_h, font_ascent)
            };

        tracing::debug!(
            "layout font metrics: family={:?} weight={} italic={} size={} char_w={:.2} char_h={:.2} ascent={:.2} (window char_w={:.2} char_h={:.2})",
            default_resolved.font_family,
            default_resolved.font_weight,
            default_resolved.italic,
            default_resolved.font_size,
            default_face_char_w,
            default_face_h,
            default_face_ascent,
            char_w,
            char_h,
        );

        let mode_line_face = if params.mode_line_height > 0.0 {
            Some(face_resolver.resolve_named_face(if params.selected {
                "mode-line"
            } else {
                "mode-line-inactive"
            }))
        } else {
            None
        };
        let header_line_face = if params.header_line_height > 0.0 {
            Some(face_resolver.resolve_named_face(if params.selected {
                "header-line-active"
            } else {
                "header-line-inactive"
            }))
        } else {
            None
        };
        let tab_line_face = if params.tab_line_height > 0.0 {
            Some(face_resolver.resolve_named_face("tab-line"))
        } else {
            None
        };

        let mode_line_height = mode_line_face.as_ref().map_or(0.0, |face| {
            self.status_line_row_height_for_face(face, char_w, default_face_ascent, default_face_h)
        });
        let header_line_height = header_line_face.as_ref().map_or(0.0, |face| {
            self.status_line_row_height_for_face(face, char_w, default_face_ascent, default_face_h)
        });
        let tab_line_height = tab_line_face.as_ref().map_or(0.0, |face| {
            self.status_line_row_height_for_face(face, char_w, default_face_ascent, default_face_h)
        });
        let top_chrome_rows =
            usize::from(tab_line_height > 0.0) + usize::from(header_line_height > 0.0);

        let text_x = params.text_bounds.x;
        let text_y = params.text_bounds.y + header_line_height + tab_line_height;
        let text_width = params.text_bounds.width;
        let text_height =
            params.bounds.height - mode_line_height - header_line_height - tab_line_height;

        // In Emacs, w->vscroll is negative when content is shifted up.
        let vscroll = (-params.vscroll).max(0) as f32;
        let text_height = (text_height - vscroll).max(0.0);

        // Compute line number column width
        let lnum_cols = if lnum_enabled {
            let total_lines = buf_access.count_lines(0, buf_access.zv()) + 1;
            let digit_count = format!("{}", total_lines).len() as i32;
            let min = lnum_min_width.max(1);
            digit_count.max(min) + 1 // +1 for trailing space separator
        } else {
            0
        };
        let lnum_pixel_width = lnum_cols as f32 * char_w;

        let max_rows = (text_height / char_h).floor() as usize;
        // The minibuffer must always render at least 1 row.  Its pixel
        // height may be fractionally smaller than char_h (e.g. 24px vs
        // 24.15 with line-spacing) causing floor() to yield 0.
        // Exception: when vscroll is active, don't force 1 row -- vscroll
        // is used (e.g. by vertico-posframe) to intentionally hide content.
        let max_rows =
            if params.is_minibuffer && max_rows == 0 && text_height > 0.0 && vscroll == 0.0 {
                1
            } else {
                max_rows
            };
        // GNU `resize_mini_window` (`xdisp.c:13161-13301`) pre-
        // grows the minibuffer BEFORE layout by running
        // `move_it_to` to walk ALL content (buffer text + overlay
        // strings) and measuring the resulting pixel height.
        //
        // neomacs approximation: count `\n` in the buffer text
        // plus all overlay `after-string` properties to estimate
        // the display line count. Pre-expand max_rows to that
        // count (clamped to max-mini-window-height = 25% of
        // frame). This avoids the boot-time "tall echo area" bug
        // (single-line content stays at 1 row) while allowing
        // fido-vertical-mode's multi-line overlay to render.
        let max_rows = if params.is_minibuffer {
            let buf_id = neovm_core::buffer::BufferId(params.buffer_id);
            let content_lines = evaluator
                .buffer_manager()
                .get(buf_id)
                .map(|b| {
                    // Count newlines in accessible buffer text.
                    let text_lines = b
                        .buffer_substring_bytes(b.point_min(), b.point_max())
                        .into_iter()
                        .filter(|&byte| byte == b'\n')
                        .count();
                    // Count newlines in overlay after-strings.
                    // Scan all overlays in the buffer's Emacs-byte range.
                    let overlay_lines: usize = b
                        .overlays
                        .overlays_in(0, b.total_bytes())
                        .iter()
                        .filter_map(|ov| {
                            b.overlays
                                .overlay_get_named(*ov, Value::symbol("after-string"))
                                .and_then(|v| v.as_lisp_string())
                                .map(|s| s.as_bytes().iter().filter(|&&byte| byte == b'\n').count())
                        })
                        .sum();
                    // Total lines = text lines + overlay lines + 1
                    // (the first line doesn't need a preceding \n)
                    text_lines + overlay_lines + 1
                })
                .unwrap_or(1);
            let frame_rows = _frame_params.height / char_h;
            let max_mini = max_mini_window_lines(evaluator, frame_rows).ceil() as usize;
            content_lines.clamp(1, max_mini)
        } else {
            max_rows
        };
        let text_matrix_row_base = top_chrome_rows;
        let text_matrix_rows = max_rows.max(1);
        let bottom_chrome_rows = usize::from(mode_line_height > 0.0);
        let mode_line_matrix_row = text_matrix_row_base + text_matrix_rows;
        let cols = ((text_width - lnum_pixel_width) / char_w).floor() as usize;
        let content_x = text_x + lnum_pixel_width;
        let window_text_row = |row: usize| text_matrix_row_base as i64 + row as i64;
        let window_text_row_u32 = |row: usize| (text_matrix_row_base + row) as u32;

        // Read buffer text starting from window_start.
        // Auto-adjust window_start when point is above the visible region.
        let window_start = {
            let mut ws = params.window_start.max(params.buffer_begv);
            // GNU Emacs xdisp.c: if window-start is beyond the buffer content
            // that can fill the window, scroll back to show meaningful content.
            // This happens after buffer deletions that shrink the buffer below
            // the previous window-start.
            if ws > params.buffer_begv {
                let remaining_chars = params.buffer_size - ws;
                if remaining_chars < max_rows as i64 && params.buffer_size > max_rows as i64 {
                    // Not enough content after ws to fill the window.
                    // Recenter around point.
                    let target_rows_above = (max_rows / 2).max(1) as i64;
                    let mut lines_back: i64 = 0;
                    let mut scan_pos = params.point.max(params.buffer_begv);
                    while scan_pos > params.buffer_begv && lines_back < target_rows_above {
                        scan_pos -= 1;
                        let bp = buf_access.charpos_to_bytepos(scan_pos);
                        if buf_access.byte_at(bp) == Some(b'\n') {
                            lines_back += 1;
                        }
                    }
                    ws = scan_pos.max(params.buffer_begv);
                }
            }
            if params.point >= params.buffer_begv && params.point < ws {
                // Point is above the visible region: scroll backward.
                // Target: show point about 25% of the way down from the top.
                let target_rows_above = (max_rows / 4).max(1) as i64;
                let mut lines_back: i64 = 0;
                let mut scan_pos = params.point;
                // Scan backward through buffer text counting newlines
                while scan_pos > params.buffer_begv && lines_back < target_rows_above {
                    scan_pos -= 1;
                    let bp = buf_access.charpos_to_bytepos(scan_pos);
                    if buf_access.byte_at(bp) == Some(b'\n') {
                        lines_back += 1;
                    }
                }
                ws = scan_pos.max(params.buffer_begv);
                tracing::debug!(
                    "layout_window_rust: adjusted window_start {} -> {} (point={})",
                    params.window_start,
                    ws,
                    params.point
                );
            } else if params.point > 0 && !params.is_minibuffer && {
                // Forward-scroll trigger: either
                //   (a) we have a previous window_end and
                //       point is past it (standard
                //       scroll-below-previous case), or
                //   (b) we have no previous window_end (first
                //       layout after construction) and point
                //       is far enough past window_start that
                //       a first-pass layout starting from ws
                //       could not plausibly reach it.
                //
                // Case (b) handles the
                // `converges_visibility_for_wrapped_rows` and
                // `retries_window_when_point_starts_below_visible_span`
                // tests, which construct a fresh window with
                // window_start=1 and point far below, and
                // expect layout_frame_rust to publish geometry
                // that includes point without a second
                // redisplay pass.
                let has_prev_end = params.window_end > 0 && params.point > params.window_end;
                let max_visible_chars =
                    (max_rows.max(1) as i64) * (params.bounds.width.max(1.0) as i64);
                let far_below_without_prev_end =
                    params.window_end == 0 && params.point - ws > max_visible_chars;
                has_prev_end || far_below_without_prev_end
            } {
                // Mirror GNU/legacy forward scroll: when point moved below the
                // previous visible end, choose a new start before layout so the
                // current redisplay already includes point.
                let target_rows_above = ((max_rows * 3) / 4).max(1) as i64;
                let mut lines_back: i64 = 0;
                let mut scan_pos = params.point;
                while scan_pos > params.buffer_begv && lines_back < target_rows_above {
                    scan_pos -= 1;
                    let bp = buf_access.charpos_to_bytepos(scan_pos);
                    if buf_access.byte_at(bp) == Some(b'\n') {
                        lines_back += 1;
                    }
                }
                ws = scan_pos.max(params.buffer_begv);
                tracing::debug!(
                    "layout_window_rust: forward-adjusted window_start {} -> {} (point={}, prev_end={})",
                    params.window_start,
                    ws,
                    params.point,
                    params.window_end
                );
            }
            ws
        };
        // GNU Emacs redisplay advances iterators until the visible window is
        // fully resolved; it does not stop at an arbitrary "rows * cols"
        // character budget.  Capping the text slice here truncates long
        // wrapped or truncated lines before they are actually offscreen, which
        // breaks both redisplay and geometry queries.
        let read_chars = params.buffer_size - window_start + 1;

        let text_start_byte = buf_access.charpos_to_bytepos(window_start) as usize;
        let bytes_read = if read_chars <= 0 {
            0i64
        } else {
            let text_end = (window_start + read_chars).min(params.buffer_size);
            let byte_to = buf_access.charpos_to_bytepos(text_end);
            buf_access.copy_text(text_start_byte as i64, byte_to, &mut self.text_buf);
            self.text_buf.len() as i64
        };

        let text = if bytes_read > 0 {
            &self.text_buf[..bytes_read as usize]
        } else {
            &[]
        };
        let transition_hints_len_before = self.matrix_builder.transition_hints().len();
        let effect_hints_len_before = self.matrix_builder.effect_hints().len();

        tracing::debug!(
            "  layout_window_rust id={}: text_y={:.1} text_h={:.1} max_rows={} bytes_read={}",
            params.window_id,
            text_y,
            text_height,
            max_rows,
            bytes_read
        );

        if text_height <= 0.0 || text_width <= 0.0 {
            return;
        }

        // Per-face metrics — start with defaults, updated on face change
        let mut face_char_w = default_face_char_w;
        let mut face_space_w;
        let mut face_h = default_face_h;
        let mut face_ascent_val = default_face_ascent;

        // Face resolution state
        let mut face_next_check: usize = 0;
        // Load the frame-wide face-id counter so this window's
        // glyph/mode-line/header-line faces get IDs that do NOT
        // collide with earlier siblings' faces in the frame-scoped
        // `matrix_builder.faces` HashMap. Write back below before
        // returning. Mirrors GNU's single `face_cache->used`
        // counter per frame at `src/xfaces.c::lookup_face` /
        // `init_frame_faces`.
        let mut current_face_id: u32 = self.frame_face_id_counter.max(1);
        let mut _current_fg: Color = default_fg; // tracks foreground across face changes
        let mut current_bg: Color = default_bg; // tracks background across face changes
        let mut current_font_family = if default_resolved.font_family.is_empty() {
            "monospace".to_string()
        } else {
            default_resolved.font_family.clone()
        };
        let mut current_font_weight = default_resolved.font_weight;
        let mut current_font_italic = default_resolved.italic;
        let mut current_font_size_px = default_resolved.font_size.max(1.0).round() as i32;

        self.current_resolved_family = current_font_family.clone();
        self.resolved_family_face_id = 0;
        face_space_w = unsafe {
            char_advance(
                &mut self.ascii_width_cache,
                &mut self.font_metrics,
                ' ',
                1,
                char_w,
                current_font_size_px,
                face_char_w,
                &self.current_resolved_family,
                current_font_weight,
                current_font_italic,
            )
        };

        if let Some(echo_message) = echo_message {
            // The echo area is minibuffer content, not post-window chrome.
            // Size the matrix from the echo text itself so the existing
            // minibuffer auto-resize pass can grow the physical minibuffer
            // window when a multiline message needs more rows than the
            // current one-line allocation.
            let echo_lines = echo_message
                .split(|ch| ch == '\n' || ch == '\r')
                .count()
                .max(1);
            let frame_rows = _frame_params.height / char_h;
            let max_mini = max_mini_window_lines(evaluator, frame_rows).ceil().max(1.0) as usize;
            let max_rows_echo = echo_lines.clamp(1, max_mini);
            let cols_echo = (text_width / char_w).ceil().max(1.0) as usize;
            self.matrix_builder.begin_window(
                params.window_id as u64,
                max_rows_echo,
                cols_echo,
                params.bounds,
                params.selected,
            );
            let (rendered_face, rows) = self.render_minibuffer_echo_via_backend(
                text_width,
                char_w,
                default_face_ascent,
                char_h,
                default_resolved,
                echo_message,
                max_rows_echo,
            );
            self.matrix_builder
                .insert_face(rendered_face.id, rendered_face);
            let row_ascent = default_face_ascent.max(
                self.matrix_builder
                    .faces()
                    .get(&0)
                    .map(|face| face.font_ascent.max(0) as f32)
                    .unwrap_or(0.0),
            );
            for (row_index, glyphs) in rows.into_iter().enumerate() {
                self.matrix_builder.begin_row(
                    row_index,
                    neomacs_display_protocol::frame_glyphs::GlyphRowRole::Minibuffer,
                );
                self.matrix_builder.set_current_row_metrics(
                    params.bounds.y + row_index as f32 * char_h,
                    char_h,
                    row_ascent,
                );
                self.matrix_builder.install_current_row_glyphs(glyphs);
                self.matrix_builder.end_row();
            }
            self.matrix_builder.end_window();
            return;
        }

        // Line number state
        let window_start_byte = buf_access.charpos_to_bytepos(window_start);
        let begin_byte = if lnum_widen { 0 } else { buf_access.begv() };
        let mut current_line: i64 = if lnum_enabled {
            buf_access.count_lines(begin_byte, window_start_byte) + 1
        } else {
            1
        };
        let point_line: i64 = if lnum_enabled && lnum_mode >= 2 {
            let pt_byte = buf_access.charpos_to_bytepos(params.point);
            buf_access.count_lines(begin_byte, pt_byte) + 1
        } else {
            0
        };
        let mut need_line_number = lnum_enabled;

        // Simple monospace text layout
        let mut x = content_x;
        let mut y = text_y;
        let mut row = 0usize;
        let mut col = 0usize;
        let mut byte_idx = 0usize;
        let mut charpos = window_start;
        let mut invis_next_check: i64 = window_start; // Next position where visibility might change
        let mut display_next_check: i64 = window_start; // Next position where display props might change

        // Display :raise property: vertical Y offset for glyphs
        let mut raise_y_offset: f32 = 0.0;
        let mut raise_end: i64 = window_start;

        // Display :height property: font scale factor
        let mut height_scale: f32 = 0.0; // 0.0 = no scaling
        let mut height_end: i64 = window_start;

        // Fringe state tracking
        let left_fringe_x = params.text_bounds.x - params.left_fringe_width;
        let right_fringe_x = params.text_bounds.x + params.text_bounds.width;
        let mut row_continued = vec![false; max_rows];
        let mut row_truncated = vec![false; max_rows];
        let mut row_continuation = vec![false; max_rows];

        // Horizontal scroll: skip first hscroll columns on each line
        let hscroll = if params.truncate_lines {
            params.hscroll.max(0) as i32
        } else {
            0
        };
        let show_left_trunc = hscroll > 0;
        let mut hscroll_remaining = hscroll;

        // Word-wrap break tracking
        let mut wrap_break_byte_idx = 0usize;
        let mut wrap_break_charpos = window_start;
        let mut _wrap_break_x: f32 = 0.0;
        let mut _wrap_break_col = 0usize;
        let mut wrap_break_display_point_count = 0usize;
        let mut wrap_break_row_first_display_pos: Option<usize> = None;
        let mut wrap_break_row_last_display_pos: Option<usize> = None;
        let mut wrap_has_break = false;
        let mut word_wrap_may_wrap = false;

        // Line/wrap prefix tracking: 0=none, 1=line-prefix, 2=wrap-prefix
        let mut need_prefix: u8 = if has_prefix && line_prefix_str.is_some() {
            1
        } else {
            0
        };

        let reserve_right_border_width = if reserve_right_border_col {
            char_w
        } else {
            0.0
        };
        let avail_width = (text_width - lnum_pixel_width - reserve_right_border_width).max(0.0);

        // Variable-height row tracking
        let mut row_max_height: f32 = char_h; // max glyph height on current row
        let mut row_max_ascent: f32 = default_face_ascent; // max ascent on current row
        let mut row_extra_y: f32 = 0.0; // cumulative extra height from previous rows
        let mut row_y_positions: Vec<f32> = Vec::with_capacity(max_rows);
        row_y_positions.push(text_y); // row 0
        // Trailing whitespace tracking
        let trailing_ws_bg = if params.show_trailing_whitespace {
            Some(Color::from_pixel(params.trailing_ws_bg))
        } else {
            None
        };
        let mut trailing_ws_start_col: i32 = -1; // -1 = no trailing ws
        let mut trailing_ws_start_x: f32 = 0.0;
        let mut trailing_ws_row: usize = 0;

        // Check if the buffer has any overlays (optimization: skip per-char overlay checks if empty)
        let has_overlays = !buffer.overlays.is_empty();

        // Face :extend tracking — extends face background to end of line
        let mut row_extend_bg: Option<(Color, u32)> = None; // (bg_color, face_id)
        let mut row_extend_row: i32 = -1;

        // Box face tracking: track active :box face regions
        let mut box_active = false;
        let mut box_start_x: f32 = 0.0;
        let mut box_row: usize = 0;

        // Cursor metrics captured during the main layout loop.
        let mut cursor_info: Option<CapturedCursorInfo> = None;

        // Hit-test data for this window
        let mut hit_rows: Vec<HitRow> = Vec::new();
        let mut hit_row_charpos_start: i64 = window_start;
        let text_area_left = text_x;
        let window_top = params.bounds.y;
        let mut output_emitter = WindowOutputEmitter::new(
            frame_id,
            window_id,
            text_matrix_row_base,
            text_area_left,
            window_top,
        );
        output_emitter.begin_update(evaluator);
        let sync_charpos_from_byte_idx = |byte_idx: usize| {
            buf_access.bytepos_to_charpos(text_start_byte as i64 + byte_idx as i64)
        };

        let ligatures = self.ligatures_enabled;
        self.run_buf.clear();

        // Margin state tracking
        let has_margins = params.left_margin_width > 0.0 || params.right_margin_width > 0.0;

        // Clear margin backgrounds with default face background so they don't
        // show visual artifacts.  Default Emacs layout (fringes-outside-margins
        // nil): | LEFT_MARGIN | LEFT_FRINGE | TEXT_AREA | RIGHT_FRINGE | RIGHT_MARGIN |
        // So left margin is outermost (before fringe), right margin is outermost
        // (after fringe).
        if has_margins {
            if params.left_margin_width > 0.0 {
                let _margin_x = text_x - params.left_fringe_width - params.left_margin_width;
            }
            if params.right_margin_width > 0.0 {
                let _margin_x = text_x + text_width + params.right_fringe_width;
            }
        }

        macro_rules! resolve_current_face_state {
            () => {
                if (charpos as usize) >= face_next_check {
                    flush_run(&self.run_buf, ligatures);
                    self.run_buf.clear();
                    let resolved =
                        face_resolver.face_at_pos(buffer, charpos as usize, &mut face_next_check);
                    let face_id = current_face_id;

                    let metrics = self.font_metrics.as_mut().map(|svc| {
                        svc.font_metrics(
                            &resolved.font_family,
                            resolved.font_weight,
                            resolved.italic,
                            resolved.font_size,
                        )
                    });
                    if let Some(m) = metrics {
                        face_char_w = m.char_width;
                        face_h = m.line_height;
                        face_ascent_val = m.ascent;
                    } else {
                        face_char_w = char_w;
                        face_h = char_h;
                        face_ascent_val = font_ascent;
                    }

                    if face_h > row_max_height {
                        row_max_height = face_h;
                    }
                    if face_ascent_val > row_max_ascent {
                        row_max_ascent = face_ascent_val;
                    }

                    let fg = Color::from_pixel(resolved.fg);
                    _current_fg = fg;
                    let bg = Color::from_pixel(resolved.bg);
                    current_bg = bg;
                    current_font_family = if resolved.font_family.is_empty() {
                        "monospace".to_string()
                    } else {
                        resolved.font_family.clone()
                    };
                    current_font_weight = resolved.font_weight;
                    current_font_italic = resolved.italic;
                    current_font_size_px = resolved.font_size.max(1.0).round() as i32;
                    self.current_resolved_family = current_font_family.clone();
                    self.resolved_family_face_id = face_id;
                    face_space_w = unsafe {
                        char_advance(
                            &mut self.ascii_width_cache,
                            &mut self.font_metrics,
                            ' ',
                            1,
                            char_w,
                            current_font_size_px,
                            face_char_w,
                            &self.current_resolved_family,
                            current_font_weight,
                            current_font_italic,
                        )
                    };

                    apply_resolved_face(&mut self.matrix_builder, face_id, &resolved, metrics);
                    current_face_id += 1;

                    if resolved.extend {
                        let ext_bg = Color::from_pixel(resolved.bg);
                        row_extend_bg = Some((ext_bg, face_id));
                        row_extend_row = row as i32;
                    }

                    if box_active && resolved.box_type == 0 {
                        box_active = false;
                    }
                    if resolved.box_type > 0 {
                        box_active = true;
                        box_start_x = x;
                        box_row = row;
                    }
                }
            };
        }

        macro_rules! save_word_wrap_candidate {
            ($ch:expr, $break_byte_idx:expr) => {
                if params.word_wrap && word_wrap_may_wrap && char_can_wrap_before_basic($ch) {
                    flush_run(&self.run_buf, ligatures);
                    self.run_buf.clear();
                    wrap_break_byte_idx = $break_byte_idx;
                    wrap_break_charpos = charpos;
                    wrap_break_display_point_count = output_emitter.display_point_len();
                    (
                        wrap_break_row_first_display_pos,
                        wrap_break_row_last_display_pos,
                    ) = output_emitter.current_row_display_positions();
                    wrap_has_break = true;
                }
            };
        }

        // --- GlyphMatrix builder: begin window and first row ---
        let matrix_rows = text_matrix_row_base + text_matrix_rows + bottom_chrome_rows;
        let matrix_cols = cols.max(1);
        self.matrix_builder.begin_window(
            params.window_id as u64,
            matrix_rows,
            matrix_cols,
            params.bounds,
            params.selected,
        );
        self.matrix_builder.begin_row(
            text_matrix_row_base,
            neomacs_display_protocol::frame_glyphs::GlyphRowRole::Text,
        );
        output_emitter.begin_text_row(evaluator, row, col, y, x);

        while byte_idx < text.len() && row < max_rows && y + row_max_height <= text_y + text_height
        {
            // Render line number at start of each visual line
            if need_line_number && lnum_enabled {
                let display_num = match lnum_mode {
                    2 | 3 => {
                        // Relative/visual mode
                        if lnum_current_absolute && current_line == point_line {
                            (current_line + lnum_offset).abs()
                        } else {
                            (current_line - point_line).abs()
                        }
                    }
                    _ => {
                        // Absolute mode
                        (current_line + lnum_offset).abs()
                    }
                };

                // Resolve line number face
                let is_current = current_line == point_line;
                let lnum_face = if is_current {
                    face_resolver.resolve_named_face("line-number-current-line")
                } else if lnum_major_tick > 0 && current_line % lnum_major_tick as i64 == 0 {
                    face_resolver.resolve_named_face("line-number-major-tick")
                } else {
                    face_resolver.resolve_named_face("line-number")
                };
                let _lnum_bg = Color::from_pixel(lnum_face.bg);
                // Realize and register the line-number face so the renderer
                // uses the same family/weight/slant the layout chose.
                apply_resolved_face(&mut self.matrix_builder, current_face_id, &lnum_face, None);
                let lnum_face_id = current_face_id;
                current_face_id += 1;

                // Format number right-aligned
                let num_str = format!("{}", display_num);
                let num_chars = num_str.len() as i32;
                let padding = (lnum_cols - 1) - num_chars; // -1 for trailing space

                let _gy = y;

                // Leading padding (stretch)
                if padding > 0 {
                    self.matrix_builder
                        .push_left_margin_stretch(padding as u16, lnum_face_id);
                }

                // Number digits
                for (i, ch) in num_str.chars().enumerate() {
                    let _dx = text_x + (padding.max(0) + i as i32) as f32 * char_w;
                    self.matrix_builder.push_left_margin_char(ch, lnum_face_id);
                }

                // Trailing space separator
                let _space_x = text_x + (lnum_cols - 1) as f32 * char_w;
                self.matrix_builder
                    .push_left_margin_stretch(1, lnum_face_id);

                // Force face resolution to re-apply text face after line number face
                face_next_check = 0;

                need_line_number = false;
            }

            // --- Line/wrap prefix rendering ---
            if need_prefix > 0 {
                // Check text property prefix first (overrides buffer-local)
                let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                let prefix = if need_prefix == 2 {
                    text_props
                        .get_text_prop_string(charpos, "wrap-prefix")
                        .or_else(|| wrap_prefix_str.as_deref().map(|s| s.to_string()))
                } else {
                    text_props
                        .get_text_prop_string(charpos, "line-prefix")
                        .or_else(|| line_prefix_str.as_deref().map(|s| s.to_string()))
                };

                if let Some(prefix_text) = prefix {
                    // Flush ligature run before prefix
                    flush_run(&self.run_buf, ligatures);
                    self.run_buf.clear();

                    let right_limit = content_x + avail_width;
                    for pch in prefix_text.chars() {
                        if pch == '\n' || pch == '\r' {
                            continue;
                        }
                        let p_cols = if is_wide_char(pch) { 2 } else { 1 };
                        let p_adv = p_cols as f32 * face_char_w;
                        if x + p_adv > right_limit {
                            break;
                        }
                        x += p_adv;
                        col += p_cols as usize;
                    }
                }
                need_prefix = 0;
            }

            // --- Invisible text check ---
            // Only call check_invisible at property change boundaries for efficiency
            if charpos >= invis_next_check {
                let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                let (invisible, next_visible) = text_props.check_invisible(charpos);
                if invisible.hidden {
                    let skip_to = next_visible.min(params.buffer_size);
                    let point_in_hidden_region =
                        cursor_info.is_none() && params.point >= charpos && params.point < skip_to;
                    if point_in_hidden_region {
                        capture_cursor_info(
                            &mut cursor_info,
                            CapturedCursorInfo {
                                x,
                                y,
                                face_w: face_char_w,
                                face_h,
                                face_ascent: face_ascent_val,
                                bg: current_bg,
                                byte_idx,
                                col,
                                face_id: current_face_id.saturating_sub(1),
                                face_space_w,
                                matrix_row: row,
                                slot_width: Some(face_char_w.max(1.0)),
                                stretch_like: false,
                            },
                        );
                    }

                    // Skip to next_visible position
                    while charpos < skip_to && byte_idx < text.len() {
                        let (_ch, ch_len) = decode_utf8(&text[byte_idx..]);
                        byte_idx += ch_len;
                        charpos += 1;
                    }
                    invis_next_check = next_visible;

                    // GNU displays ellipsis only when the matching
                    // `buffer-invisibility-spec' entry requests it.
                    if invisible.ellipsis {
                        flush_run(&self.run_buf, ligatures);
                        self.run_buf.clear();
                        let right_limit = content_x + avail_width;
                        for _ in 0..3 {
                            if x + face_char_w > right_limit {
                                break;
                            }
                            let dot_start_x = x;
                            let dot_start_col = col;
                            self.matrix_builder.push_char(
                                '.',
                                current_face_id.saturating_sub(1),
                                charpos.max(0) as usize,
                            );
                            x += face_char_w;
                            col += 1;
                            output_emitter.emit_synthetic_text_span(
                                evaluator,
                                row,
                                y,
                                dot_start_x,
                                x - dot_start_x,
                                dot_start_col,
                                col,
                            );
                        }
                    }

                    // Check for overlay strings at invisible region boundary.
                    // Packages like org-mode use overlay after-strings at invisible
                    // boundaries to show fold indicators (e.g. "[N lines]").
                    if has_overlays {
                        let invis_text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                        let (_before_strings, after_strings) =
                            invis_text_props.overlay_strings_at(charpos);
                        if !after_strings.is_empty() {
                            flush_run(&self.run_buf, ligatures);
                            self.run_buf.clear();
                            let right_limit = content_x + avail_width;
                            for (string_bytes, overlay_id) in &after_strings {
                                let ov_face = buffer
                                    .overlays
                                    .overlay_get_named(*overlay_id, Value::symbol("face"))
                                    .and_then(|val| face_resolver.resolve_face_from_value(&val));
                                render_overlay_string(
                                    evaluator,
                                    &mut output_emitter,
                                    string_bytes,
                                    &mut x,
                                    &mut y,
                                    &mut col,
                                    &mut row,
                                    &mut hit_rows,
                                    &mut hit_row_charpos_start,
                                    charpos,
                                    &mut row_y_positions,
                                    &mut row_max_height,
                                    &mut row_max_ascent,
                                    face_char_w,
                                    char_h,
                                    default_face_ascent,
                                    right_limit,
                                    content_x,
                                    text_y,
                                    row_extra_y,
                                    text_matrix_row_base,
                                    max_rows,
                                    ov_face.as_ref(),
                                    &mut current_face_id,
                                    &mut self.matrix_builder,
                                );
                            }
                        }
                    }

                    flush_run(&self.run_buf, ligatures);
                    self.run_buf.clear();
                    continue;
                }
                invis_next_check = next_visible;
            }

            // Handle hscroll: skip columns consumed by horizontal scroll
            if hscroll_remaining > 0 {
                flush_run(&self.run_buf, ligatures);
                self.run_buf.clear();
                let ch_start_byte_idx = byte_idx;
                let (ch, ch_len) = decode_utf8(&text[byte_idx..]);
                byte_idx += ch_len;
                charpos += 1;

                if ch == '\n' {
                    // Newline within hscroll region: advance to next row
                    if row_max_height > char_h {
                        row_extra_y += row_max_height - char_h;
                    }
                    x = content_x;
                    // Record newline position on the row (see main \n handler).
                    output_emitter.note_display_buffer_pos(charpos as usize);
                    // Record hit-test row (hscroll newline)
                    hit_rows.push(HitRow {
                        y_start: y,
                        y_end: y + row_max_height,
                        charpos_start: hit_row_charpos_start,
                        charpos_end: charpos,
                    });
                    finish_text_row(
                        &mut self.matrix_builder,
                        &mut output_emitter,
                        y,
                        row_max_height,
                        row_max_ascent,
                    );
                    hit_row_charpos_start = charpos;
                    row_extend_bg = None;
                    row_extend_row = -1;

                    row += 1;
                    y = text_y + row as f32 * char_h + row_extra_y;
                    row_max_height = char_h;
                    row_max_ascent = default_face_ascent;
                    row_y_positions.push(y);
                    output_emitter.begin_text_row(evaluator, row, col, y, x);
                    col = 0;
                    current_line += 1;
                    need_line_number = lnum_enabled;
                    hscroll_remaining = hscroll; // reset for next line
                    trailing_ws_start_col = -1;
                    if has_prefix {
                        need_prefix = 1;
                    }
                    if cursor_info.is_none() && params.point == charpos {
                        capture_cursor_info(
                            &mut cursor_info,
                            CapturedCursorInfo {
                                x,
                                y,
                                face_w: face_char_w,
                                face_h: char_h,
                                face_ascent: face_ascent_val,
                                bg: current_bg,
                                byte_idx: ch_start_byte_idx,
                                col,
                                face_id: current_face_id.saturating_sub(1),
                                face_space_w,
                                matrix_row: row,
                                slot_width: Some(face_char_w.max(1.0)),
                                stretch_like: false,
                            },
                        );
                    }
                } else {
                    let ch_cols: i32 = if ch == '\t' {
                        let tab_w = params.tab_width.max(1) as i32;
                        let consumed = hscroll - hscroll_remaining;
                        ((consumed / tab_w + 1) * tab_w) - consumed
                    } else if is_wide_char(ch) {
                        2
                    } else {
                        1
                    };
                    hscroll_remaining -= ch_cols.min(hscroll_remaining);

                    // When hscroll is exhausted, show $ indicator at left edge
                    if hscroll_remaining <= 0 && show_left_trunc {
                        let trunc_start_x = content_x;
                        col = 1; // $ takes 1 column
                        x = content_x + char_w;
                        output_emitter.emit_synthetic_text_span(
                            evaluator,
                            row,
                            y,
                            trunc_start_x,
                            x - trunc_start_x,
                            0,
                            col,
                        );
                    }
                    if cursor_info.is_none() && params.point == charpos {
                        capture_cursor_info(
                            &mut cursor_info,
                            CapturedCursorInfo {
                                x,
                                y,
                                face_w: face_char_w,
                                face_h,
                                face_ascent: face_ascent_val,
                                bg: current_bg,
                                byte_idx: ch_start_byte_idx,
                                col,
                                face_id: current_face_id.saturating_sub(1),
                                face_space_w,
                                matrix_row: row,
                                slot_width: Some(face_char_w.max(1.0)),
                                stretch_like: false,
                            },
                        );
                    }
                }
                continue;
            }

            // --- Display property check ---
            // Only call check_display_prop at property change boundaries for efficiency
            if charpos >= display_next_check {
                let display_prop_val: Option<neovm_core::emacs_core::Value> = {
                    let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                    let (dp, next_change) = text_props.check_display_prop(charpos);
                    display_next_check = next_change;
                    dp
                };

                if let Some(prop_val) = display_prop_val {
                    flush_run(&self.run_buf, ligatures);
                    self.run_buf.clear();
                    let skip_to = display_next_check.min(params.buffer_size);
                    let point_in_display_replacement =
                        cursor_info.is_none() && params.point >= charpos && params.point < skip_to;
                    // Case 1: String replacement — render the string instead of buffer text
                    if let Some(replacement) = prop_val.as_utf8_str() {
                        let replacement_start_x = x;
                        let replacement_start_col = col;
                        if point_in_display_replacement {
                            let slot_width = replacement
                                .chars()
                                .next()
                                .map(|rch| slot_char_width(rch, face_char_w))
                                .unwrap_or_else(|| face_char_w.max(1.0));
                            capture_cursor_info(
                                &mut cursor_info,
                                CapturedCursorInfo {
                                    x,
                                    y,
                                    face_w: face_char_w,
                                    face_h,
                                    face_ascent: face_ascent_val,
                                    bg: current_bg,
                                    byte_idx,
                                    col,
                                    face_id: current_face_id.saturating_sub(1),
                                    face_space_w,
                                    matrix_row: row,
                                    slot_width: Some(slot_width.max(1.0)),
                                    stretch_like: false,
                                },
                            );
                        }
                        if !replacement.is_empty() {
                            let right_limit = content_x + (text_width - lnum_pixel_width);
                            for rch in replacement.chars() {
                                let rch_is_wide = is_wide_char(rch);
                                let rch_advance = if rch_is_wide {
                                    2.0 * face_char_w
                                } else {
                                    face_char_w
                                };
                                if x + rch_advance > right_limit {
                                    break;
                                }
                                if rch_is_wide {
                                    self.matrix_builder.push_wide_char(
                                        rch,
                                        current_face_id.saturating_sub(1),
                                        charpos as usize,
                                    );
                                } else {
                                    self.matrix_builder.push_char(
                                        rch,
                                        current_face_id.saturating_sub(1),
                                        charpos as usize,
                                    );
                                }
                                x += rch_advance;
                                col += if rch_is_wide { 2 } else { 1 };
                            }
                        }

                        if x > replacement_start_x || col > replacement_start_col {
                            output_emitter.emit_text_span(
                                evaluator,
                                charpos as i64 + 1,
                                row,
                                y,
                                replacement_start_x,
                                y + raise_y_offset,
                                x - replacement_start_x,
                                face_h,
                                replacement_start_col,
                                col,
                            );
                        }

                        // Skip the buffer text that this display property covers
                        while charpos < skip_to && byte_idx < text.len() {
                            let (_ch, ch_len) = decode_utf8(&text[byte_idx..]);
                            byte_idx += ch_len;
                            charpos += 1;
                        }
                        continue;
                    }

                    // Case 2: Space spec — (space :width …) or (space :align-to …)
                    if is_display_space_spec(&prop_val) {
                        let replacement_start_x = x;
                        let replacement_start_col = col;
                        let space_width = eval_display_space_as_width(
                            &prop_val,
                            x,
                            content_x,
                            face_char_w,
                            params,
                        );
                        if point_in_display_replacement {
                            capture_cursor_info(
                                &mut cursor_info,
                                CapturedCursorInfo {
                                    x,
                                    y,
                                    face_w: face_char_w,
                                    face_h,
                                    face_ascent: face_ascent_val,
                                    bg: current_bg,
                                    byte_idx,
                                    col,
                                    face_id: current_face_id.saturating_sub(1),
                                    face_space_w,
                                    matrix_row: row,
                                    slot_width: Some(space_width.max(face_char_w).max(1.0)),
                                    stretch_like: true,
                                },
                            );
                        }
                        if space_width > 0.0 {
                            let _bg = Color::from_pixel(default_resolved.bg);
                            self.matrix_builder.push_stretch(
                                (space_width / face_char_w).ceil() as u16,
                                current_face_id.saturating_sub(1),
                            );
                            x += space_width;
                            col += (space_width / face_char_w).ceil() as usize;
                            output_emitter.emit_text_span(
                                evaluator,
                                charpos as i64 + 1,
                                row,
                                y,
                                replacement_start_x,
                                y + raise_y_offset,
                                x - replacement_start_x,
                                face_h,
                                replacement_start_col,
                                col,
                            );
                        }

                        // Skip covered buffer text
                        while charpos < skip_to && byte_idx < text.len() {
                            let (_ch, ch_len) = decode_utf8(&text[byte_idx..]);
                            byte_idx += ch_len;
                            charpos += 1;
                        }
                        continue;
                    }

                    // Case 3: Image — emit a real inline image glyph when a GUI
                    // display host can resolve it, otherwise keep the TTY placeholder.
                    if is_display_image_spec(&prop_val) {
                        let replacement_start_x = x;
                        let replacement_start_col = col;
                        let maybe_image = parse_display_image_layout(&prop_val).and_then(|spec| {
                            evaluator
                                .display_host
                                .as_ref()
                                .and_then(|host| host.resolve_image(spec.request).ok().flatten())
                                .map(|resolved| (spec.scale, resolved))
                        });

                        if let Some((scale, resolved)) = maybe_image {
                            let mut display_width = resolved.width.max(1) as f32;
                            let mut display_height = resolved.height.max(1) as f32;
                            if (scale - 1.0).abs() > f32::EPSILON
                                && scale.is_finite()
                                && scale > 0.0
                            {
                                display_width = (display_width * scale).round().max(1.0);
                                display_height = (display_height * scale).round().max(1.0);
                            }

                            if point_in_display_replacement {
                                capture_cursor_info(
                                    &mut cursor_info,
                                    CapturedCursorInfo {
                                        x,
                                        y,
                                        face_w: face_char_w,
                                        face_h: display_height,
                                        face_ascent: display_height,
                                        bg: current_bg,
                                        byte_idx,
                                        col,
                                        face_id: current_face_id.saturating_sub(1),
                                        face_space_w,
                                        matrix_row: row,
                                        slot_width: Some(display_width.max(1.0)),
                                        stretch_like: false,
                                    },
                                );
                            }

                            let slot_id = DisplaySlotId {
                                window_id: params.window_id,
                                row: window_text_row_u32(row),
                                col: col as u16,
                            };
                            let image_y = y + raise_y_offset;
                            self.matrix_builder.push_image_with_slot_id(
                                params.window_id,
                                GlyphRowRole::Text,
                                Some(params.text_bounds),
                                slot_id,
                                resolved.image_id,
                                x,
                                image_y,
                                display_width,
                                display_height,
                            );
                            row_max_height = row_max_height.max(display_height);
                            row_max_ascent = row_max_ascent.max(display_height);
                            x += display_width;
                            col += ((display_width / face_char_w.max(1.0)).ceil() as usize).max(1);
                            output_emitter.emit_text_span(
                                evaluator,
                                charpos as i64 + 1,
                                row,
                                y,
                                replacement_start_x,
                                image_y,
                                x - replacement_start_x,
                                display_height,
                                replacement_start_col,
                                col,
                            );
                        } else {
                            if point_in_display_replacement {
                                capture_cursor_info(
                                    &mut cursor_info,
                                    CapturedCursorInfo {
                                        x,
                                        y,
                                        face_w: face_char_w,
                                        face_h,
                                        face_ascent: face_ascent_val,
                                        bg: current_bg,
                                        byte_idx,
                                        col,
                                        face_id: current_face_id.saturating_sub(1),
                                        face_space_w,
                                        matrix_row: row,
                                        slot_width: Some(face_char_w.max(1.0)),
                                        stretch_like: false,
                                    },
                                );
                            }
                            let placeholder = "[img]";
                            let right_limit = content_x + (text_width - lnum_pixel_width);
                            for _rch in placeholder.chars() {
                                if x + face_char_w > right_limit {
                                    break;
                                }
                                x += face_char_w;
                                col += 1;
                            }
                            if x > replacement_start_x || col > replacement_start_col {
                                output_emitter.emit_text_span(
                                    evaluator,
                                    charpos as i64 + 1,
                                    row,
                                    y,
                                    replacement_start_x,
                                    y + raise_y_offset,
                                    x - replacement_start_x,
                                    face_h,
                                    replacement_start_col,
                                    col,
                                );
                            }
                        }

                        // Skip covered buffer text
                        while charpos < skip_to && byte_idx < text.len() {
                            let (_ch, ch_len) = decode_utf8(&text[byte_idx..]);
                            byte_idx += ch_len;
                            charpos += 1;
                        }
                        continue;
                    }

                    // Case 4: Raise — (raise FACTOR) or plist with :raise
                    if let Some(factor) = parse_display_raise_factor(&prop_val) {
                        raise_y_offset = -(factor * char_h);
                        raise_end = display_next_check;
                    }

                    // Case 5: Height — (height FACTOR) or plist with :height
                    if let Some(factor) = parse_display_height_factor(&prop_val) {
                        if factor > 0.0 {
                            height_scale = factor;
                            height_end = display_next_check;
                        }
                    }
                    // Other display property types: fall through to normal rendering
                }
            }

            // Decode UTF-8 character. Keep the original byte/char position so
            // character-wrap can resume from the same buffer position on the
            // next visual row, like GNU Emacs restoring its iterator state.
            let ch_start_byte_idx = byte_idx;
            let _ch_start_charpos = charpos;
            let ch = match std::str::from_utf8(&text[byte_idx..]) {
                Ok(s) => {
                    let ch = s.chars().next().unwrap_or('\u{FFFD}');
                    byte_idx += ch.len_utf8();
                    ch
                }
                Err(e) => {
                    // Partial valid UTF-8: try decoding from the valid prefix
                    let valid_up_to = e.valid_up_to();
                    if valid_up_to > 0 {
                        if let Ok(s) = std::str::from_utf8(&text[byte_idx..byte_idx + valid_up_to])
                        {
                            let ch = s.chars().next().unwrap_or('\u{FFFD}');
                            byte_idx += ch.len_utf8();
                            ch
                        } else {
                            byte_idx += 1;
                            '\u{FFFD}'
                        }
                    } else {
                        byte_idx += 1;
                        '\u{FFFD}'
                    }
                }
            };

            // Selective display: \r hides rest of line until \n
            if selective_display > 0 && ch == '\r' {
                flush_run(&self.run_buf, ligatures);
                self.run_buf.clear();
                // Show ... ellipsis indicator
                let ellipsis = "...";
                let ellipsis_start_x = x;
                let ellipsis_start_col = col;
                for _ech in ellipsis.chars() {
                    if x + face_char_w <= content_x + avail_width {
                        x += face_char_w;
                        col += 1;
                    }
                }
                output_emitter.emit_synthetic_text_span(
                    evaluator,
                    row,
                    y,
                    ellipsis_start_x,
                    x - ellipsis_start_x,
                    ellipsis_start_col,
                    col,
                );
                // Skip remaining chars until newline
                charpos += 1;
                while byte_idx < text.len() {
                    let (skip_ch, skip_len) = decode_utf8(&text[byte_idx..]);
                    byte_idx += skip_len;
                    charpos += 1;
                    if skip_ch == '\n' {
                        // Advance to next row (same as newline handler)
                        if row_max_height > char_h {
                            row_extra_y += row_max_height - char_h;
                        }
                        x = content_x;
                        hit_rows.push(HitRow {
                            y_start: y,
                            y_end: y + row_max_height,
                            charpos_start: hit_row_charpos_start,
                            charpos_end: charpos,
                        });
                        finish_text_row(
                            &mut self.matrix_builder,
                            &mut output_emitter,
                            y,
                            row_max_height,
                            row_max_ascent,
                        );
                        row_extend_bg = None;
                        row_extend_row = -1;
                        if box_active {
                            box_start_x = content_x;
                            box_row = row + 1;
                        }
                        row += 1;
                        y = text_y + row as f32 * char_h + row_extra_y;
                        row_max_height = char_h;
                        row_max_ascent = default_face_ascent;
                        row_y_positions.push(y);
                        output_emitter.begin_text_row(evaluator, row, col, y, x);
                        charpos = sync_charpos_from_byte_idx(byte_idx);
                        hit_row_charpos_start = charpos;
                        col = 0;
                        current_line += 1;
                        need_line_number = lnum_enabled;
                        hscroll_remaining = hscroll;
                        word_wrap_may_wrap = false;
                        wrap_has_break = false;
                        trailing_ws_start_col = -1;
                        if has_prefix {
                            need_prefix = 1;
                        }
                        break;
                    }
                }
                continue;
            }

            resolve_current_face_state!();
            save_word_wrap_candidate!(ch, ch_start_byte_idx);

            if ch == '\n' {
                flush_run(&self.run_buf, ligatures);
                self.run_buf.clear();
                // Highlight trailing whitespace before advancing to next row
                if let Some(_tw_bg) = trailing_ws_bg {
                    if trailing_ws_start_col >= 0 && trailing_ws_row == row {
                        let tw_x = trailing_ws_start_x;
                        let tw_w = x - tw_x;
                        if tw_w > 0.0 {}
                    }
                }
                trailing_ws_start_col = -1;

                // Face :extend: fill rest of row with extending face background
                if let Some((_ext_bg, _ext_face_id)) = row_extend_bg {
                    if row_extend_row == row as i32 {
                        let right_edge = content_x + avail_width;
                        if x < right_edge {}
                    }
                }
                row_extend_bg = None;
                row_extend_row = -1;

                // Box face tracking: box stays active across line breaks
                if box_active {
                    box_start_x = content_x;
                }

                // Newline: advance to next row
                if row_max_height > char_h {
                    row_extra_y += row_max_height - char_h;
                }
                charpos += 1;

                // Check line-spacing text property on the newline we just consumed.
                // Text property overrides buffer-local line-spacing for that line.
                let text_prop_spacing = {
                    let nl_pos = charpos - 1; // the newline char
                    let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                    text_props.check_line_spacing(nl_pos, char_h)
                };
                if text_prop_spacing > 0.0 {
                    row_extra_y += text_prop_spacing;
                } else if params.extra_line_spacing > 0.0 {
                    // Fall back to buffer-local line-spacing
                    row_extra_y += params.extra_line_spacing;
                }

                x = content_x;
                // Record the newline position so the row's
                // end_buffer_pos includes it. GNU's redisplay engine
                // counts newlines as part of the row they terminate,
                // so window-end reflects the position AFTER the last
                // newline. Without this, trailing empty rows have
                // end_buffer_pos=None and window-end falls short of
                // point-max, causing %p to show "Top" instead of "All".
                output_emitter.note_display_buffer_pos(charpos as usize);
                // Record hit-test row (newline ends the row)
                hit_rows.push(HitRow {
                    y_start: y,
                    y_end: y + row_max_height,
                    charpos_start: hit_row_charpos_start,
                    charpos_end: charpos,
                });
                finish_text_row(
                    &mut self.matrix_builder,
                    &mut output_emitter,
                    y,
                    row_max_height,
                    row_max_ascent,
                );

                self.matrix_builder.end_row();
                row += 1;
                self.matrix_builder.begin_row(
                    text_matrix_row_base + row,
                    neomacs_display_protocol::frame_glyphs::GlyphRowRole::Text,
                );
                y = text_y + row as f32 * char_h + row_extra_y;
                row_max_height = char_h;
                row_max_ascent = default_face_ascent;
                row_y_positions.push(y);
                output_emitter.begin_text_row(evaluator, row, col, y, x);
                charpos = sync_charpos_from_byte_idx(byte_idx);
                hit_row_charpos_start = charpos;
                if box_active {
                    box_row = row;
                }
                col = 0;
                current_line += 1;
                need_line_number = lnum_enabled;
                hscroll_remaining = hscroll;
                word_wrap_may_wrap = false;
                wrap_has_break = false;
                if has_prefix {
                    need_prefix = 1;
                }
                // Selective display: skip lines indented beyond threshold
                if selective_display > 0 && selective_display < i32::MAX && byte_idx < text.len() {
                    let mut shown_ellipsis = false;
                    loop {
                        if byte_idx >= text.len() {
                            break;
                        }
                        // Peek at indentation of next line
                        let mut indent = 0i32;
                        let mut peek = byte_idx;
                        while peek < text.len() {
                            let b = text[peek];
                            if b == b' ' {
                                indent += 1;
                                peek += 1;
                            } else if b == b'\t' {
                                let tab_w = params.tab_width.max(1) as i32;
                                indent = ((indent / tab_w) + 1) * tab_w;
                                peek += 1;
                            } else {
                                break;
                            }
                        }
                        if indent > selective_display {
                            // Show ... ellipsis once for the hidden block
                            if !shown_ellipsis && row > 0 {
                                let _prev_row_y = row_y_positions
                                    .get(row - 1)
                                    .copied()
                                    .unwrap_or(text_y + (row - 1) as f32 * char_h);
                                for dot_i in 0..3 {
                                    let dot_x = content_x + dot_i as f32 * face_char_w;
                                    if dot_x + face_char_w <= content_x + avail_width {}
                                }
                                shown_ellipsis = true;
                            }
                            // Skip this hidden line
                            while byte_idx < text.len() {
                                let (skip_ch, skip_len) = decode_utf8(&text[byte_idx..]);
                                byte_idx += skip_len;
                                charpos += 1;
                                if skip_ch == '\n' {
                                    current_line += 1;
                                    break;
                                }
                            }
                        } else {
                            break; // Next line is visible
                        }
                    }
                }
                continue;
            }

            if ch == '\t' {
                flush_run(&self.run_buf, ligatures);
                self.run_buf.clear();
                // Tab: advance to next tab stop using per-face char width
                let x_before_tab = x;
                let next_tab = if !params.tab_stop_list.is_empty() {
                    // Custom tab stops from tab-stop-list
                    params
                        .tab_stop_list
                        .iter()
                        .find(|&&stop| (stop as usize) > col)
                        .map(|&stop| stop as usize)
                        .unwrap_or_else(|| {
                            // Past last defined stop: use fixed tabs from last stop
                            let last = *params.tab_stop_list.last().unwrap() as usize;
                            let tab_w = params.tab_width.max(1) as usize;
                            if col >= last {
                                last + ((col - last) / tab_w + 1) * tab_w
                            } else {
                                last
                            }
                        })
                } else {
                    let tab_w = params.tab_width as usize;
                    if tab_w > 0 {
                        ((col / tab_w) + 1) * tab_w
                    } else {
                        col + 1
                    }
                };
                // Ensure tab advances at least one column
                let next_tab = next_tab.max(col + 1);
                let spaces = next_tab - col;
                if cursor_info.is_none() && params.point == charpos {
                    capture_cursor_info(
                        &mut cursor_info,
                        CapturedCursorInfo {
                            x: x_before_tab,
                            y,
                            face_w: face_char_w,
                            face_h,
                            face_ascent: face_ascent_val,
                            bg: current_bg,
                            byte_idx: ch_start_byte_idx,
                            col,
                            face_id: current_face_id.saturating_sub(1),
                            face_space_w,
                            matrix_row: row,
                            slot_width: Some((spaces as f32 * face_space_w).max(1.0)),
                            stretch_like: true,
                        },
                    );
                }
                output_emitter.emit_text_span(
                    evaluator,
                    charpos + 1,
                    row,
                    y,
                    x_before_tab,
                    y + raise_y_offset,
                    spaces as f32 * face_space_w,
                    char_h,
                    col,
                    next_tab,
                );
                self.matrix_builder
                    .push_stretch(spaces as u16, current_face_id.saturating_sub(1));
                x += spaces as f32 * face_space_w;
                col = next_tab;
                charpos += 1;
                if params.word_wrap {
                    _wrap_break_col = col;
                    _wrap_break_x = x - content_x;
                }
                word_wrap_may_wrap = char_can_wrap_after_basic(ch);
                // Track trailing whitespace (tab counts as whitespace)
                if trailing_ws_bg.is_some() && trailing_ws_start_col < 0 {
                    trailing_ws_start_col = col as i32;
                    trailing_ws_start_x = x_before_tab;
                    trailing_ws_row = row;
                }
                continue;
            }

            // Control characters: render as ^X notation
            if ch < ' ' || ch == '\x7F' {
                flush_run(&self.run_buf, ligatures);
                self.run_buf.clear();
                let _ctrl_ch = if ch == '\x7F' {
                    '?'
                } else {
                    char::from((ch as u8) + b'@')
                };
                let needed_width = 2.0 * face_char_w;

                // Check if we have room for ^X (2 columns)
                if x + needed_width > content_x + (text_width - lnum_pixel_width) {
                    // Doesn't fit — wrap or truncate
                    if params.truncate_lines {
                        if row < max_rows {
                            row_truncated[row] = true;
                        }
                        // Same byte_idx/charpos desync as the main-char
                        // truncation path: byte_idx is past the overflowing
                        // control char, but charpos hasn't been incremented
                        // for it yet. Compensate before skipping.
                        charpos += 1;
                        if skip_to_newline(text, &mut byte_idx, &mut charpos) {
                            current_line += 1;
                            need_line_number = lnum_enabled;
                        }
                        if row_max_height > char_h {
                            row_extra_y += row_max_height - char_h;
                        }
                        x = content_x;
                        // Record hit-test row (wrap/truncation break)
                        hit_rows.push(HitRow {
                            y_start: y,
                            y_end: y + row_max_height,
                            charpos_start: hit_row_charpos_start,
                            charpos_end: charpos,
                        });
                        finish_text_row(
                            &mut self.matrix_builder,
                            &mut output_emitter,
                            y,
                            row_max_height,
                            row_max_ascent,
                        );
                        row_extend_bg = None;
                        row_extend_row = -1;
                        row += 1;
                        y = text_y + row as f32 * char_h + row_extra_y;
                        row_max_height = char_h;
                        row_max_ascent = default_face_ascent;
                        row_y_positions.push(y);
                        output_emitter.begin_text_row(evaluator, row, col, y, x);
                        charpos = sync_charpos_from_byte_idx(byte_idx);
                        hit_row_charpos_start = charpos;
                        col = 0;
                        word_wrap_may_wrap = false;
                        trailing_ws_start_col = -1;
                        if has_prefix {
                            need_prefix = 1;
                        }
                        continue;
                    } else {
                        if row < max_rows {
                            row_continued[row] = true;
                        }
                        if row_max_height > char_h {
                            row_extra_y += row_max_height - char_h;
                        }
                        x = content_x;
                        // Record hit-test row (wrap/truncation break)
                        hit_rows.push(HitRow {
                            y_start: y,
                            y_end: y + row_max_height,
                            charpos_start: hit_row_charpos_start,
                            charpos_end: charpos,
                        });
                        finish_text_row(
                            &mut self.matrix_builder,
                            &mut output_emitter,
                            y,
                            row_max_height,
                            row_max_ascent,
                        );
                        hit_row_charpos_start = charpos;
                        row_extend_bg = None;
                        row_extend_row = -1;
                        row += 1;
                        y = text_y + row as f32 * char_h + row_extra_y;
                        row_max_height = char_h;
                        row_max_ascent = default_face_ascent;
                        row_y_positions.push(y);
                        output_emitter.begin_text_row(evaluator, row, col, y, x);
                        col = 0;
                        trailing_ws_start_col = -1;
                        if row < max_rows {
                            row_continuation[row] = true;
                        }
                        if has_prefix {
                            need_prefix = 2;
                        }
                        if row >= max_rows || y + row_max_height > text_y + text_height {
                            break;
                        }
                    }
                }

                // Render ^X with escape-glyph face color
                if params.escape_glyph_fg != 0 {
                    current_face_id += 1;
                }
                output_emitter.emit_text_span(
                    evaluator,
                    charpos + 1,
                    row,
                    y,
                    x,
                    y + raise_y_offset,
                    needed_width,
                    char_h,
                    col,
                    col + 2,
                );
                x += face_char_w;
                x += face_char_w;
                col += 2;
                charpos += 1;
                word_wrap_may_wrap = false;
                face_next_check = 0; // force face re-check to restore text face
                continue;
            }

            // Nobreak character display (U+00A0 non-breaking space, U+00AD soft hyphen)
            if params.nobreak_char_display > 0 && (ch == '\u{00A0}' || ch == '\u{00AD}') {
                flush_run(&self.run_buf, ligatures);
                self.run_buf.clear();
                match params.nobreak_char_display {
                    1 => {
                        // Highlight mode: render with nobreak face color
                        if params.nobreak_char_fg != 0 {
                            let _nb_fg = Color::from_pixel(params.nobreak_char_fg);
                            current_face_id += 1;
                        }
                        // Render as visible space or hyphen
                        let _display_ch = if ch == '\u{00A0}' { ' ' } else { '-' };
                        output_emitter.emit_text_span(
                            evaluator,
                            charpos + 1,
                            row,
                            y,
                            x,
                            y + raise_y_offset,
                            face_char_w,
                            char_h,
                            col,
                            col + 1,
                        );
                        x += face_char_w;
                        col += 1;
                        charpos += 1;
                        word_wrap_may_wrap = false;
                        face_next_check = 0; // restore face on next char
                        continue;
                    }
                    2 => {
                        // Escape notation mode: show as "\\ " for NBSP, "\\-" for soft hyphen
                        let _indicator = if ch == '\u{00A0}' { ' ' } else { '-' };
                        if params.nobreak_char_fg != 0 {
                            let _nb_fg = Color::from_pixel(params.nobreak_char_fg);
                            current_face_id += 1;
                        }
                        // Check if 2 columns fit
                        let needed = 2.0 * face_char_w;
                        if x + needed <= content_x + avail_width {
                            output_emitter.emit_text_span(
                                evaluator,
                                charpos + 1,
                                row,
                                y,
                                x,
                                y + raise_y_offset,
                                needed,
                                char_h,
                                col,
                                col + 2,
                            );
                            x += face_char_w;
                            x += face_char_w;
                            col += 2;
                        }
                        charpos += 1;
                        word_wrap_may_wrap = false;
                        face_next_check = 0;
                        continue;
                    }
                    _ => {} // mode 0 or unknown: fall through to normal rendering
                }
            }
            // Glyphless character detection (C1 controls, format chars, etc.)
            let glyphless = check_glyphless_char(ch);
            if glyphless > 0 {
                flush_run(&self.run_buf, ligatures);
                self.run_buf.clear();
                let replacement_start_x = x;
                let replacement_start_col = col;

                match glyphless {
                    1 => {
                        // Thin space: advance by a small amount
                        x += face_char_w * 0.25;
                        col += 1;
                    }
                    2 => {
                        // Empty box: render U+25A1 (□) character
                        if x + face_char_w <= content_x + avail_width {
                            x += face_char_w;
                            col += 1;
                        }
                    }
                    3 => {
                        // Hex code: render as U+XXXX
                        let hex_str = if (ch as u32) < 0x10000 {
                            format!("U+{:04X}", ch as u32)
                        } else {
                            format!("U+{:06X}", ch as u32)
                        };
                        let needed = hex_str.len() as f32 * face_char_w;

                        // Use glyphless-char face color if available
                        if params.glyphless_char_fg != 0 {
                            current_face_id += 1;
                        }

                        let right_limit = content_x + avail_width;
                        if x + needed <= right_limit {
                            for _hch in hex_str.chars() {
                                x += face_char_w;
                            }
                            col += hex_str.len();
                        } else {
                            // Partial rendering: emit as many chars as fit
                            for _hch in hex_str.chars() {
                                if x + face_char_w > right_limit {
                                    break;
                                }
                                x += face_char_w;
                                col += 1;
                            }
                        }
                        face_next_check = 0; // restore face on next char
                    }
                    5 => {
                        // Zero width: skip entirely (no visual output)
                    }
                    _ => {}
                }
                if x > replacement_start_x || col > replacement_start_col {
                    output_emitter.emit_text_span(
                        evaluator,
                        charpos as i64 + 1,
                        row,
                        y,
                        replacement_start_x,
                        y + raise_y_offset,
                        x - replacement_start_x,
                        face_h,
                        replacement_start_col,
                        col,
                    );
                }
                charpos += 1;
                word_wrap_may_wrap = false;
                continue;
            }

            // Check for line wrap / truncation using per-face char width

            let control_display = control_char_display_pair(
                ch,
                super::neovm_bridge::buffer_local_bool(buffer, "ctl-arrow"),
            );

            // Grapheme-cluster extenders (combining marks, ZWJ,
            // variation selectors) share the preceding base char's
            // cell — zero columns, zero advance. CJK chars occupy 2
            // columns. Everything else occupies 1.
            let is_extender = is_cluster_extender(ch);
            let char_cols = if control_display.is_some() {
                2
            } else if is_extender {
                0
            } else if is_wide_char(ch) {
                2
            } else {
                1
            };
            let advance = if control_display.is_some() {
                2.0 * face_char_w
            } else if is_extender {
                0.0
            } else {
                unsafe {
                    char_advance(
                        &mut self.ascii_width_cache,
                        &mut self.font_metrics,
                        ch,
                        char_cols as i32,
                        char_w,
                        current_font_size_px,
                        face_char_w,
                        &self.current_resolved_family,
                        current_font_weight,
                        current_font_italic,
                    )
                }
            };
            update_cursor_info_for_main_char(&mut cursor_info, ch_start_byte_idx, advance);
            if x + advance > content_x + avail_width {
                flush_run(&self.run_buf, ligatures);
                self.run_buf.clear();
                if params.truncate_lines {
                    if row < max_rows {
                        row_truncated[row] = true;
                    }
                    // The current char has been decoded and `byte_idx` is
                    // already past it, but `charpos` is not yet incremented
                    // (that happens after the would-be push below). Account
                    // for the consumed-but-uncounted char here so
                    // `skip_to_newline` starts from the right offset.
                    charpos += 1;
                    // Skip remaining chars until newline
                    if skip_to_newline(text, &mut byte_idx, &mut charpos) {
                        current_line += 1;
                        need_line_number = lnum_enabled;
                    }
                    if row_max_height > char_h {
                        row_extra_y += row_max_height - char_h;
                    }
                    x = content_x;
                    // Record hit-test row (wrap/truncation break)
                    hit_rows.push(HitRow {
                        y_start: y,
                        y_end: y + row_max_height,
                        charpos_start: hit_row_charpos_start,
                        charpos_end: charpos,
                    });
                    finish_text_row(
                        &mut self.matrix_builder,
                        &mut output_emitter,
                        y,
                        row_max_height,
                        row_max_ascent,
                    );
                    row_extend_bg = None;
                    row_extend_row = -1;
                    self.matrix_builder.end_row();
                    row += 1;
                    self.matrix_builder.begin_row(
                        text_matrix_row_base + row,
                        neomacs_display_protocol::frame_glyphs::GlyphRowRole::Text,
                    );
                    y = text_y + row as f32 * char_h + row_extra_y;
                    row_max_height = char_h;
                    row_max_ascent = default_face_ascent;
                    row_y_positions.push(y);
                    output_emitter.begin_text_row(evaluator, row, col, y, x);
                    col = 0;
                    word_wrap_may_wrap = false;
                    wrap_has_break = false;
                    trailing_ws_start_col = -1;
                    if has_prefix {
                        need_prefix = 1;
                    }
                    continue;
                } else if params.word_wrap && wrap_has_break {
                    // Word-wrap: rewind to last break point
                    output_emitter.truncate_display_points(wrap_break_display_point_count);
                    output_emitter.restore_current_row_display_positions(
                        wrap_break_row_first_display_pos,
                        wrap_break_row_last_display_pos,
                    );
                    byte_idx = wrap_break_byte_idx;
                    charpos = wrap_break_charpos;
                    col = 0;

                    if row < max_rows {
                        row_continued[row] = true;
                    }
                    if row_max_height > char_h {
                        row_extra_y += row_max_height - char_h;
                    }
                    x = content_x;
                    // Record hit-test row (wrap/truncation break)
                    hit_rows.push(HitRow {
                        y_start: y,
                        y_end: y + row_max_height,
                        charpos_start: hit_row_charpos_start,
                        charpos_end: charpos,
                    });
                    finish_text_row(
                        &mut self.matrix_builder,
                        &mut output_emitter,
                        y,
                        row_max_height,
                        row_max_ascent,
                    );
                    row_extend_bg = None;
                    row_extend_row = -1;
                    self.matrix_builder.end_row();
                    row += 1;
                    self.matrix_builder.begin_row(
                        text_matrix_row_base + row,
                        neomacs_display_protocol::frame_glyphs::GlyphRowRole::Text,
                    );
                    y = text_y + row as f32 * char_h + row_extra_y;
                    row_max_height = char_h;
                    row_max_ascent = default_face_ascent;
                    row_y_positions.push(y);
                    output_emitter.begin_text_row(evaluator, row, col, y, x);
                    charpos = sync_charpos_from_byte_idx(byte_idx);
                    hit_row_charpos_start = charpos;
                    if row < max_rows {
                        row_continuation[row] = true;
                    }
                    word_wrap_may_wrap = false;
                    wrap_has_break = false;
                    trailing_ws_start_col = -1;
                    if has_prefix {
                        need_prefix = 2;
                    }

                    // Force face re-check since we rewound
                    face_next_check = 0;

                    if row >= max_rows || y + row_max_height > text_y + text_height {
                        break;
                    }
                    continue;
                } else {
                    // Character wrap (no break point available)
                    if row < max_rows {
                        row_continued[row] = true;
                    }
                    if row_max_height > char_h {
                        row_extra_y += row_max_height - char_h;
                    }
                    x = content_x;
                    // Record hit-test row (wrap/truncation break)
                    hit_rows.push(HitRow {
                        y_start: y,
                        y_end: y + row_max_height,
                        charpos_start: hit_row_charpos_start,
                        charpos_end: charpos,
                    });
                    finish_text_row(
                        &mut self.matrix_builder,
                        &mut output_emitter,
                        y,
                        row_max_height,
                        row_max_ascent,
                    );
                    row_extend_bg = None;
                    row_extend_row = -1;
                    self.matrix_builder.end_row();
                    row += 1;
                    self.matrix_builder.begin_row(
                        text_matrix_row_base + row,
                        neomacs_display_protocol::frame_glyphs::GlyphRowRole::Text,
                    );
                    y = text_y + row as f32 * char_h + row_extra_y;
                    row_max_height = char_h;
                    row_max_ascent = default_face_ascent;
                    row_y_positions.push(y);
                    output_emitter.begin_text_row(evaluator, row, col, y, x);
                    col = 0;
                    trailing_ws_start_col = -1;
                    if row < max_rows {
                        row_continuation[row] = true;
                    }
                    byte_idx = ch_start_byte_idx;
                    charpos = sync_charpos_from_byte_idx(byte_idx);
                    hit_row_charpos_start = charpos;
                    word_wrap_may_wrap = false;
                    face_next_check = 0;
                    if has_prefix {
                        need_prefix = 2;
                    }
                    if row >= max_rows || y + row_max_height > text_y + text_height {
                        break;
                    }
                    continue;
                }
            }

            // Reset raise offset when past the raise region
            if raise_end > window_start && charpos >= raise_end {
                raise_y_offset = 0.0;
                raise_end = window_start;
            }
            // Reset height scale when past the height region
            if height_end > window_start && charpos >= height_end {
                height_scale = 0.0;
                height_end = window_start;
            }

            // Capture cursor metrics at point position during the main layout
            // so cursor emission uses the correct per-face height/width.
            if cursor_info.is_none() && charpos == params.point {
                capture_cursor_info(
                    &mut cursor_info,
                    CapturedCursorInfo {
                        x,
                        y,
                        face_w: face_char_w,
                        face_h,
                        face_ascent: face_ascent_val,
                        bg: current_bg,
                        byte_idx,
                        col,
                        face_id: current_face_id.saturating_sub(1),
                        face_space_w,
                        matrix_row: row,
                        slot_width: None,
                        stretch_like: false,
                    },
                );
            }

            // --- Overlay before-strings ---
            if has_overlays {
                let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                let (before_strings, _) = text_props.overlay_strings_at(charpos);
                if !before_strings.is_empty() {
                    // Flush run buffer before emitting overlay chars
                    flush_run(&self.run_buf, ligatures);
                    self.run_buf.clear();
                    let right_limit = content_x + avail_width;
                    for (string_bytes, overlay_id) in &before_strings {
                        let ov_face = buffer
                            .overlays
                            .overlay_get_named(*overlay_id, Value::symbol("face"))
                            .and_then(|val| face_resolver.resolve_face_from_value(&val));
                        render_overlay_string(
                            evaluator,
                            &mut output_emitter,
                            string_bytes,
                            &mut x,
                            &mut y,
                            &mut col,
                            &mut row,
                            &mut hit_rows,
                            &mut hit_row_charpos_start,
                            charpos,
                            &mut row_y_positions,
                            &mut row_max_height,
                            &mut row_max_ascent,
                            face_char_w,
                            char_h,
                            default_face_ascent,
                            right_limit,
                            content_x,
                            text_y,
                            row_extra_y,
                            text_matrix_row_base,
                            max_rows,
                            ov_face.as_ref(),
                            &mut current_face_id,
                            &mut self.matrix_builder,
                        );
                    }
                }
            }

            // Accumulate character into ligature run buffer
            if self.run_buf.is_empty() {
                let gy = y + raise_y_offset;
                self.run_buf.start(
                    x,
                    gy,
                    face_h,
                    face_ascent_val,
                    current_face_id.saturating_sub(1),
                    false,
                    height_scale,
                );
            }
            let glyph_x = x;
            let glyph_col = col;
            let buffer_pos = charpos as i64 + 1;
            if let Some((prefix, suffix)) = control_display {
                self.run_buf.push(prefix, face_char_w);
                self.run_buf.push(suffix, face_char_w);
                self.matrix_builder.push_char(
                    prefix,
                    current_face_id.saturating_sub(1),
                    charpos as usize,
                );
                self.matrix_builder.push_char(
                    suffix,
                    current_face_id.saturating_sub(1),
                    charpos as usize,
                );
            } else {
                self.run_buf.push(ch, advance);
                // Record character into GlyphMatrix builder
                if char_cols == 2 {
                    self.matrix_builder.push_wide_char(
                        ch,
                        current_face_id.saturating_sub(1),
                        charpos as usize,
                    );
                } else {
                    self.matrix_builder.push_char(
                        ch,
                        current_face_id.saturating_sub(1),
                        charpos as usize,
                    );
                }
            }

            // Flush if run is too long
            if self.run_buf.len() >= MAX_LIGATURE_RUN_LEN {
                flush_run(&self.run_buf, ligatures);
                self.run_buf.clear();
            }

            x += advance;
            col += char_cols as usize;
            output_emitter.emit_text_span(
                evaluator,
                buffer_pos,
                row,
                y,
                glyph_x,
                y + raise_y_offset,
                advance,
                face_h,
                glyph_col,
                col,
            );
            charpos += 1;
            word_wrap_may_wrap = char_can_wrap_after_basic(ch);

            // --- Overlay after-strings ---
            if has_overlays {
                let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                let (_, after_strings) = text_props.overlay_strings_at(charpos);
                if !after_strings.is_empty() {
                    // Flush run buffer before emitting overlay chars
                    flush_run(&self.run_buf, ligatures);
                    self.run_buf.clear();
                    let right_limit = content_x + avail_width;
                    for (string_bytes, overlay_id) in &after_strings {
                        let ov_face = buffer
                            .overlays
                            .overlay_get_named(*overlay_id, Value::symbol("face"))
                            .and_then(|val| face_resolver.resolve_face_from_value(&val));
                        render_overlay_string(
                            evaluator,
                            &mut output_emitter,
                            string_bytes,
                            &mut x,
                            &mut y,
                            &mut col,
                            &mut row,
                            &mut hit_rows,
                            &mut hit_row_charpos_start,
                            charpos,
                            &mut row_y_positions,
                            &mut row_max_height,
                            &mut row_max_ascent,
                            face_char_w,
                            char_h,
                            default_face_ascent,
                            right_limit,
                            content_x,
                            text_y,
                            row_extra_y,
                            text_matrix_row_base,
                            max_rows,
                            ov_face.as_ref(),
                            &mut current_face_id,
                            &mut self.matrix_builder,
                        );
                    }
                }
            }

            // Track trailing whitespace
            if trailing_ws_bg.is_some() {
                if ch == ' ' || ch == '\t' {
                    if trailing_ws_start_col < 0 {
                        trailing_ws_start_col = (col as i32) - 1;
                        trailing_ws_start_x = x - advance;
                        trailing_ws_row = row;
                    }
                } else {
                    trailing_ws_start_col = -1;
                }
            }
        }

        flush_run(&self.run_buf, ligatures);
        self.run_buf.clear();

        let point_is_visible_eob =
            params.point == params.buffer_size && charpos == params.buffer_size;

        // Capture cursor at end-of-buffer position.
        // GNU Emacs shows point at point-max+1 as a real cursor location.
        // In the layout engine's internal 0-based space, that is `buffer_size`.
        if cursor_info.is_none() && (charpos == params.point || point_is_visible_eob) {
            if point_is_visible_eob {
                tracing::debug!(
                    "layout_window_rust: capturing EOB cursor at x={:.1} y={:.1} point={} point-max={}",
                    x,
                    y,
                    params.point,
                    params.buffer_size
                );
            }
            capture_cursor_info(
                &mut cursor_info,
                CapturedCursorInfo {
                    x,
                    y,
                    face_w: face_char_w,
                    face_h,
                    face_ascent: face_ascent_val,
                    bg: current_bg,
                    byte_idx,
                    col,
                    face_id: current_face_id.saturating_sub(1),
                    face_space_w,
                    matrix_row: row,
                    slot_width: Some(face_char_w.max(1.0)),
                    stretch_like: false,
                },
            );
        }

        // Close any remaining box face region at end of text
        if box_active {
            let _ = (box_start_x, box_row); // suppress unused warnings
        }

        // EOB overlay strings: check for overlay strings at the end-of-buffer position
        if has_overlays && row < max_rows {
            let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
            let (before_strings, after_strings) = text_props.overlay_strings_at(charpos);
            let right_limit = content_x + avail_width;
            for (string_bytes, overlay_id) in before_strings.iter().chain(after_strings.iter()) {
                let ov_face = buffer
                    .overlays
                    .overlay_get_named(*overlay_id, Value::symbol("face"))
                    .and_then(|val| face_resolver.resolve_face_from_value(&val));
                render_overlay_string(
                    evaluator,
                    &mut output_emitter,
                    string_bytes,
                    &mut x,
                    &mut y,
                    &mut col,
                    &mut row,
                    &mut hit_rows,
                    &mut hit_row_charpos_start,
                    charpos,
                    &mut row_y_positions,
                    &mut row_max_height,
                    &mut row_max_ascent,
                    face_char_w,
                    char_h,
                    default_face_ascent,
                    right_limit,
                    content_x,
                    text_y,
                    row_extra_y,
                    text_matrix_row_base,
                    max_rows,
                    ov_face.as_ref(),
                    &mut current_face_id,
                    &mut self.matrix_builder,
                );
            }
        }

        // Face :extend at end-of-buffer: fill remaining empty rows
        // with the last :extend face's background color
        if let Some((_ext_bg, _ext_face_id)) = row_extend_bg {
            let right_edge = content_x + avail_width;
            // First, extend the current (partially filled) row if text didn't fill it
            if x < right_edge && row < max_rows {
                let _ry = row_y_positions
                    .get(row)
                    .copied()
                    .unwrap_or(text_y + row as f32 * char_h + row_extra_y);
            }
            // Then fill completely empty rows below
            let start_row = (row + 1).min(max_rows);
            for r in start_row..max_rows {
                let ry = row_y_positions
                    .get(r)
                    .copied()
                    .unwrap_or(text_y + r as f32 * char_h + row_extra_y);
                if ry + char_h > text_y + text_height {
                    break;
                } // Don't extend past text area
            }
        }

        // Render fringe indicators
        if params.left_fringe_width > 0.0 || params.right_fringe_width > 0.0 {
            let _fringe_char_w = params.left_fringe_width.min(char_w).max(char_w * 0.5);

            for r in 0..row.min(max_rows) {
                let _gy = row_y_positions
                    .get(r)
                    .copied()
                    .unwrap_or(text_y + r as f32 * char_h);

                // Right fringe: continuation arrow for wrapped lines
                if params.right_fringe_width > 0.0 && row_continued.get(r).copied().unwrap_or(false)
                {
                }

                // Right fringe: truncation indicator
                if params.right_fringe_width > 0.0 && row_truncated.get(r).copied().unwrap_or(false)
                {
                }

                // Left fringe: continuation from previous line
                if params.left_fringe_width > 0.0
                    && row_continuation.get(r).copied().unwrap_or(false)
                {}
            }

            // Empty line indicators (after buffer text ends)
            if params.indicate_empty_lines > 0 {
                let eob_start = row.min(max_rows);
                for r in eob_start..max_rows {
                    let _gy = row_y_positions
                        .get(r)
                        .copied()
                        .unwrap_or(text_y + r as f32 * char_h + row_extra_y);
                    let _fringe_x = if params.indicate_empty_lines == 2 {
                        right_fringe_x
                    } else {
                        left_fringe_x
                    };
                    let fringe_w = if params.indicate_empty_lines == 2 {
                        params.right_fringe_width
                    } else {
                        params.left_fringe_width
                    };
                    if fringe_w > 0.0 {}
                }
            }
        }

        // Render fill-column indicator
        if params.fill_column_indicator > 0 {
            let fci_col = params.fill_column_indicator;
            let _fci_char = params.fill_column_indicator_char;
            let _fci_fg = if params.fill_column_indicator_fg != 0 {
                Color::from_pixel(params.fill_column_indicator_fg)
            } else {
                default_fg
            };

            // Draw indicator character at the fill column on each row
            if (fci_col as usize) < cols {
                let indicator_x = content_x + fci_col as f32 * char_w;
                let total_rows = row.min(max_rows);
                for r in 0..total_rows {
                    let _gy = row_y_positions
                        .get(r)
                        .copied()
                        .unwrap_or(text_y + r as f32 * char_h);
                    if indicator_x < content_x + avail_width {}
                }
            }
        }

        if params.point >= window_start && (params.point <= charpos || point_is_visible_eob) {
            if let Some(cursor) = cursor_info {
                let row_metric = row_metrics_for_cursor(
                    output_emitter.row_metrics(),
                    text_matrix_row_base + cursor.matrix_row,
                    text_matrix_row_base + row,
                    y,
                    row_max_height,
                    row_max_ascent,
                );
                output_emitter.set_logical_cursor(WindowCursorPos {
                    x: (cursor.x - text_area_left).round() as i64,
                    y: (row_metric.pixel_y - window_top).round() as i64,
                    row: window_text_row(cursor.matrix_row),
                    col: cursor.col as i64,
                });
                if let Some(style) = cursor_style_for_window(params) {
                    let computed_slot_width = if let Some(slot_width) = cursor.slot_width {
                        slot_width.max(1.0)
                    } else {
                        cursor_width_for_style(
                            style,
                            text,
                            cursor.byte_idx,
                            cursor.col as i32,
                            params,
                            cursor.face_w,
                        )
                        .max(1.0)
                    };
                    let fallback_cursor_w = computed_slot_width.max(1.0);
                    let actual_cursor_w = if cursor.slot_width.is_some() {
                        computed_slot_width
                    } else if let Some(face) = self.matrix_builder.faces().get(&cursor.face_id) {
                        unsafe {
                            cursor_point_advance(
                                text,
                                cursor.byte_idx,
                                cursor.col as i32,
                                params,
                                cursor.face_w,
                                cursor.face_space_w,
                                char_w,
                                face.font_size.max(1.0).round() as i32,
                                &face.font_family,
                                face.font_weight,
                                face.is_italic(),
                                &mut self.ascii_width_cache,
                                &mut self.font_metrics,
                            )
                            .unwrap_or(fallback_cursor_w)
                        }
                        .max(1.0)
                    } else {
                        fallback_cursor_w
                    };
                    let cursor_w = if cursor.stretch_like
                        && !params.x_stretch_cursor
                        && !matches!(style, CursorStyle::Bar(_))
                    {
                        char_w.max(1.0)
                    } else {
                        actual_cursor_w
                    };
                    let (cursor_y, cursor_h, cursor_ascent) = resolve_cursor_vertical_metrics(
                        cursor.y,
                        cursor.face_h,
                        cursor.face_ascent,
                        row_metric.height,
                        row_metric.ascent,
                        char_h,
                        point_is_visible_eob,
                    );
                    let resolved_cursor = ResolvedCursorGeometry {
                        x: cursor.x,
                        y: cursor_y,
                        width: cursor_w,
                        height: cursor_h,
                        ascent: cursor_ascent,
                        row: text_matrix_row_base + cursor.matrix_row,
                        col: cursor.col,
                        style,
                        color: Color::from_pixel(params.cursor_color),
                        cursor_fg: cursor.bg,
                    };
                    if resolved_cursor.y >= text_y
                        && resolved_cursor.y + resolved_cursor.height <= text_y + text_height
                    {
                        self.matrix_builder.push_cursor(
                            params.window_id as i32,
                            DisplaySlotId {
                                window_id: params.window_id,
                                row: resolved_cursor.row as u32,
                                col: resolved_cursor.col as u16,
                            },
                            resolved_cursor.x,
                            resolved_cursor.y,
                            resolved_cursor.width,
                            resolved_cursor.height,
                            resolved_cursor.style,
                            resolved_cursor.color,
                        );
                        self.matrix_builder.set_cursor_at_row(
                            resolved_cursor.row,
                            resolved_cursor.col as u16,
                            resolved_cursor.style,
                        );
                        output_emitter.set_phys_cursor(WindowCursorSnapshot {
                            kind: window_cursor_kind(resolved_cursor.style),
                            x: (resolved_cursor.x - text_area_left).round() as i64,
                            y: (resolved_cursor.y - window_top).round() as i64,
                            width: resolved_cursor.width.round() as i64,
                            height: resolved_cursor.height.round() as i64,
                            ascent: resolved_cursor.ascent.round() as i64,
                            row: resolved_cursor.row as i64,
                            col: resolved_cursor.col as i64,
                        });
                        if params.selected {
                            self.matrix_builder.set_phys_cursor(PhysCursor {
                                window_id: params.window_id as i32,
                                charpos: params.point.max(0) as usize,
                                row: resolved_cursor.row,
                                col: resolved_cursor.col as u16,
                                slot_id: DisplaySlotId {
                                    window_id: params.window_id,
                                    row: resolved_cursor.row as u32,
                                    col: resolved_cursor.col as u16,
                                },
                                x: resolved_cursor.x,
                                y: resolved_cursor.y,
                                width: resolved_cursor.width,
                                height: resolved_cursor.height,
                                ascent: resolved_cursor.ascent,
                                style: resolved_cursor.style,
                                color: resolved_cursor.color,
                                cursor_fg: resolved_cursor.cursor_fg,
                            });
                        }

                        if point_is_visible_eob {
                            tracing::debug!(
                                "layout_window_rust: emitting EOB cursor at x={:.1} y={:.1} w={:.1} h={:.1}",
                                resolved_cursor.x,
                                resolved_cursor.y,
                                resolved_cursor.width,
                                resolved_cursor.height
                            );
                        }
                    }
                }
            } else {
                tracing::debug!(
                    "layout_window_rust: no explicit cursor capture for point={} window_start={} charpos_end={}",
                    params.point,
                    window_start,
                    charpos
                );
            }
        }

        let has_pending_row_output = output_emitter.current_row_has_output();
        if row < max_rows && (charpos > hit_row_charpos_start || has_pending_row_output) {
            let row_y_start = row_y_positions
                .get(row)
                .copied()
                .unwrap_or(text_y + row as f32 * char_h + row_extra_y);
            hit_rows.push(HitRow {
                y_start: row_y_start,
                y_end: row_y_start + row_max_height,
                charpos_start: hit_row_charpos_start,
                charpos_end: charpos,
            });
            finish_text_row(
                &mut self.matrix_builder,
                &mut output_emitter,
                row_y_start,
                row_max_height,
                row_max_ascent,
            );
        }

        // GNU redisplay keeps iterating until point visibility converges or no
        // further progress can be made.  Advance by actual rendered row spans
        // from this pass rather than rescanning by logical newlines, since
        // wrapped and variable-height lines are exactly where newline-based
        // retry selection goes wrong.
        let visible_end_lisp = output_emitter
            .rows()
            .iter()
            .rev()
            .find_map(|row| row.end_buffer_pos);
        let point_lisp = (params.point as usize).saturating_add(1);
        let visible_end_lisp = if point_is_visible_eob {
            Some(visible_end_lisp.unwrap_or(point_lisp).max(point_lisp))
        } else {
            visible_end_lisp
        };
        let visible_progress = visible_end_lisp
            .map(|end_lisp| end_lisp as i64)
            .unwrap_or(charpos);
        let point_beyond_visible_span = visible_end_lisp
            .map(|end_lisp| point_lisp > end_lisp)
            .unwrap_or(params.point > charpos);

        let scroll_down_ws = if point_beyond_visible_span
            && visible_progress > window_start
            && !params.is_minibuffer
        {
            let new_ws = next_window_start_from_visible_rows(output_emitter.rows(), window_start)
                .map(|new_ws| new_ws.min(params.point.max(params.buffer_begv)));
            tracing::debug!(
                "layout_window_rust: point={} beyond visible_end={:?} (charpos_end={}), visible_rows={}, new_window_start={:?}",
                point_lisp,
                visible_end_lisp,
                charpos,
                output_emitter.rows().len(),
                new_ws
            );
            new_ws
        } else {
            None
        };
        let text_area_top = (text_y - window_top).round() as i64;
        let text_area_bottom = (text_y + text_height - window_top).round() as i64;
        let point_row_ws = next_window_start_for_partially_visible_point_row(
            output_emitter.rows(),
            params.point,
            text_area_top,
            text_area_bottom,
            window_start,
        );
        if point_row_ws.is_some() {
            tracing::debug!(
                "layout_window_rust: point={} row partially visible within {}..{}, new_window_start={:?}",
                params.point,
                text_area_top,
                text_area_bottom,
                point_row_ws
            );
        }
        let point_line_ws = next_window_start_for_point_line_continuation(
            output_emitter.rows(),
            params.point,
            window_start,
            &buf_access,
            params.buffer_size,
        );
        if point_line_ws.is_some() {
            tracing::debug!(
                "layout_window_rust: point={} line continues below final visible row, new_window_start={:?}",
                params.point,
                point_line_ws
            );
        }
        let retry_window_start = scroll_down_ws.or(point_row_ws).or(point_line_ws);

        if let Some(new_window_start) = retry_window_start
            && remaining_visibility_retries > 0
            && new_window_start > window_start
        {
            tracing::debug!(
                "layout_window_rust: retrying window {} with adjusted window_start {} -> {} (remaining={})",
                params.window_id,
                window_start,
                new_window_start,
                remaining_visibility_retries
            );
            self.matrix_builder
                .truncate_transition_hints(transition_hints_len_before);
            self.matrix_builder
                .truncate_effect_hints(effect_hints_len_before);

            let mut retry_params = params.clone();
            retry_params.window_start = new_window_start;
            retry_params.window_end = 0;
            // Persist the counter BEFORE recursing so the retry
            // call loads the parent's bumped value as its base.
            // The retry will write back its final counter; the
            // unconditional `return` below skips the bottom-of-
            // function writeback path.
            self.frame_face_id_counter = current_face_id;
            self.layout_window_rust(
                evaluator,
                frame_id,
                &retry_params,
                _frame_params,
                face_resolver,
                reserve_right_border_col,
                remaining_visibility_retries.saturating_sub(1),
            );
            return;
        }

        let window_start_lisp = (window_start as usize).saturating_add(1);
        // Use the last row that actually has a buffer position, not
        // just the last row.  Empty trailing rows (e.g. the blank
        // line after a buffer ending with `\n`) have
        // end_buffer_pos = None.  Using `.last()` hit that None and
        // fell back to 1, making the %p mode-line construct show
        // "Top" instead of "All" for short buffers.
        let window_end_lisp = output_emitter
            .rows()
            .iter()
            .rev()
            .find_map(|row| row.end_buffer_pos)
            .map(|pos| pos.saturating_add(1))
            .unwrap_or(1);
        let window_end_byte = text_start_byte.saturating_add(byte_idx);
        let window_end_vpos = output_emitter
            .rows()
            .last()
            .map(|row| row.row.max(0) as usize)
            .unwrap_or(0);

        if let Some(info) = self.matrix_builder.window_infos_last_mut()
            && info.window_id == params.window_id
        {
            info.window_start = window_start_lisp as i64;
            info.window_end = window_end_lisp as i64;
        }

        tracing::debug!(
            "  layout_window_rust: window_start={} window_end={}",
            window_start_lisp,
            window_end_lisp
        );

        // GNU status-line percent specs read the live window state from the
        // just-produced redisplay. Publish the authoritative window geometry
        // before evaluating mode-line/header-line/tab-line forms so `%p/%P/%o`
        // reflect the frame we are about to render, not stale state from the
        // previous redisplay.
        {
            let win_id = neovm_core::window::WindowId(params.window_id as u64);

            if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
                let update_window = |w: &mut neovm_core::window::Window| {
                    if let neovm_core::window::Window::Leaf {
                        window_start: ws, ..
                    } = w
                    {
                        *ws = window_start_lisp;
                        w.set_window_end_from_positions(
                            buffer_z_char,
                            buffer_z_byte,
                            window_end_lisp,
                            window_end_byte,
                            window_end_vpos,
                        );
                    }
                };

                if let Some(window) = frame.root_window.find_mut(win_id) {
                    update_window(window);
                } else if let Some(ref mut mini) = frame.minibuffer_leaf
                    && mini.id() == win_id
                {
                    update_window(mini);
                }
            }
        }

        // --- GlyphMatrix builder: finalize text rows, then emit chrome rows
        // into their real glyph-matrix slots before closing the window. ---
        for metric in output_emitter.row_metrics() {
            self.matrix_builder.set_row_metrics(
                metric.row,
                metric.pixel_y,
                metric.height,
                metric.ascent,
            );
        }
        self.matrix_builder.end_row();
        for (row_idx, truncated) in row_truncated.iter().copied().enumerate() {
            if truncated {
                let target_col = if reserve_right_border_col {
                    matrix_cols.saturating_sub(2)
                } else {
                    matrix_cols.saturating_sub(1)
                };
                self.matrix_builder
                    .overwrite_current_window_row_glyph_at_col(row_idx, target_col, '$', 0);
            }
        }

        let mut status_line_symbol_values = std::collections::HashMap::new();
        if let Some(buffer) = evaluator
            .buffer_manager()
            .get(neovm_core::buffer::BufferId(params.buffer_id))
        {
            if let Some(value) = buffer.buffer_local_value("header-line-indent-width") {
                status_line_symbol_values.insert("header-line-indent-width".to_string(), value);
            }
        }

        // Tab-line: evaluate format-mode-line with tab-line-format
        if params.tab_line_height > 0.0 {
            // Tab-line is above header-line (at the very top of the window)
            let tl_y = params.bounds.y;
            let tl_row = 0i64;
            let tl_face = tab_line_face
                .as_ref()
                .expect("tab-line face should exist when tab-line height is positive");

            let tab_line_target_cols = ((params.bounds.width / char_w.max(1.0)).round().max(1.0)
                as usize)
                .saturating_sub(usize::from(reserve_right_border_col))
                .max(1);
            let tab_text = eval_status_line_format_value(
                evaluator,
                "tab-line-format",
                params.window_id,
                params.buffer_id,
                tab_line_target_cols,
            )
            .unwrap_or_else(|| Value::string(""));

            let mut builder = std::mem::replace(
                &mut self.matrix_builder,
                crate::matrix_builder::GlyphMatrixBuilder::new(),
            );
            output_emitter.begin_chrome_row(evaluator, tl_row, tl_y);
            let mut advance_output =
                |progress: crate::display_status_line::StatusLineOutputProgress| {
                    output_emitter.move_chrome_output_to(evaluator, tl_row, progress);
                };
            let tab_output = self.render_rust_status_line_value_via_backend(
                params.bounds.x,
                tl_y,
                params.bounds.width,
                tab_line_height,
                0,
                params.window_id,
                char_w,
                font_ascent,
                &mut current_face_id,
                tl_face,
                tab_text,
                face_resolver,
                status_line_symbol_values.clone(),
                StatusLineKind::TabLine,
                Some(&mut builder),
                Some(&mut advance_output),
            );
            self.matrix_builder = builder;
            if let Some(progress) = tab_output {
                output_emitter.push_chrome_row_progress(progress);
            }
        }

        // Header-line: evaluate format-mode-line with header-line-format.
        // Emit top chrome in visual order so live output progression does not
        // regress from later body rows back to row 0.
        if params.header_line_height > 0.0 {
            let hl_y = params.bounds.y + tab_line_height;
            let hl_row = i64::from(tab_line_height > 0.0);
            let hl_face = header_line_face
                .as_ref()
                .expect("header-line face should exist when header-line height is positive");

            let header_line_target_cols = ((params.bounds.width / char_w.max(1.0)).round().max(1.0)
                as usize)
                .saturating_sub(usize::from(reserve_right_border_col))
                .max(1);
            let header_text = eval_status_line_format_value(
                evaluator,
                "header-line-format",
                params.window_id,
                params.buffer_id,
                header_line_target_cols,
            )
            .unwrap_or_else(|| Value::string(""));

            let mut builder = std::mem::replace(
                &mut self.matrix_builder,
                crate::matrix_builder::GlyphMatrixBuilder::new(),
            );
            output_emitter.begin_chrome_row(evaluator, hl_row, hl_y);
            let mut advance_output =
                |progress: crate::display_status_line::StatusLineOutputProgress| {
                    output_emitter.move_chrome_output_to(evaluator, hl_row, progress);
                };
            let header_output = self.render_rust_status_line_value_via_backend(
                params.bounds.x,
                hl_y,
                params.bounds.width,
                header_line_height,
                usize::from(tab_line_height > 0.0),
                params.window_id,
                char_w,
                font_ascent,
                &mut current_face_id,
                hl_face,
                header_text,
                face_resolver,
                status_line_symbol_values.clone(),
                StatusLineKind::HeaderLine,
                Some(&mut builder),
                Some(&mut advance_output),
            );
            self.matrix_builder = builder;
            if let Some(progress) = header_output {
                output_emitter.push_chrome_row_progress(progress);
            }
        }

        // Mode-line: evaluate format-mode-line or fall back to buffer name.
        // Commit it last so live output progression ends on the visually last
        // row in the window matrix.
        if params.mode_line_height > 0.0 {
            let ml_y = params.bounds.y + params.bounds.height - mode_line_height;
            let ml_row = mode_line_matrix_row as i64;
            let ml_face = mode_line_face
                .as_ref()
                .expect("mode-line face should exist when mode-line height is positive");

            // GNU `display_mode_line` walks the format in
            // `MODE_LINE_DISPLAY` mode, so `%-` fills the remaining
            // row width with dashes. Compute the row width in
            // character cells and pass it through.
            let mode_line_target_cols = ((params.bounds.width / char_w.max(1.0)).round().max(1.0)
                as usize)
                .saturating_sub(usize::from(reserve_right_border_col))
                .max(1);
            let mode_text = {
                let result = eval_status_line_format_value(
                    evaluator,
                    "mode-line-format",
                    params.window_id,
                    params.buffer_id,
                    mode_line_target_cols,
                )
                .unwrap_or_else(|| Value::string(format!(" {} ", buffer_name)));
                tracing::debug!(
                    "mode-line eval result: {:?} (len={})",
                    result
                        .as_utf8_str()
                        .map(|s| &s[..s.len().min(120)])
                        .unwrap_or(""),
                    result.as_utf8_str().map(str::len).unwrap_or(0)
                );
                result
            };

            let mut builder = std::mem::replace(
                &mut self.matrix_builder,
                crate::matrix_builder::GlyphMatrixBuilder::new(),
            );
            output_emitter.begin_chrome_row(evaluator, ml_row, ml_y);
            let mut advance_output =
                |progress: crate::display_status_line::StatusLineOutputProgress| {
                    output_emitter.move_chrome_output_to(evaluator, ml_row, progress);
                };
            let mode_output = self.render_rust_status_line_value_via_backend(
                params.bounds.x,
                ml_y,
                params.bounds.width,
                mode_line_height,
                mode_line_matrix_row,
                params.window_id,
                char_w,
                font_ascent,
                &mut current_face_id,
                ml_face,
                mode_text,
                face_resolver,
                status_line_symbol_values.clone(),
                StatusLineKind::ModeLine,
                Some(&mut builder),
                Some(&mut advance_output),
            );
            self.matrix_builder = builder;
            if let Some(progress) = mode_output {
                output_emitter.push_chrome_row_progress(progress);
            }
        }

        self.matrix_builder.end_window();

        // Store hit-test data for this window
        self.hit_data.push(WindowHitData {
            window_id: params.window_id,
            content_x,
            char_w,
            rows: hit_rows,
        });

        let snapshot = output_emitter.finish_snapshot(
            evaluator,
            (text_area_left - params.bounds.x).round() as i64,
            mode_line_height.round() as i64,
            header_line_height.round() as i64,
            tab_line_height.round() as i64,
        );
        self.display_snapshots.push(snapshot);

        // Persist the face-id counter back to the frame-wide
        // slot so the NEXT window in this frame starts allocating
        // face_ids past the ones we just used. Without this
        // write-back every sibling window would reuse ids 1..N
        // and overwrite this window's entries in the shared
        // `matrix_builder.faces` HashMap — the original
        // manifestation of the "C-x 2 paints both mode lines
        // with mode-line-inactive colors" bug. Mirrors GNU's
        // single `face_cache->used` counter at
        // `src/xfaces.c::init_frame_faces`.
        self.frame_face_id_counter = current_face_id;
    }

    /// Trigger fontification for a buffer region via the Rust Context.
    ///
    /// Calls `(run-hook-with-args 'fontification-functions START)` if
    /// `fontification-functions` is bound and non-nil.  This is the same
    /// mechanism Emacs uses in `handle_fontified_prop` to ensure text
    /// properties (e.g. `font-lock-face`) are set before display.
    ///
    /// Errors are non-fatal: layout continues without fontification if
    /// the hook signals or is not configured.
    fn ensure_fontified_rust(
        evaluator: &mut neovm_core::emacs_core::Context,
        _buf_id: neovm_core::buffer::BufferId,
        from: i64,
        _to: i64,
    ) {
        // Check if fontification-functions is bound and non-nil by evaluating
        // the symbol.
        let has_fontification = match evaluator.eval_str("fontification-functions") {
            Ok(val) => !val.is_nil(),
            Err(_) => false,
        };

        if !has_fontification {
            return; // No fontification configured
        }

        // Call (run-hook-with-args 'fontification-functions FROM).
        // This is what Emacs does in handle_fontified_prop to trigger
        // jit-lock-fontify-now (via jit-lock-function on the hook).
        // The hook functions receive the buffer position and fontify the
        // surrounding region, setting font-lock-face text properties.
        let expr_str = format!(
            "(run-hook-with-args 'fontification-functions {})",
            from.saturating_add(1)
        );

        if let Err(e) = evaluator.eval_str(&expr_str) {
            tracing::debug!("ensure_fontified_rust: fontification error: {:?}", e);
        }
    }
}

impl LayoutEngine {
    /// Build the minibuffer echo row through the shared DisplayBackend path.
    ///
    /// This returns the realized face plus the row's text glyphs so the
    /// caller can install them into the currently open minibuffer window.
    pub(crate) fn render_minibuffer_echo_via_backend(
        &mut self,
        text_width: f32,
        char_w: f32,
        ascent: f32,
        row_height: f32,
        default_resolved: &crate::neovm_bridge::ResolvedFace,
        echo_message: String,
        max_rows: usize,
    ) -> (
        neomacs_display_protocol::face::Face,
        Vec<Vec<neomacs_display_protocol::glyph_matrix::Glyph>>,
    ) {
        use crate::display_backend::{
            DisplayBackend, GuiDisplayBackend, TtyDisplayBackend, display_text_plain_via_backend,
        };
        use neomacs_display_protocol::glyph_matrix::GlyphRow;

        // Reuse the shared face realization so GUI and TTY echo text use the
        // same measured face data as the rest of redisplay.
        let sl_face =
            self.realize_status_line_face(0, default_resolved, char_w, ascent, row_height);
        let rendered_face = sl_face.render_face();
        let char_width = self.status_line_char_width(&sl_face, char_w);

        // GNU's echo area displays internal newlines as minibuffer row
        // breaks. Keep the shared plain-text helper single-row for status
        // line callers, and split echo text before walking each row.
        let mut rows = Vec::new();
        for line in echo_message
            .split(|ch| ch == '\n' || ch == '\r')
            .take(max_rows.max(1))
        {
            let mut tty_backend = TtyDisplayBackend::new();
            let mut gui_backend = self.font_metrics.as_mut().map(GuiDisplayBackend::new);
            let backend: &mut dyn DisplayBackend = match gui_backend {
                Some(ref mut g) => g,
                None => &mut tty_backend,
            };
            display_text_plain_via_backend(backend, line, &rendered_face, char_width, text_width);

            let mut flush_row =
                GlyphRow::new(neomacs_display_protocol::frame_glyphs::GlyphRowRole::Minibuffer);
            flush_row.enabled = true;
            backend.finish_row(flush_row);
            let glyphs = backend
                .take_rows()
                .into_iter()
                .next()
                .map(|mut row| std::mem::take(&mut row.glyphs[1]))
                .unwrap_or_default();
            rows.push(glyphs);
        }
        if rows.is_empty() {
            rows.push(Vec::new());
        }
        (rendered_face, rows)
    }

    pub(crate) fn status_line_char_width(
        &mut self,
        face: &StatusLineFace,
        fallback_char_width: f32,
    ) -> f32 {
        if face.font_char_width > 0.0 {
            return face.font_char_width;
        }
        if let Some(ref mut svc) = self.font_metrics {
            let metrics = svc.font_metrics(
                &face.font_family,
                face.font_weight,
                face.italic,
                face.font_size,
            );
            return metrics.char_width;
        }
        fallback_char_width
    }

    pub(crate) fn status_line_font_metrics(
        &mut self,
        face: &StatusLineFace,
    ) -> crate::font_metrics::FontMetrics {
        // If the engine was started in TTY mode (no
        // `enable_cosmic_metrics()` call), `self.font_metrics` is
        // None and we return the face's cell-based fallback
        // metrics. GUI mode populated the service at startup.
        if let Some(ref mut svc) = self.font_metrics {
            return svc.font_metrics(
                &face.font_family,
                face.font_weight,
                face.italic,
                face.font_size,
            );
        }

        crate::font_metrics::FontMetrics {
            ascent: face.font_ascent.max(1.0),
            descent: face.font_descent.max(0) as f32,
            line_height: (face.font_ascent + face.font_descent as f32).max(1.0),
            char_width: face.font_char_width.max(1.0),
        }
    }

    /// Render the frame-level tab-bar from GNU Lisp keymap output on the Rust path.
    ///
    /// Build the frame-level tab-bar row and attach it to the published
    /// `FrameDisplayState` as frame chrome, not as a leaf-window row.
    ///
    /// GNU handles the tab bar outside ordinary leaf-window text rows:
    /// - GUI uses `frame->tab_bar_window`
    /// - TTY writes tab-bar rows directly into the frame matrix
    ///
    /// Neomacs keeps immutable snapshots, so this method records a
    /// frame-level `FrameChromeRow` that renderers can consume directly.
    fn render_frame_tab_bar_rust(
        &mut self,
        evaluator: &mut neovm_core::emacs_core::Context,
        frame_window_id: i64,
        face_resolver: &super::neovm_bridge::FaceResolver,
        frame_params: &FrameParams,
        tab_bar_height: f32,
    ) {
        use crate::display_backend::{
            DisplayBackend, GuiDisplayBackend, TtyDisplayBackend, display_text_plain_via_backend,
        };

        let Some(tab_bar) = build_tab_bar_display(evaluator, frame_window_id as u64) else {
            return;
        };

        let width = frame_params.width;
        let tab_bar_face = face_resolver.resolve_named_face("tab-bar");
        let face_id = self.frame_face_id_counter.max(1);
        self.frame_face_id_counter = face_id.saturating_add(1);

        let sl_face = self.realize_status_line_face(
            face_id,
            &tab_bar_face,
            frame_params.char_width,
            frame_params.char_height * 0.8,
            tab_bar_height,
        );
        let rendered_face = sl_face.render_face();
        self.matrix_builder
            .insert_face(face_id, rendered_face.clone());
        let char_width = self.status_line_char_width(&sl_face, frame_params.char_width);

        // Dispatch between GUI (cosmic-text) and TTY (cell-grid)
        // backends based on whether cosmic metrics are enabled on
        // this LayoutEngine.
        let mut tty_backend = TtyDisplayBackend::new();
        let mut gui_backend = self.font_metrics.as_mut().map(GuiDisplayBackend::new);
        let backend: &mut dyn DisplayBackend = match gui_backend {
            Some(ref mut g) => g,
            None => &mut tty_backend,
        };
        display_text_plain_via_backend(backend, &tab_bar.text, &rendered_face, char_width, width);
        let glyphs: Vec<_> = backend.pending_glyphs().to_vec();
        if glyphs.is_empty() {
            return;
        }

        let mut row = neomacs_display_protocol::glyph_matrix::GlyphRow::new(
            neomacs_display_protocol::frame_glyphs::GlyphRowRole::TabBar,
        );
        row.enabled = true;
        row.mode_line = true;
        row.pixel_y = 0.0;
        row.height_px = tab_bar_height.max(1.0);
        row.ascent_px = sl_face.font_ascent.max(0.0).min(row.height_px);
        row.glyphs[neomacs_display_protocol::glyph_matrix::GlyphArea::Text as usize] = glyphs;
        crate::matrix_builder::GlyphMatrixBuilder::normalize_external_row(&mut row);

        let row_index = if frame_params.char_height > 0.0 {
            (frame_params.menu_bar_height / frame_params.char_height)
                .round()
                .max(0.0) as u32
        } else {
            0
        };
        let tab_bar_y = frame_params.menu_bar_height;

        self.pending_frame_chrome_rows.push(
            neomacs_display_protocol::glyph_matrix::FrameChromeRow {
                row_index,
                pixel_bounds: Rect::new(0.0, tab_bar_y, width, tab_bar_height),
                row,
            },
        );
        self.pending_tab_bar = Some(neomacs_display_protocol::frame_glyphs::FrameTabBarState {
            items: tab_bar.items,
            y: tab_bar_y,
            height: tab_bar_height,
        });
    }
}

/// Get the advance width for a character in a specific face.
///
/// Standalone function to avoid borrow conflicts with `LayoutEngine::text_buf`.
///
/// Uses `FontMetricsService` (cosmic-text) for measurement, matching the render
/// thread's font resolution exactly.
unsafe fn char_advance(
    ascii_width_cache: &mut std::collections::HashMap<AsciiWidthCacheKey, [f32; 128]>,
    font_metrics_svc: &mut Option<FontMetricsService>,
    ch: char,
    char_cols: i32,
    char_w: f32,
    font_size: i32,
    face_char_w: f32,
    font_family: &str,
    font_weight: u16,
    font_italic: bool,
) -> f32 {
    #[inline]
    fn snap_advance_to_pixel_grid(advance: f32, min_advance: f32) -> f32 {
        let snapped_min = min_advance.round().max(1.0);
        if !advance.is_finite() || advance <= 0.0 {
            return snapped_min;
        }

        // GNU Emacs stores realized glyph widths and positions in integer
        // pixels. Snapping each advance before it enters layout keeps the
        // published window geometry (`posn-at-point`, cursor x, etc.) on the
        // same integer grid instead of accumulating fractional drift across a
        // row.
        advance.round().max(1.0)
    }

    // Use the face-specific character width when available (handles
    // faces with :height attribute that use a differently-sized font).
    let face_w = if face_char_w > 0.0 {
        face_char_w
    } else {
        char_w
    };
    let min_grid_advance = char_cols as f32 * face_w;

    // TTY mode: when no font metrics service exists (enable_cosmic_metrics not called),
    // use char-cell grid advance directly.  Don't auto-create pixel-based metrics.
    let svc = match font_metrics_svc.as_mut() {
        Some(svc) => svc,
        None => return snap_advance_to_pixel_grid(min_grid_advance, min_grid_advance),
    };
    let font_size_f = if font_size > 0 {
        font_size as f32
    } else {
        face_w.max(1.0)
    };
    let cp = ch as u32;
    if cp < 128 {
        let cache_key = AsciiWidthCacheKey::new(font_family, font_weight, font_italic, font_size);
        let widths = ascii_width_cache.entry(cache_key).or_insert_with(|| {
            let mut widths =
                svc.fill_ascii_widths(font_family, font_weight, font_italic, font_size_f);
            for w in &mut widths {
                *w = snap_advance_to_pixel_grid(*w, min_grid_advance);
            }
            widths
        });
        return widths[cp as usize];
    }

    let measured = svc.char_width(ch, font_family, font_weight, font_italic, font_size_f);
    snap_advance_to_pixel_grid(measured, min_grid_advance)
}

#[cfg(test)]
#[path = "engine_test.rs"]
mod tests;
