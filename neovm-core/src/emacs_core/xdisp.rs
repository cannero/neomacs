//! Display engine builtins for the Elisp interpreter.
//!
//! Implements display-related functions from Emacs `xdisp.c`:
//! - `format-mode-line` — format a mode line string
//! - `invisible-p` — check if a position or property is invisible
//! - `line-pixel-height` — get line height in pixels
//! - `window-text-pixel-size` — calculate text pixel dimensions
//! - `pos-visible-in-window-p` — check if position is visible
//! - `move-point-visually` — move point in visual order
//! - `lookup-image-map` — lookup image map coordinates
//! - `current-bidi-paragraph-direction` — get bidi paragraph direction
//! - `move-to-window-line` — move to a specific window line
//! - `tool-bar-height` — get tool bar height
//! - `tab-bar-height` — get tab bar height
//! - `line-number-display-width` — get line number display width
//! - `long-line-optimizations-p` — check if long-line optimizations are enabled

use super::chartable::{make_char_table_value, make_char_table_with_extra_slots};
use super::error::{EvalResult, Flow, signal};
use super::value::*;
use crate::window::{FrameId, WindowId};

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_args_range(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_integer_or_marker(arg: &Value) -> Result<(), Flow> {
    match arg {
        Value::Int(_) | Value::Char(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn expect_fixnum_arg(name: &str, arg: &Value) -> Result<(), Flow> {
    match arg {
        Value::Int(_) | Value::Char(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol(name), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// (format-mode-line &optional FORMAT FACE WINDOW BUFFER) -> string
///
/// Batch-compatible behavior: accepts 1..4 args and returns an empty string.
pub(crate) fn builtin_format_mode_line(args: Vec<Value>) -> EvalResult {
    expect_args_range("format-mode-line", &args, 1, 4)?;
    if let Some(window) = args.get(2) {
        if !window.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("windowp"), *window],
            ));
        }
    }
    if let Some(buffer) = args.get(3) {
        if !buffer.is_nil() && !matches!(buffer, Value::Buffer(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("bufferp"), *buffer],
            ));
        }
    }
    Ok(Value::string(""))
}

/// `(format-mode-line &optional FORMAT FACE WINDOW BUFFER)` evaluator-backed variant.
///
/// Handles string formats with %-construct expansion and list-based format
/// specs by recursively processing elements (symbols, strings, :eval, :propertize,
/// and conditional cons cells).
pub(crate) fn builtin_format_mode_line_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("format-mode-line", &args, 1, 4)?;
    validate_optional_window_designator(eval, args.get(2), "windowp")?;
    validate_optional_buffer_designator(eval, args.get(3))?;

    let format_val = args[0];
    let mut result = String::new();
    format_mode_line_recursive(eval, &format_val, &mut result, 0);
    Ok(Value::string(&result))
}

/// Recursively process a mode-line format spec, appending output to `result`.
///
/// FORMAT can be:
/// - A string: expand %-constructs (%b, %f, %*, %l, %c, %p, etc.)
/// - A symbol: look up its value, recursively format
/// - A list: process each element in sequence
/// - `(:eval FORM)`: evaluate FORM, use result as format
/// - `(:propertize ELT PROPS...)`: process ELT (ignore text properties)
/// - A cons `(SYMBOL . REST)`: if SYMBOL's value is non-nil, process REST
fn format_mode_line_recursive(
    eval: &mut super::eval::Evaluator,
    format: &Value,
    result: &mut String,
    depth: usize,
) {
    if depth > 20 {
        return; // Guard against infinite recursion
    }

    match format {
        Value::Nil => {}

        Value::Str(_) => {
            if let Some(fmt_str) = format.as_str() {
                expand_mode_line_percent(eval, fmt_str, result);
            }
        }

        Value::Int(n) => {
            // Integer in mode-line-format: if positive, specifies minimum
            // field width; if negative, max width with truncation.
            // The actual padding/truncation is applied to subsequent elements
            // which we don't track here, so just ignore the width spec.
            let _ = n;
        }

        _ if format.is_symbol() => {
            if let Some(name) = format.as_symbol_name() {
                // Skip well-known problematic symbols
                if name == "mode-line-front-space" || name == "mode-line-end-spaces" {
                    result.push(' ');
                    return;
                }
                // Look up the symbol's value and recurse
                if let Ok(val) = eval.eval_symbol(name) {
                    if !val.is_nil() {
                        format_mode_line_recursive(eval, &val, result, depth + 1);
                    }
                }
            }
        }

        _ if format.is_cons() => {
            let car = format.cons_car();
            let cdr = format.cons_cdr();

            // (:eval FORM)
            if car.is_symbol_named(":eval") {
                if cdr.is_cons() {
                    let form_val = cdr.cons_car();
                    if let Ok(val) = eval.eval_value(&form_val) {
                        format_mode_line_recursive(eval, &val, result, depth + 1);
                    }
                }
                return;
            }

            // (:propertize ELT PROPS...) — process ELT, ignore properties
            if car.is_symbol_named(":propertize") {
                if cdr.is_cons() {
                    let elt = cdr.cons_car();
                    format_mode_line_recursive(eval, &elt, result, depth + 1);
                }
                return;
            }

            // Check if car is a symbol — conditional semantics:
            // (SYMBOL . REST) where if SYMBOL's value is non-nil, process REST
            if car.is_symbol() && !car.is_symbol_named("t") {
                if let Some(sym_name) = car.as_symbol_name() {
                    if let Ok(val) = eval.eval_symbol(sym_name) {
                        if !val.is_nil() {
                            format_mode_line_recursive(eval, &cdr, result, depth + 1);
                        }
                    }
                }
                return;
            }

            // Regular list: process each element
            if let Some(elements) = list_to_vec(format) {
                for elem in &elements {
                    format_mode_line_recursive(eval, elem, result, depth + 1);
                }
            }
        }

        _ => {
            // Unknown format type — try to get a string representation
            if let Some(s) = format.as_str() {
                result.push_str(s);
            }
        }
    }
}

/// Expand %-constructs in a mode-line format string.
fn expand_mode_line_percent(eval: &super::eval::Evaluator, fmt_str: &str, result: &mut String) {
    let buf = eval.buffer_manager().current_buffer();
    let buf_name = buf.map(|b| b.name.as_str()).unwrap_or("*scratch*");
    let file_name = buf.and_then(|b| b.file_name.as_deref()).unwrap_or("");
    let modified = buf.map(|b| b.is_modified()).unwrap_or(false);

    let (line_num, col_num) = if let Some(b) = buf {
        let pt = b.pt;
        let text = b.text.to_string();
        let before = &text[..pt.min(text.len())];
        let line = before.chars().filter(|&c| c == '\n').count() + 1;
        let col = before.rfind('\n').map(|nl| pt - nl - 1).unwrap_or(pt);
        (line, col)
    } else {
        (1, 0)
    };

    let mut chars = fmt_str.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            // Skip optional field width digits (e.g. %12b, %-3c)
            if chars.peek() == Some(&'-') {
                chars.next();
            }
            while chars.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                chars.next();
            }
            match chars.next() {
                Some('b') => result.push_str(buf_name),
                Some('f') => result.push_str(file_name),
                Some('F') => result.push_str("Neomacs"),
                Some('*') => result.push(if modified { '*' } else { '-' }),
                Some('+') => result.push(if modified { '+' } else { '-' }),
                Some('-') => result.push('-'),
                Some('%') => result.push('%'),
                Some('n') => {} // Narrow indicator
                Some('l') => result.push_str(&line_num.to_string()),
                Some('c') => result.push_str(&col_num.to_string()),
                Some('p') | Some('P') => {
                    if let Some(b) = buf {
                        let total = b.text.len();
                        if total == 0 {
                            result.push_str("All");
                        } else {
                            let pct = (b.pt * 100) / total;
                            if pct == 0 {
                                result.push_str("Top");
                            } else if pct >= 99 {
                                result.push_str("Bot");
                            } else {
                                result.push_str(&format!("{}%", pct));
                            }
                        }
                    }
                }
                Some('z') => result.push_str("U"), // Coding system mnemonic (simplified)
                Some('@') => result.push('-'),     // Default input method indicator
                Some('Z') => result.push_str("U"), // Like %z but includes eol type
                Some('[') | Some(']') => {}        // Recursive edit depth brackets
                Some('e') => {}                    // Error message area
                Some(' ') => result.push(' '),
                Some(c) => {
                    result.push('%');
                    result.push(c);
                }
                None => result.push('%'),
            }
        } else {
            result.push(ch);
        }
    }
}

/// (invisible-p POS-OR-PROP) -> boolean
///
/// Batch semantics mirror current oracle behavior:
/// - numeric positions > 0 are visible (nil),
/// - position 0 is out-of-range,
/// - negative numeric positions are invisible (t),
/// - nil is visible (nil),
/// - all other property values are treated as invisible (t).
pub(crate) fn builtin_invisible_p(args: Vec<Value>) -> EvalResult {
    expect_args("invisible-p", &args, 1)?;
    match &args[0] {
        Value::Int(v) => {
            if *v == 0 {
                Err(signal("args-out-of-range", vec![Value::Int(*v)]))
            } else if *v < 0 {
                Ok(Value::symbol("t"))
            } else {
                Ok(Value::Nil)
            }
        }
        Value::Char(ch) => {
            if *ch == '\0' {
                Err(signal("args-out-of-range", vec![Value::Char(*ch)]))
            } else {
                Ok(Value::Nil)
            }
        }
        Value::Nil => Ok(Value::Nil),
        _ => Ok(Value::symbol("t")),
    }
}

/// (line-pixel-height) -> integer
///
/// Batch-compatible behavior returns 1.
pub(crate) fn builtin_line_pixel_height(args: Vec<Value>) -> EvalResult {
    expect_args("line-pixel-height", &args, 0)?;
    Ok(Value::Int(1))
}

/// (window-text-pixel-size &optional WINDOW FROM TO X-LIMIT Y-LIMIT MODE) -> (WIDTH . HEIGHT)
///
/// Batch-compatible behavior returns `(0 . 0)` and enforces argument
/// validation for WINDOW / FROM / TO.
pub(crate) fn builtin_window_text_pixel_size(args: Vec<Value>) -> EvalResult {
    expect_args_range("window-text-pixel-size", &args, 0, 7)?;

    if let Some(window) = args.first() {
        if !window.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *window],
            ));
        }
    }
    if let Some(from) = args.get(1) {
        if !from.is_nil() {
            expect_integer_or_marker(from)?;
        }
    }
    if let Some(to) = args.get(2) {
        if !to.is_nil() {
            expect_integer_or_marker(to)?;
        }
    }

    Ok(Value::cons(Value::Int(0), Value::Int(0)))
}

/// `(window-text-pixel-size &optional WINDOW FROM TO X-LIMIT Y-LIMIT MODE)` evaluator-backed variant.
///
/// Batch mode returns `(0 . 0)` and validates optional WINDOW / FROM / TO
/// designators against evaluator state.
pub(crate) fn builtin_window_text_pixel_size_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("window-text-pixel-size", &args, 0, 7)?;
    validate_optional_window_designator(eval, args.first(), "window-live-p")?;
    if let Some(from) = args.get(1) {
        if !from.is_nil() {
            expect_integer_or_marker(from)?;
        }
    }
    if let Some(to) = args.get(2) {
        if !to.is_nil() {
            expect_integer_or_marker(to)?;
        }
    }
    Ok(Value::cons(Value::Int(0), Value::Int(0)))
}

/// (pos-visible-in-window-p &optional POS WINDOW PARTIALLY) -> boolean
///
/// Batch-compatible behavior: no window visibility is reported, so this
/// returns nil.
pub(crate) fn builtin_pos_visible_in_window_p(args: Vec<Value>) -> EvalResult {
    expect_args_range("pos-visible-in-window-p", &args, 0, 3)?;
    if let Some(window) = args.get(1) {
        if !window.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *window],
            ));
        }
    }
    // POS can be nil (point), t (end of buffer), or an integer/marker.
    if let Some(pos) = args.first() {
        if !pos.is_nil() && !matches!(pos, Value::True) && !pos.is_symbol_named("t") {
            expect_integer_or_marker(pos)?;
        }
    }
    Ok(Value::Nil)
}

/// `(pos-visible-in-window-p &optional POS WINDOW PARTIALLY)` evaluator-backed variant.
///
/// Mirror GNU Emacs: return t if POS is visible in WINDOW, nil otherwise.
/// Checks if position is between window-start and an estimated window-end.
pub(crate) fn builtin_pos_visible_in_window_p_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("pos-visible-in-window-p", &args, 0, 3)?;
    validate_optional_window_designator(eval, args.get(1), "window-live-p")?;

    // Extract buffer data up-front so we can release the immutable borrow on
    // `eval` before calling window helpers that need `&mut eval`.
    let (check_pos, text_bytes, zv) = {
        let Some(buf) = eval.buffers.current_buffer() else {
            return Ok(Value::Nil);
        };
        let check_pos = match args.first() {
            Some(Value::True) | Some(Value::Symbol(_))
                if args
                    .first()
                    .is_some_and(|v| matches!(v, Value::True) || v.is_symbol_named("t")) =>
            {
                buf.zv
            }
            Some(v) if !v.is_nil() => {
                expect_integer_or_marker(v)?;
                let n = v.as_int().unwrap_or(0);
                buf.text
                    .char_to_byte(((n - 1).max(0) as usize).min(buf.text.byte_to_char(buf.zv)))
            }
            _ => buf.pt,
        };
        let text_bytes = buf.text.to_string().into_bytes();
        (check_pos, text_bytes, buf.zv)
    };

    // Get window-start (char position → byte offset).
    let ws = super::window_cmds::builtin_window_start(eval, vec![])
        .ok()
        .and_then(|v| match v {
            Value::Int(n) => Some(n),
            _ => None,
        })
        .unwrap_or(1);
    // Convert char position to byte offset using the extracted text.
    let mut ws_byte = 0usize;
    let mut chars_seen = 0i64;
    for (i, &b) in text_bytes.iter().enumerate() {
        if chars_seen >= (ws - 1).max(0) {
            ws_byte = i;
            break;
        }
        // Count only leading bytes of UTF-8 sequences as char starts.
        if (b & 0xC0) != 0x80 {
            chars_seen += 1;
        }
    }
    if chars_seen < (ws - 1).max(0) {
        ws_byte = text_bytes.len();
    }

    // Get window height to estimate window-end.
    let wh = super::window_cmds::builtin_window_body_height(eval, vec![])
        .ok()
        .and_then(|v| match v {
            Value::Int(n) => Some(n),
            _ => None,
        })
        .unwrap_or(24);

    // Estimate window-end: scan wh lines from window-start.
    let mut we_byte = ws_byte;
    for _ in 0..wh {
        while we_byte < zv && we_byte < text_bytes.len() && text_bytes[we_byte] != b'\n' {
            we_byte += 1;
        }
        if we_byte < zv && we_byte < text_bytes.len() {
            we_byte += 1;
        }
    }

    if check_pos >= ws_byte && check_pos <= we_byte {
        Ok(Value::True)
    } else {
        Ok(Value::Nil)
    }
}

/// (move-point-visually DIRECTION) -> boolean
///
/// Batch semantics: direction is validated as a fixnum and the command
/// signals `args-out-of-range` in non-window contexts.
pub(crate) fn builtin_move_point_visually(args: Vec<Value>) -> EvalResult {
    expect_args("move-point-visually", &args, 1)?;
    match &args[0] {
        Value::Int(v) => Err(signal(
            "args-out-of-range",
            vec![Value::Int(*v), Value::Int(*v)],
        )),
        Value::Char(ch) => Err(signal(
            "args-out-of-range",
            vec![Value::Char(*ch), Value::Char(*ch)],
        )),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *other],
        )),
    }
}

/// (lookup-image-map MAP X Y) -> symbol or nil
///
/// Lookup an image map at coordinates. Stub implementation
/// returns nil while preserving arity validation.
pub(crate) fn builtin_lookup_image_map(args: Vec<Value>) -> EvalResult {
    expect_args("lookup-image-map", &args, 3)?;
    if !args[0].is_nil() {
        expect_fixnum_arg("fixnump", &args[1])?;
        expect_fixnum_arg("fixnump", &args[2])?;
    }
    Ok(Value::Nil)
}

/// (current-bidi-paragraph-direction &optional BUFFER) -> symbol
///
/// Get the bidi paragraph direction. Returns the symbol 'left-to-right.
pub(crate) fn builtin_current_bidi_paragraph_direction(args: Vec<Value>) -> EvalResult {
    expect_args_range("current-bidi-paragraph-direction", &args, 0, 1)?;
    if let Some(bufferish) = args.first() {
        if !bufferish.is_nil() && !matches!(bufferish, Value::Buffer(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("bufferp"), *bufferish],
            ));
        }
    }
    // Return 'left-to-right
    Ok(Value::symbol("left-to-right"))
}

/// `(bidi-resolved-levels &optional PARAGRAPH-DIRECTION)` -> nil
///
/// Batch compatibility: this currently returns nil and only enforces the
/// `fixnump` argument contract when PARAGRAPH-DIRECTION is non-nil.
pub(crate) fn builtin_bidi_resolved_levels(args: Vec<Value>) -> EvalResult {
    expect_args_range("bidi-resolved-levels", &args, 0, 1)?;
    if let Some(direction) = args.first() {
        if !direction.is_nil() {
            expect_fixnum_arg("fixnump", direction)?;
        }
    }
    Ok(Value::Nil)
}

/// `(bidi-find-overridden-directionality STRING/START END/START STRING/END
/// &optional DIRECTION)` -> nil
///
/// Batch compatibility mirrors oracle argument guards:
/// - when arg3 is a string, this path accepts arg1/arg2 without additional
///   type checks and returns nil;
/// - when arg3 is nil, arg1 and arg2 must satisfy `integer-or-marker-p`.
pub(crate) fn builtin_bidi_find_overridden_directionality(args: Vec<Value>) -> EvalResult {
    expect_args_range("bidi-find-overridden-directionality", &args, 3, 4)?;
    let third = &args[2];
    if third.is_nil() {
        expect_integer_or_marker(&args[0])?;
        expect_integer_or_marker(&args[1])?;
        return Ok(Value::Nil);
    }
    if !matches!(third, Value::Str(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *third],
        ));
    }
    Ok(Value::Nil)
}

/// (move-to-window-line ARG) -> integer or nil
///
/// Batch semantics: in non-window contexts this command errors with the
/// standard unrelated-buffer message.
pub(crate) fn builtin_move_to_window_line(args: Vec<Value>) -> EvalResult {
    expect_args("move-to-window-line", &args, 1)?;
    Err(signal(
        "error",
        vec![Value::string(
            "move-to-window-line called from unrelated buffer",
        )],
    ))
}

/// (tool-bar-height &optional FRAME PIXELWISE) -> integer
///
/// Get the height of the tool bar. Returns 0 (no tool bar).
pub(crate) fn builtin_tool_bar_height(args: Vec<Value>) -> EvalResult {
    expect_args_range("tool-bar-height", &args, 0, 2)?;
    // Return 0 (no tool bar)
    Ok(Value::Int(0))
}

/// `(tool-bar-height &optional FRAME PIXELWISE)` evaluator-backed variant.
///
/// Accepts nil or a live frame designator for FRAME.
pub(crate) fn builtin_tool_bar_height_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_tool_bar_height_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_tool_bar_height_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("tool-bar-height", &args, 0, 2)?;
    if let Some(frame) = args.first().filter(|frame| !frame.is_nil()) {
        let _ =
            super::window_cmds::resolve_frame_id_in_state(frames, buffers, Some(frame), "framep")?;
    }
    Ok(Value::Int(0))
}

/// (tab-bar-height &optional FRAME PIXELWISE) -> integer
///
/// Get the height of the tab bar. Returns 0 (no tab bar).
pub(crate) fn builtin_tab_bar_height(args: Vec<Value>) -> EvalResult {
    expect_args_range("tab-bar-height", &args, 0, 2)?;
    // Return 0 (no tab bar)
    Ok(Value::Int(0))
}

/// `(tab-bar-height &optional FRAME PIXELWISE)` evaluator-backed variant.
///
/// Accepts nil or a live frame designator for FRAME.
pub(crate) fn builtin_tab_bar_height_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_tab_bar_height_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_tab_bar_height_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args_range("tab-bar-height", &args, 0, 2)?;
    if let Some(frame) = args.first().filter(|frame| !frame.is_nil()) {
        let _ =
            super::window_cmds::resolve_frame_id_in_state(frames, buffers, Some(frame), "framep")?;
    }
    Ok(Value::Int(0))
}

/// (line-number-display-width &optional ON-DISPLAY) -> integer
///
/// Get the width of the line number display. Returns 0 (no line numbers).
pub(crate) fn builtin_line_number_display_width(args: Vec<Value>) -> EvalResult {
    expect_args_range("line-number-display-width", &args, 0, 1)?;
    // Return 0 (no line numbers)
    Ok(Value::Int(0))
}

/// (long-line-optimizations-p) -> boolean
///
/// Check if long-line optimizations are enabled. Returns nil.
pub(crate) fn builtin_long_line_optimizations_p(args: Vec<Value>) -> EvalResult {
    expect_args("long-line-optimizations-p", &args, 0)?;
    // Return nil (optimizations not enabled)
    Ok(Value::Nil)
}

fn validate_optional_frame_designator(
    eval: &super::eval::Evaluator,
    value: Option<&Value>,
) -> Result<(), Flow> {
    let Some(frameish) = value else {
        return Ok(());
    };
    if frameish.is_nil() {
        return Ok(());
    }
    match frameish {
        Value::Int(id) if *id >= 0 => {
            if eval.frames.get(FrameId(*id as u64)).is_some() {
                return Ok(());
            }
        }
        Value::Frame(id) => {
            if eval.frames.get(FrameId(*id)).is_some() {
                return Ok(());
            }
        }
        _ => {}
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("framep"), *frameish],
    ))
}

fn validate_optional_window_designator(
    eval: &super::eval::Evaluator,
    value: Option<&Value>,
    predicate: &str,
) -> Result<(), Flow> {
    let Some(windowish) = value else {
        return Ok(());
    };
    if windowish.is_nil() {
        return Ok(());
    }
    let wid = match windowish {
        Value::Window(id) => Some(WindowId(*id)),
        Value::Int(id) if *id >= 0 => Some(WindowId(*id as u64)),
        _ => None,
    };
    if let Some(wid) = wid {
        for fid in eval.frames.frame_list() {
            if let Some(frame) = eval.frames.get(fid) {
                if frame.find_window(wid).is_some() {
                    return Ok(());
                }
            }
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol(predicate), *windowish],
    ))
}

fn validate_optional_buffer_designator(
    eval: &super::eval::Evaluator,
    value: Option<&Value>,
) -> Result<(), Flow> {
    let Some(bufferish) = value else {
        return Ok(());
    };
    if bufferish.is_nil() {
        return Ok(());
    }
    if let Value::Buffer(id) = bufferish {
        if eval.buffers.get(*id).is_some() {
            return Ok(());
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("bufferp"), *bufferish],
    ))
}

// ---------------------------------------------------------------------------
// Bootstrap variables
// ---------------------------------------------------------------------------

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    obarray.set_symbol_value("redisplay--inhibit-bidi", Value::True);
    obarray.set_symbol_value("blink-matching-delay", Value::Int(1));
    obarray.set_symbol_value("blink-matching-paren", Value::True);
    obarray.set_symbol_value("global-font-lock-mode", Value::Nil);
    obarray.set_symbol_value("display-line-numbers", Value::Nil);
    obarray.set_symbol_value("display-line-numbers-type", Value::True);
    obarray.set_symbol_value("display-line-numbers-width", Value::Nil);
    obarray.set_symbol_value("display-line-numbers-current-absolute", Value::True);
    obarray.set_symbol_value("display-line-numbers-widen", Value::Nil);
    obarray.set_symbol_value("display-fill-column-indicator", Value::Nil);
    obarray.set_symbol_value("display-fill-column-indicator-column", Value::Nil);
    obarray.set_symbol_value("display-fill-column-indicator-character", Value::Nil);
    obarray.set_symbol_value("visible-bell", Value::Nil);
    obarray.set_symbol_value("no-redraw-on-reenter", Value::Nil);
    obarray.set_symbol_value("cursor-in-echo-area", Value::Nil);
    obarray.set_symbol_value("truncate-partial-width-windows", Value::Int(50));
    obarray.set_symbol_value("mode-line-in-non-selected-windows", Value::True);
    obarray.set_symbol_value("line-number-display-limit", Value::Nil);
    obarray.set_symbol_value("highlight-nonselected-windows", Value::Nil);
    obarray.set_symbol_value("message-truncate-lines", Value::Nil);
    obarray.set_symbol_value("scroll-step", Value::Int(0));
    obarray.set_symbol_value("scroll-conservatively", Value::Int(0));
    obarray.set_symbol_value("scroll-margin", Value::Int(0));
    obarray.set_symbol_value("hscroll-margin", Value::Int(5));
    obarray.set_symbol_value("hscroll-step", Value::Int(0));
    obarray.set_symbol_value("auto-hscroll-mode", Value::True);
    obarray.set_symbol_value("void-text-area-pointer", Value::symbol("arrow"));
    obarray.set_symbol_value("inhibit-message", Value::Nil);
    obarray.set_symbol_value("make-cursor-line-fully-visible", Value::True);
    obarray.set_symbol_value("x-stretch-cursor", Value::Nil);
    obarray.set_symbol_value("show-trailing-whitespace", Value::Nil);
    obarray.set_symbol_value("show-paren-context-when-offscreen", Value::Nil);
    obarray.set_symbol_value("nobreak-char-display", Value::True);
    obarray.set_symbol_value("overlay-arrow-variable-list", Value::Nil);
    obarray.set_symbol_value("overlay-arrow-string", Value::string("=>"));
    obarray.set_symbol_value("overlay-arrow-position", Value::Nil);
    // Mirror GNU Emacs: set char-table-extra-slots property for all subtypes
    // that need extra slots. Fmake_char_table reads this property to allocate
    // the correct number of extra slots.
    // See: casetab.c:249, category.c:426, character.c:1143, coding.c:11737,
    //      fontset.c:2158-2160, xdisp.c:31594, keymap.c:3346, syntax.c:3659
    obarray.put_property("case-table", "char-table-extra-slots", Value::Int(3));
    obarray.put_property("category-table", "char-table-extra-slots", Value::Int(2));
    obarray.put_property("char-script-table", "char-table-extra-slots", Value::Int(1));
    obarray.put_property("translation-table", "char-table-extra-slots", Value::Int(2));
    obarray.put_property("fontset", "char-table-extra-slots", Value::Int(8));
    obarray.put_property("fontset-info", "char-table-extra-slots", Value::Int(1));
    obarray.put_property(
        "glyphless-char-display",
        "char-table-extra-slots",
        Value::Int(1),
    );
    obarray.put_property("keymap", "char-table-extra-slots", Value::Int(0));
    obarray.put_property("syntax-table", "char-table-extra-slots", Value::Int(0));
    obarray.set_symbol_value(
        "char-script-table",
        make_char_table_with_extra_slots(Value::symbol("char-script-table"), Value::Nil, 1),
    );
    obarray.set_symbol_value("pre-redisplay-function", Value::Nil);
    obarray.set_symbol_value("pre-redisplay-functions", Value::Nil);

    // auto-fill-chars: a char-table for characters which invoke auto-filling.
    // Official Emacs (character.c) creates it with sub-type `auto-fill-chars`
    // and sets space and newline to t.
    let auto_fill = make_char_table_value(Value::symbol("auto-fill-chars"), Value::Nil);
    // Set space and newline entries to t.  We use set-char-table-range
    // via the underlying data: store single-char entries.
    use super::chartable::ct_set_single;
    ct_set_single(&auto_fill, ' ' as i64, Value::True);
    ct_set_single(&auto_fill, '\n' as i64, Value::True);
    obarray.set_symbol_value("auto-fill-chars", auto_fill);

    // char-width-table: a char-table for character display widths.
    // Official Emacs (character.c) creates it with default 1.
    obarray.set_symbol_value(
        "char-width-table",
        make_char_table_value(Value::symbol("char-width-table"), Value::Int(1)),
    );

    // translation-table-vector: vector recording all translation tables.
    // Official Emacs (character.c) creates a 16-element nil vector.
    obarray.set_symbol_value(
        "translation-table-vector",
        Value::vector(vec![Value::Nil; 16]),
    );

    // translation-hash-table-vector: vector of translation hash tables.
    // Official Emacs (ccl.c) initializes to nil.
    obarray.set_symbol_value("translation-hash-table-vector", Value::Nil);

    // printable-chars: a char-table of printable characters.
    // Official Emacs (character.c) creates it with default t.
    obarray.set_symbol_value(
        "printable-chars",
        make_char_table_value(Value::symbol("printable-chars"), Value::True),
    );

    // default-process-coding-system: cons of coding systems for process I/O.
    // Official Emacs (coding.c) initializes to nil.
    obarray.set_symbol_value("default-process-coding-system", Value::Nil);

    // ambiguous-width-chars: char-table for characters whose width can be 1 or 2.
    // Official Emacs (character.c) creates empty char-table; populated by characters.el.
    obarray.set_symbol_value(
        "ambiguous-width-chars",
        make_char_table_value(Value::Nil, Value::Nil),
    );

    // text-property-default-nonsticky: alist of properties vs non-stickiness.
    // Official Emacs (textprop.c) initializes to ((syntax-table . t) (display . t)).
    obarray.set_symbol_value(
        "text-property-default-nonsticky",
        Value::list(vec![
            Value::cons(Value::symbol("syntax-table"), Value::True),
            Value::cons(Value::symbol("display"), Value::True),
        ]),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "xdisp_test.rs"]
mod tests;
