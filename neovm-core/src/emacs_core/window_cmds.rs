//! Window, frame, and display-related builtins for the Elisp VM.
//!
//! Bridges the `FrameManager` (in `crate::window`) to Elisp by exposing
//! builtins such as `selected-window`, `split-window-internal`,
//! `selected-frame`, etc.
//! Frames are represented as frame handles. Windows are represented as window
//! handles, while legacy integer designators are still accepted in resolver
//! paths for compatibility.

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::{Value, list_to_vec, next_float_id, read_cons, with_heap};
use crate::buffer::BufferId;
use crate::window::{
    FrameId, FrameManager, SplitDirection, Window, WindowId, window_first_child_id,
    window_next_sibling_id, window_parent_id, window_prev_sibling_id,
};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Expect exactly N arguments.
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

/// Expect at least N arguments.
fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

/// Expect at most N arguments.
fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

/// Extract an integer from a Value.
fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

/// Extract a numeric value from a Value.
fn expect_number(value: &Value) -> Result<f64, Flow> {
    match value {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(n, _) => Ok(*n),
        Value::Char(c) => Ok(*c as i64 as f64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *other],
        )),
    }
}

#[derive(Clone, Debug)]
enum IntegerOrMarkerArg {
    Int(i64),
    Marker { raw: Value, position: Option<i64> },
}

fn parse_integer_or_marker_arg(value: &Value) -> Result<IntegerOrMarkerArg, Flow> {
    match value {
        Value::Int(n) => Ok(IntegerOrMarkerArg::Int(*n)),
        Value::Char(c) => Ok(IntegerOrMarkerArg::Int(*c as i64)),
        v if super::marker::is_marker(v) => {
            let position = match v {
                Value::Vector(vec) => {
                    let elems = with_heap(|h| h.get_vector(*vec).clone());
                    match elems.get(2) {
                        Some(Value::Int(n)) => Some(*n),
                        Some(Value::Char(c)) => Some(*c as i64),
                        _ => None,
                    }
                }
                _ => None,
            };
            Ok(IntegerOrMarkerArg::Marker {
                raw: *value,
                position,
            })
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn expect_number_or_marker_count(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        Value::Float(n, _) => Ok(n.floor() as i64),
        marker if super::marker::is_marker(marker) => match parse_integer_or_marker_arg(marker)? {
            IntegerOrMarkerArg::Marker {
                position: Some(pos),
                ..
            } => Ok(pos),
            IntegerOrMarkerArg::Marker { position: None, .. } => Err(signal(
                "error",
                vec![Value::string("Marker does not point anywhere")],
            )),
            IntegerOrMarkerArg::Int(pos) => Ok(pos),
        },
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *other],
        )),
    }
}

fn clamped_window_position(
    eval: &super::eval::Evaluator,
    fid: FrameId,
    wid: WindowId,
    pos: i64,
) -> Option<usize> {
    if pos <= 0 {
        return None;
    }
    let requested = pos as usize;
    let Some(Window::Leaf { buffer_id, .. }) = eval
        .frames
        .get(fid)
        .and_then(|frame| frame.find_window(wid))
    else {
        return Some(requested);
    };
    let buffer_end = eval
        .buffers
        .get(*buffer_id)
        .map(|buf| buf.text.char_count().saturating_add(1))
        .unwrap_or(requested);
    Some(requested.min(buffer_end.max(1)))
}

/// Extract a fixnum-like integer from a Value.
fn expect_fixnum(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *other],
        )),
    }
}

/// Extract a number-or-marker argument as f64.
fn expect_number_or_marker(value: &Value) -> Result<f64, Flow> {
    match value {
        Value::Int(n) => Ok(*n as f64),
        Value::Char(c) => Ok(*c as i64 as f64),
        Value::Float(f, _) => Ok(*f),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *other],
        )),
    }
}

/// Parse a window margin argument (`nil` or non-negative integer).
fn expect_margin_width(value: &Value) -> Result<usize, Flow> {
    const MAX_MARGIN: i64 = 2_147_483_647;
    match value {
        Value::Nil => Ok(0),
        Value::Int(n) => {
            if *n < 0 || *n > MAX_MARGIN {
                return Err(signal(
                    "args-out-of-range",
                    vec![Value::Int(*n), Value::Int(0), Value::Int(MAX_MARGIN)],
                ));
            }
            Ok(*n as usize)
        }
        Value::Char(c) => {
            let n = *c as i64;
            if n > MAX_MARGIN {
                return Err(signal(
                    "args-out-of-range",
                    vec![Value::Int(n), Value::Int(0), Value::Int(MAX_MARGIN)],
                ));
            }
            Ok(n as usize)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

fn window_value(wid: WindowId) -> Value {
    Value::Window(wid.0)
}

fn resolve_window_frame_id_for_pred(
    frames: &FrameManager,
    wid: WindowId,
    pred: &str,
) -> Option<FrameId> {
    match pred {
        "window-valid-p" => frames.find_valid_window_frame_id(wid),
        _ => frames.find_window_frame_id(wid),
    }
}

fn window_id_from_designator(value: &Value) -> Option<WindowId> {
    match value {
        Value::Window(id) => Some(WindowId(*id)),
        Value::Int(n) if *n >= 0 => Some(WindowId(*n as u64)),
        _ => None,
    }
}

/// Resolve an optional window designator.
///
/// - nil/omitted => selected window of selected frame
/// - non-nil invalid designator => `(wrong-type-argument PRED VALUE)`
fn resolve_window_id_with_pred(
    eval: &mut super::eval::Evaluator,
    arg: Option<&Value>,
    pred: &str,
) -> Result<(FrameId, WindowId), Flow> {
    match arg {
        None | Some(Value::Nil) => {
            let frame_id = ensure_selected_frame_id(eval);
            let frame = eval
                .frames
                .get(frame_id)
                .ok_or_else(|| signal("error", vec![Value::string("No selected frame")]))?;
            Ok((frame_id, frame.selected_window))
        }
        Some(val) => {
            let Some(wid) = window_id_from_designator(val) else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol(pred), *val],
                ));
            };
            if let Some(frame_id) = resolve_window_frame_id_for_pred(&eval.frames, wid, pred) {
                Ok((frame_id, wid))
            } else {
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol(pred), *val],
                ))
            }
        }
    }
}

fn resolve_window_id(
    eval: &mut super::eval::Evaluator,
    arg: Option<&Value>,
) -> Result<(FrameId, WindowId), Flow> {
    resolve_window_id_with_pred(eval, arg, "window-live-p")
}

/// Resolve an optional window designator that may be stale (window object).
///
/// - nil/omitted => selected live window
/// - non-nil invalid designator => `(wrong-type-argument PRED VALUE)`
fn resolve_window_object_id_with_pred(
    eval: &mut super::eval::Evaluator,
    arg: Option<&Value>,
    pred: &str,
) -> Result<WindowId, Flow> {
    match arg {
        None | Some(Value::Nil) => {
            let (_fid, wid) = resolve_window_id(eval, None)?;
            Ok(wid)
        }
        Some(val) => {
            let Some(wid) = window_id_from_designator(val) else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol(pred), *val],
                ));
            };
            if eval.frames.is_window_object_id(wid) {
                Ok(wid)
            } else {
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol(pred), *val],
                ))
            }
        }
    }
}

/// Resolve a window designator for mutation-style window ops.
///
/// GNU Emacs uses generic `error` signaling for invalid designators in some
/// split/delete window builtins, rather than `wrong-type-argument`.
fn resolve_window_id_or_error(
    eval: &mut super::eval::Evaluator,
    arg: Option<&Value>,
) -> Result<(FrameId, WindowId), Flow> {
    match arg {
        None | Some(Value::Nil) => resolve_window_id(eval, arg),
        Some(value) => {
            let Some(wid) = window_id_from_designator(value) else {
                return Err(signal("error", vec![Value::string("Invalid window")]));
            };
            if let Some(fid) = eval.frames.find_window_frame_id(wid) {
                Ok((fid, wid))
            } else {
                Err(signal("error", vec![Value::string("Invalid window")]))
            }
        }
    }
}

fn format_window_designator_for_error(eval: &super::eval::Evaluator, value: &Value) -> String {
    if let Some(wid) = window_id_from_designator(value) {
        if eval.frames.is_window_object_id(wid) || matches!(value, Value::Window(_)) {
            return format!("#<window {}>", wid.0);
        }
    }
    super::print::print_value(value)
}

fn resolve_window_id_or_window_error(
    eval: &mut super::eval::Evaluator,
    arg: Option<&Value>,
    live_only: bool,
) -> Result<(FrameId, WindowId), Flow> {
    match arg {
        None | Some(Value::Nil) => resolve_window_id(eval, arg),
        Some(val) => {
            let Some(wid) = window_id_from_designator(val) else {
                let window_kind = if live_only { "live" } else { "valid" };
                return Err(signal(
                    "error",
                    vec![Value::string(format!(
                        "{} is not a {} window",
                        format_window_designator_for_error(eval, val),
                        window_kind
                    ))],
                ));
            };
            let frame_id = if live_only {
                eval.frames.find_window_frame_id(wid)
            } else {
                eval.frames.find_valid_window_frame_id(wid)
            };
            if let Some(fid) = frame_id {
                Ok((fid, wid))
            } else {
                let window_kind = if live_only { "live" } else { "valid" };
                Err(signal(
                    "error",
                    vec![Value::string(format!(
                        "{} is not a {} window",
                        format_window_designator_for_error(eval, val),
                        window_kind
                    ))],
                ))
            }
        }
    }
}

/// Resolve a frame designator, signaling predicate-shaped type errors.
///
/// When ARG is nil/omitted, GNU Emacs resolves against the selected frame.
/// In batch compatibility mode we bootstrap that frame on demand.
fn resolve_frame_id(
    eval: &mut super::eval::Evaluator,
    arg: Option<&Value>,
    predicate: &str,
) -> Result<FrameId, Flow> {
    match arg {
        None | Some(Value::Nil) => Ok(ensure_selected_frame_id(eval)),
        Some(Value::Int(n)) => {
            let fid = FrameId(*n as u64);
            if eval.frames.get(fid).is_some() {
                Ok(fid)
            } else {
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol(predicate), Value::Int(*n)],
                ))
            }
        }
        Some(Value::Frame(id)) => {
            let fid = FrameId(*id);
            if eval.frames.get(fid).is_some() {
                Ok(fid)
            } else {
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol(predicate), Value::Frame(*id)],
                ))
            }
        }
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol(predicate), *other],
        )),
    }
}

/// Resolve a frame designator that may also be a live window designator.
///
/// `frame-first-window` accepts either a frame or window object in GNU Emacs.
fn resolve_frame_or_window_frame_id(
    eval: &mut super::eval::Evaluator,
    arg: Option<&Value>,
    predicate: &str,
) -> Result<FrameId, Flow> {
    match arg {
        None | Some(Value::Nil) => Ok(ensure_selected_frame_id(eval)),
        Some(Value::Frame(id)) => {
            let fid = FrameId(*id);
            if eval.frames.get(fid).is_some() {
                Ok(fid)
            } else {
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol(predicate), Value::Frame(*id)],
                ))
            }
        }
        Some(Value::Int(n)) => {
            let fid = FrameId(*n as u64);
            if eval.frames.get(fid).is_some() {
                return Ok(fid);
            }
            let wid = WindowId(*n as u64);
            if let Some(fid) = eval.frames.find_valid_window_frame_id(wid) {
                return Ok(fid);
            }
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol(predicate), Value::Int(*n)],
            ))
        }
        Some(Value::Window(id)) => {
            let wid = WindowId(*id);
            if let Some(fid) = eval.frames.find_valid_window_frame_id(wid) {
                return Ok(fid);
            }
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol(predicate), Value::Window(*id)],
            ))
        }
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol(predicate), *other],
        )),
    }
}

/// Helper: get a reference to a leaf window by id.
fn get_leaf(frames: &FrameManager, fid: FrameId, wid: WindowId) -> Result<&Window, Flow> {
    let frame = frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    frame
        .find_window(wid)
        .ok_or_else(|| signal("error", vec![Value::string("Window not found")]))
}

/// Ensure a selected frame exists and return its id.
///
/// In batch compatibility mode, GNU Emacs still has an initial frame (`F1`).
/// When the evaluator has no frame yet, synthesize one on demand.
pub(crate) fn ensure_selected_frame_id(eval: &mut super::eval::Evaluator) -> FrameId {
    if let Some(fid) = eval.frames.selected_frame().map(|f| f.id) {
        return fid;
    }

    let buf_id = eval
        .buffers
        .current_buffer()
        .map(|b| b.id)
        .unwrap_or_else(|| eval.buffers.create_buffer("*scratch*"));
    // Batch GNU Emacs startup exposes an initial ~80x24 text window plus
    // a minibuffer line; frame parameters report 80x25.
    // With our default 8x16 char metrics the text area corresponds to 640x384.
    let fid = eval.frames.create_frame("F1", 640, 384, buf_id);
    let minibuffer_buf_id = eval
        .buffers
        .find_buffer_by_name(" *Minibuf-0*")
        .unwrap_or_else(|| eval.buffers.create_buffer(" *Minibuf-0*"));
    if let Some(frame) = eval.frames.get_mut(fid) {
        frame.parameters.insert("width".to_string(), Value::Int(80));
        frame
            .parameters
            .insert("height".to_string(), Value::Int(25));
        if let Some(Window::Leaf {
            window_start,
            point,
            ..
        }) = frame.find_window_mut(frame.selected_window)
        {
            // Batch-mode startup in GNU Emacs reports point/window-start as 1.
            *window_start = 1;
            *point = 1;
        }
        if let Some(minibuffer_leaf) = frame.minibuffer_leaf.as_mut() {
            // Keep minibuffer window accessors aligned with GNU Emacs batch startup.
            minibuffer_leaf.set_buffer(minibuffer_buf_id);
        }
    }
    fid
}

/// Compute the height of a window in lines.
fn window_height_lines(w: &Window, char_height: f32) -> i64 {
    let h = w.bounds().height;
    if char_height > 0.0 {
        (h / char_height) as i64
    } else {
        0
    }
}

/// Compute the width of a window in columns.
fn window_width_cols(w: &Window, char_width: f32) -> i64 {
    let cw = w.bounds().width;
    if char_width > 0.0 {
        (cw / char_width) as i64
    } else {
        0
    }
}

fn is_minibuffer_window(frames: &FrameManager, fid: FrameId, wid: WindowId) -> bool {
    frames
        .get(fid)
        .is_some_and(|frame| frame.minibuffer_window == Some(wid))
}

fn window_body_height_lines(frames: &FrameManager, fid: FrameId, wid: WindowId, w: &Window) -> i64 {
    let ch = frames.get(fid).map(|f| f.char_height).unwrap_or(16.0);
    let lines = window_height_lines(w, ch);
    if is_minibuffer_window(frames, fid, wid) {
        lines
    } else {
        lines.saturating_sub(1)
    }
}

fn window_edges_cols_lines(w: &Window, char_width: f32, char_height: f32) -> (i64, i64, i64, i64) {
    let b = w.bounds();
    let left = if char_width > 0.0 {
        (b.x / char_width) as i64
    } else {
        0
    };
    let top = if char_height > 0.0 {
        (b.y / char_height) as i64
    } else {
        0
    };
    let right = if char_width > 0.0 {
        ((b.x + b.width) / char_width) as i64
    } else {
        0
    };
    let bottom = if char_height > 0.0 {
        ((b.y + b.height) / char_height) as i64
    } else {
        0
    };
    (left, top, right, bottom)
}

fn window_body_edges_cols_lines(
    frames: &FrameManager,
    fid: FrameId,
    wid: WindowId,
    w: &Window,
    char_width: f32,
    char_height: f32,
) -> (i64, i64, i64, i64) {
    let (left, top, right, bottom) = window_edges_cols_lines(w, char_width, char_height);
    let body_bottom = if is_minibuffer_window(frames, fid, wid) {
        bottom
    } else {
        bottom.saturating_sub(1)
    };
    (left, top, right, body_bottom)
}

// ===========================================================================
// Window queries
// ===========================================================================

/// `(selected-window)` -> window object.
pub(crate) fn builtin_selected_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("selected-window", &args, 0)?;
    let fid = ensure_selected_frame_id(eval);
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("No selected frame")]))?;
    Ok(window_value(frame.selected_window))
}

/// `(old-selected-window)` -> previous selected window.
pub(crate) fn builtin_old_selected_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("old-selected-window", &args, 0)?;
    let fid = ensure_selected_frame_id(eval);
    let selected_wid = eval
        .frames
        .get(fid)
        .map(|frame| frame.selected_window)
        .ok_or_else(|| signal("error", vec![Value::string("No selected frame")]))?;
    let old_wid = eval.frames.old_selected_window().unwrap_or(selected_wid);
    Ok(window_value(old_wid))
}

/// `(frame-selected-window &optional FRAME)` -> selected window of FRAME.
pub(crate) fn builtin_frame_selected_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-selected-window", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "frame-live-p")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    Ok(window_value(frame.selected_window))
}

/// `(frame-old-selected-window &optional FRAME)` -> nil.
///
/// Batch GNU Emacs reports nil for this accessor throughout startup and
/// selection operations; keep frame designator validation aligned with
/// `frame-live-p` semantics.
pub(crate) fn builtin_frame_old_selected_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-old-selected-window", &args, 1)?;
    let _ = resolve_frame_id(eval, args.first(), "frame-live-p")?;
    Ok(Value::Nil)
}

/// `(set-frame-selected-window FRAME WINDOW &optional NORECORD)` -> WINDOW.
pub(crate) fn builtin_set_frame_selected_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-frame-selected-window", &args, 2)?;
    expect_max_args("set-frame-selected-window", &args, 3)?;
    let fid = resolve_frame_id(eval, args.first(), "frame-live-p")?;
    let wid = match window_id_from_designator(&args[1]) {
        Some(wid) => {
            if eval.frames.find_window_frame_id(wid).is_none() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("window-live-p"), args[1]],
                ));
            }
            wid
        }
        None => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), args[1]],
            ));
        }
    };
    let window_fid = eval
        .frames
        .find_window_frame_id(wid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    if window_fid != fid {
        return Err(signal(
            "error",
            vec![Value::string(
                "In `set-frame-selected-window', WINDOW is not on FRAME",
            )],
        ));
    }
    let selected_fid = ensure_selected_frame_id(eval);
    if fid == selected_fid {
        let mut select_args = vec![window_value(wid)];
        if let Some(norecord) = args.get(2) {
            select_args.push(*norecord);
        }
        return builtin_select_window(eval, select_args);
    }

    let frame = eval
        .frames
        .get_mut(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    frame.selected_window = wid;
    Ok(window_value(wid))
}

/// `(frame-first-window &optional FRAME-OR-WINDOW)` -> first window on frame.
pub(crate) fn builtin_frame_first_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-first-window", &args, 1)?;
    let fid = resolve_frame_or_window_frame_id(eval, args.first(), "frame-live-p")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    let first = frame
        .window_list()
        .first()
        .copied()
        .unwrap_or(frame.selected_window);
    Ok(window_value(first))
}

/// `(frame-root-window &optional FRAME-OR-WINDOW)` -> root window on frame.
pub(crate) fn builtin_frame_root_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-root-window", &args, 1)?;
    let fid = resolve_frame_or_window_frame_id(eval, args.first(), "frame-live-p")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    Ok(window_value(frame.root_window.id()))
}

/// `(minibuffer-window &optional FRAME)` -> minibuffer window of FRAME.
pub(crate) fn builtin_minibuffer_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("minibuffer-window", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "frame-live-p")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    match frame.minibuffer_window {
        Some(wid) => Ok(window_value(wid)),
        None => Ok(Value::Nil),
    }
}

/// `(window-minibuffer-p &optional WINDOW)` -> t when WINDOW is minibuffer.
pub(crate) fn builtin_window_minibuffer_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-minibuffer-p", &args, 1)?;
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let is_minibuffer = eval
        .frames
        .get(fid)
        .is_some_and(|frame| frame.minibuffer_window == Some(wid));
    Ok(Value::bool(is_minibuffer))
}

/// `(minibuffer-selected-window)` -> nil in batch (no active minibuffer).
pub(crate) fn builtin_minibuffer_selected_window(args: Vec<Value>) -> EvalResult {
    expect_args("minibuffer-selected-window", &args, 0)?;
    Ok(Value::Nil)
}

/// `(active-minibuffer-window)` -> nil in batch.
pub(crate) fn builtin_active_minibuffer_window(args: Vec<Value>) -> EvalResult {
    expect_args("active-minibuffer-window", &args, 0)?;
    Ok(Value::Nil)
}

/// `(window-frame &optional WINDOW)` -> frame of WINDOW.
pub(crate) fn builtin_window_frame(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-frame", &args, 1)?;
    let (fid, _wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    Ok(Value::Frame(fid.0))
}

/// `(window-buffer &optional WINDOW)` -> buffer object.
pub(crate) fn builtin_window_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-buffer", &args, 1)?;
    let resolve_buffer = |frames: &FrameManager, fid: FrameId, wid: WindowId| -> EvalResult {
        let w = get_leaf(frames, fid, wid)?;
        match w.buffer_id() {
            Some(bid) => Ok(Value::Buffer(bid)),
            None => Ok(Value::Nil),
        }
    };

    match args.first() {
        None | Some(Value::Nil) => {
            let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "windowp")?;
            resolve_buffer(&eval.frames, fid, wid)
        }
        Some(val) => {
            let Some(wid) = window_id_from_designator(val) else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("windowp"), *val],
                ));
            };
            if let Some(fid) = eval.frames.find_window_frame_id(wid) {
                return resolve_buffer(&eval.frames, fid, wid);
            }
            if eval.frames.is_window_object_id(wid) {
                return Ok(Value::Nil);
            }
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("windowp"), *val],
            ))
        }
    }
}

/// `(window-display-table &optional WINDOW)` -> display table or nil.
pub(crate) fn builtin_window_display_table(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-display-table", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, wid) = resolve_window_id(eval, args.first())?;
    Ok(eval.frames.window_display_table(wid))
}

/// `(set-window-display-table WINDOW TABLE)` -> TABLE.
pub(crate) fn builtin_set_window_display_table(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-window-display-table", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, wid) = resolve_window_id(eval, args.first())?;
    let table = args[1];
    eval.frames.set_window_display_table(wid, table);
    Ok(table)
}

/// `(window-cursor-type &optional WINDOW)` -> cursor type object.
pub(crate) fn builtin_window_cursor_type(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-cursor-type", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, wid) = resolve_window_id(eval, args.first())?;
    Ok(eval.frames.window_cursor_type(wid))
}

/// `(set-window-cursor-type WINDOW TYPE)` -> TYPE.
pub(crate) fn builtin_set_window_cursor_type(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-window-cursor-type", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, wid) = resolve_window_id(eval, args.first())?;
    let cursor_type = args[1];
    eval.frames.set_window_cursor_type(wid, cursor_type);
    Ok(cursor_type)
}

/// `(window-parameter WINDOW PARAMETER)` -> window parameter or nil.
pub(crate) fn builtin_window_parameter(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("window-parameter", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let wid = resolve_window_object_id_with_pred(eval, args.first(), "windowp")?;
    Ok(eval
        .frames
        .window_parameter(wid, &args[1])
        .unwrap_or(Value::Nil))
}

/// `(set-window-parameter WINDOW PARAMETER VALUE)` -> VALUE.
pub(crate) fn builtin_set_window_parameter(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-window-parameter", &args, 3)?;
    let _ = ensure_selected_frame_id(eval);
    let wid = resolve_window_object_id_with_pred(eval, args.first(), "windowp")?;
    let value = args[2];
    eval.frames.set_window_parameter(wid, args[1], value);
    Ok(value)
}

/// `(window-parameters &optional WINDOW)` -> alist of parameters.
pub(crate) fn builtin_window_parameters(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-parameters", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    Ok(eval.frames.window_parameters_alist(wid))
}

/// `(window-parent &optional WINDOW)` -> parent window or nil.
pub(crate) fn builtin_window_parent(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-parent", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let Some(frame) = eval.frames.get(fid) else {
        return Err(signal("error", vec![Value::string("Frame not found")]));
    };
    Ok(window_parent_id(frame, wid).map_or(Value::Nil, window_value))
}

/// `(window-top-child &optional WINDOW)` -> top child for vertical combinations.
pub(crate) fn builtin_window_top_child(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-top-child", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let Some(frame) = eval.frames.get(fid) else {
        return Err(signal("error", vec![Value::string("Frame not found")]));
    };
    Ok(
        window_first_child_id(frame, wid, SplitDirection::Vertical)
            .map_or(Value::Nil, window_value),
    )
}

/// `(window-left-child &optional WINDOW)` -> left child for horizontal combinations.
pub(crate) fn builtin_window_left_child(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-left-child", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let Some(frame) = eval.frames.get(fid) else {
        return Err(signal("error", vec![Value::string("Frame not found")]));
    };
    Ok(
        window_first_child_id(frame, wid, SplitDirection::Horizontal)
            .map_or(Value::Nil, window_value),
    )
}

/// `(window-next-sibling &optional WINDOW)` -> next sibling or nil.
pub(crate) fn builtin_window_next_sibling(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-next-sibling", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let Some(frame) = eval.frames.get(fid) else {
        return Err(signal("error", vec![Value::string("Frame not found")]));
    };
    Ok(window_next_sibling_id(frame, wid).map_or(Value::Nil, window_value))
}

/// `(window-prev-sibling &optional WINDOW)` -> previous sibling or nil.
pub(crate) fn builtin_window_prev_sibling(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-prev-sibling", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let Some(frame) = eval.frames.get(fid) else {
        return Err(signal("error", vec![Value::string("Frame not found")]));
    };
    Ok(window_prev_sibling_id(frame, wid).map_or(Value::Nil, window_value))
}

/// `(window-normal-size &optional WINDOW HORIZONTAL)` -> proportional size.
pub(crate) fn builtin_window_normal_size(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-normal-size", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let horizontal = args.get(1).is_some_and(Value::is_truthy);
    let Some(frame) = eval.frames.get(fid) else {
        return Err(signal("error", vec![Value::string("Frame not found")]));
    };
    let window = frame
        .find_window(wid)
        .ok_or_else(|| signal("error", vec![Value::string("Window not found")]))?;
    let Some(parent_id) = window_parent_id(frame, wid) else {
        return Ok(Value::Float(1.0, next_float_id()));
    };
    let parent = frame
        .find_window(parent_id)
        .ok_or_else(|| signal("error", vec![Value::string("Window not found")]))?;

    let ratio = match parent {
        Window::Internal {
            direction,
            bounds: parent_bounds,
            ..
        } => match (horizontal, direction) {
            (true, SplitDirection::Horizontal) if parent_bounds.width > 0.0 => {
                window.bounds().width / parent_bounds.width
            }
            (false, SplitDirection::Vertical) if parent_bounds.height > 0.0 => {
                window.bounds().height / parent_bounds.height
            }
            _ => 1.0,
        },
        Window::Leaf { .. } => 1.0,
    };

    Ok(Value::Float(ratio as f64, next_float_id()))
}

/// `(window-start &optional WINDOW)` -> integer position.
pub(crate) fn builtin_window_start(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-start", &args, 1)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    match w {
        Window::Leaf { window_start, .. } => Ok(Value::Int(*window_start as i64)),
        _ => Ok(Value::Int(0)),
    }
}

/// `(window-group-start &optional WINDOW)` -> integer position.
///
/// Batch GNU Emacs exposes group-start as point-min (`1`) in startup flows.
pub(crate) fn builtin_window_group_start(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-group-start", &args, 1)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    if eval
        .frames
        .get(fid)
        .is_some_and(|frame| frame.minibuffer_window == Some(wid))
    {
        return Ok(Value::Int(1));
    }
    let w = get_leaf(&eval.frames, fid, wid)?;
    match w {
        Window::Leaf { window_start, .. } => Ok(Value::Int(*window_start as i64)),
        _ => Ok(Value::Int(1)),
    }
}

/// `(window-end &optional WINDOW UPDATE)` -> integer position.
///
/// We approximate window-end as window-start since we don't have real
/// display layout.  The UPDATE argument is ignored.
pub(crate) fn builtin_window_end(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-end", &args, 2)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    match w {
        Window::Leaf {
            window_start,
            bounds,
            buffer_id,
            ..
        } => {
            // Clamp the display estimate to the buffer's end position so empty
            // buffers report their 1-based start/end as GNU Emacs does.
            let frame = eval.frames.get(fid).unwrap();
            let lines = (bounds.height / frame.char_height) as usize;
            let cols = (bounds.width / frame.char_width) as usize;
            let estimated_end = window_start.saturating_add(lines.saturating_mul(cols));
            let buffer_end = eval
                .buffers
                .get(*buffer_id)
                .map(|buf| buf.text.char_count().saturating_add(1))
                .unwrap_or(*window_start);
            let clamped_end = estimated_end.min(buffer_end.max(*window_start));
            Ok(Value::Int(clamped_end as i64))
        }
        _ => Ok(Value::Int(0)),
    }
}

/// `(window-point &optional WINDOW)` -> integer position.
pub(crate) fn builtin_window_point(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-point", &args, 1)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    match w {
        Window::Leaf { point, .. } => Ok(Value::Int(*point as i64)),
        _ => Ok(Value::Int(0)),
    }
}

/// `(set-window-start WINDOW POS &optional NOFORCE)` -> POS.
pub(crate) fn builtin_set_window_start(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-window-start", &args, 2)?;
    expect_max_args("set-window-start", &args, 3)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let pos = parse_integer_or_marker_arg(&args[1])?;
    let is_minibuffer = eval
        .frames
        .get(fid)
        .is_some_and(|frame| frame.minibuffer_window == Some(wid));
    match pos {
        IntegerOrMarkerArg::Int(pos) => {
            if !is_minibuffer {
                if let Some(clamped) = clamped_window_position(eval, fid, wid, pos) {
                    if let Some(Window::Leaf { window_start, .. }) = eval
                        .frames
                        .get_mut(fid)
                        .and_then(|frame| frame.find_window_mut(wid))
                    {
                        *window_start = clamped;
                    }
                }
            }
            Ok(Value::Int(pos))
        }
        IntegerOrMarkerArg::Marker { raw, position } => {
            if !is_minibuffer {
                if let Some(pos) = position {
                    if let Some(clamped) = clamped_window_position(eval, fid, wid, pos) {
                        if let Some(Window::Leaf { window_start, .. }) = eval
                            .frames
                            .get_mut(fid)
                            .and_then(|frame| frame.find_window_mut(wid))
                        {
                            *window_start = clamped;
                        }
                    }
                }
            }
            Ok(raw)
        }
    }
}

/// `(set-window-group-start WINDOW POS &optional NOFORCE)` -> POS.
pub(crate) fn builtin_set_window_group_start(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-window-group-start", &args, 2)?;
    expect_max_args("set-window-group-start", &args, 3)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let pos = parse_integer_or_marker_arg(&args[1])?;
    let is_minibuffer = eval
        .frames
        .get(fid)
        .is_some_and(|frame| frame.minibuffer_window == Some(wid));
    match pos {
        IntegerOrMarkerArg::Int(pos) => {
            if !is_minibuffer {
                if let Some(clamped) = clamped_window_position(eval, fid, wid, pos) {
                    if let Some(Window::Leaf { window_start, .. }) = eval
                        .frames
                        .get_mut(fid)
                        .and_then(|frame| frame.find_window_mut(wid))
                    {
                        *window_start = clamped;
                    }
                }
            }
            Ok(Value::Int(pos))
        }
        IntegerOrMarkerArg::Marker { raw, position } => {
            if !is_minibuffer {
                if let Some(pos) = position {
                    if let Some(clamped) = clamped_window_position(eval, fid, wid, pos) {
                        if let Some(Window::Leaf {
                            window_start,
                            point,
                            ..
                        }) = eval
                            .frames
                            .get_mut(fid)
                            .and_then(|frame| frame.find_window_mut(wid))
                        {
                            *window_start = clamped;
                            *point = clamped;
                        }
                    }
                }
            }
            Ok(raw)
        }
    }
}

/// `(set-window-point WINDOW POS)` -> POS.
pub(crate) fn builtin_set_window_point(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-window-point", &args, 2)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let pos = parse_integer_or_marker_arg(&args[1])?;
    let is_minibuffer = eval
        .frames
        .get(fid)
        .is_some_and(|frame| frame.minibuffer_window == Some(wid));
    match pos {
        IntegerOrMarkerArg::Int(pos) => {
            if !is_minibuffer {
                if let Some(clamped) = clamped_window_position(eval, fid, wid, pos) {
                    if let Some(Window::Leaf { point, .. }) = eval
                        .frames
                        .get_mut(fid)
                        .and_then(|frame| frame.find_window_mut(wid))
                    {
                        *point = clamped;
                    }
                }
            }
            Ok(Value::Int(pos))
        }
        IntegerOrMarkerArg::Marker { raw, position } => {
            if is_minibuffer {
                return Ok(raw);
            }
            let pos = position.ok_or_else(|| {
                signal(
                    "error",
                    vec![Value::string("Marker does not point anywhere")],
                )
            })?;
            if let Some(clamped) = clamped_window_position(eval, fid, wid, pos) {
                if let Some(Window::Leaf { point, .. }) = eval
                    .frames
                    .get_mut(fid)
                    .and_then(|frame| frame.find_window_mut(wid))
                {
                    *point = clamped;
                }
                Ok(Value::Int(clamped as i64))
            } else {
                Ok(Value::Int(1))
            }
        }
    }
}

/// `(window-height &optional WINDOW)` -> integer (lines).
pub(crate) fn builtin_window_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-height", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let ch = eval.frames.get(fid).map(|f| f.char_height).unwrap_or(16.0);
    Ok(Value::Int(window_height_lines(w, ch)))
}

/// `(window-width &optional WINDOW)` -> integer (columns).
pub(crate) fn builtin_window_width(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-width", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let cw = eval.frames.get(fid).map(|f| f.char_width).unwrap_or(8.0);
    Ok(Value::Int(window_width_cols(w, cw)))
}

/// `(window-use-time &optional WINDOW)` -> integer.
pub(crate) fn builtin_window_use_time(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-use-time", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, wid) = resolve_window_id(eval, args.first())?;
    Ok(Value::Int(eval.frames.window_use_time(wid)))
}

/// `(window-bump-use-time &optional WINDOW)` -> integer or nil.
pub(crate) fn builtin_window_bump_use_time(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-bump-use-time", &args, 1)?;
    let selected_fid = ensure_selected_frame_id(eval);
    let selected_wid = eval
        .frames
        .get(selected_fid)
        .map(|frame| frame.selected_window)
        .ok_or_else(|| signal("error", vec![Value::string("No selected frame")]))?;
    let target_wid = match args.first() {
        None | Some(Value::Nil) => selected_wid,
        Some(Value::Window(id)) => {
            let wid = WindowId(*id);
            if eval.frames.find_window_frame_id(wid).is_none() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("window-live-p"), Value::Window(*id)],
                ));
            }
            wid
        }
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *other],
            ));
        }
    };
    Ok(
        match eval.frames.bump_window_use_time(selected_wid, target_wid) {
            Some(use_time) => Value::Int(use_time),
            None => Value::Nil,
        },
    )
}

/// `(window-old-point &optional WINDOW)` -> integer.
pub(crate) fn builtin_window_old_point(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-old-point", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    match w {
        Window::Leaf { point, .. } => Ok(Value::Int((*point).max(1) as i64)),
        _ => Ok(Value::Int(1)),
    }
}

/// `(window-old-buffer &optional WINDOW)` -> nil in batch.
pub(crate) fn builtin_window_old_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-old-buffer", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, _wid) = resolve_window_id(eval, args.first())?;
    Ok(Value::Nil)
}

/// `(window-prev-buffers &optional WINDOW)` -> previous buffer list or nil.
pub(crate) fn builtin_window_prev_buffers(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-prev-buffers", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, wid) = resolve_window_id(eval, args.first())?;
    Ok(eval.frames.window_prev_buffers(wid))
}

/// `(window-next-buffers &optional WINDOW)` -> next buffer list or nil.
pub(crate) fn builtin_window_next_buffers(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-next-buffers", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, wid) = resolve_window_id(eval, args.first())?;
    Ok(eval.frames.window_next_buffers(wid))
}

/// `(set-window-prev-buffers WINDOW PREV-BUFFERS)` -> PREV-BUFFERS.
pub(crate) fn builtin_set_window_prev_buffers(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-window-prev-buffers", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, wid) = resolve_window_id(eval, args.first())?;
    let value = args[1];
    eval.frames.set_window_prev_buffers(wid, value);
    Ok(value)
}

/// `(set-window-next-buffers WINDOW NEXT-BUFFERS)` -> NEXT-BUFFERS.
pub(crate) fn builtin_set_window_next_buffers(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-window-next-buffers", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, wid) = resolve_window_id(eval, args.first())?;
    let value = args[1];
    eval.frames.set_window_next_buffers(wid, value);
    Ok(value)
}

/// `(window-left-column &optional WINDOW)` -> integer.
pub(crate) fn builtin_window_left_column(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-left-column", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let cw = eval.frames.get(fid).map(|f| f.char_width).unwrap_or(8.0);
    let left = if cw > 0.0 {
        (w.bounds().x / cw) as i64
    } else {
        0
    };
    Ok(Value::Int(left))
}

/// `(window-top-line &optional WINDOW)` -> integer.
pub(crate) fn builtin_window_top_line(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-top-line", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let ch = eval.frames.get(fid).map(|f| f.char_height).unwrap_or(16.0);
    let top = if ch > 0.0 {
        (w.bounds().y / ch) as i64
    } else {
        0
    };
    Ok(Value::Int(top))
}

/// `(window-pixel-left &optional WINDOW)` -> integer.
pub(crate) fn builtin_window_pixel_left(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-pixel-left", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let cw = eval.frames.get(fid).map(|f| f.char_width).unwrap_or(8.0);
    let left = if cw > 0.0 {
        (w.bounds().x / cw) as i64
    } else {
        0
    };
    Ok(Value::Int(left))
}

/// `(window-pixel-top &optional WINDOW)` -> integer.
pub(crate) fn builtin_window_pixel_top(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-pixel-top", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let ch = eval.frames.get(fid).map(|f| f.char_height).unwrap_or(16.0);
    let top = if ch > 0.0 {
        (w.bounds().y / ch) as i64
    } else {
        0
    };
    Ok(Value::Int(top))
}

/// `(window-hscroll &optional WINDOW)` -> integer.
pub(crate) fn builtin_window_hscroll(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-hscroll", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    match w {
        Window::Leaf { hscroll, .. } => Ok(Value::Int(*hscroll as i64)),
        _ => Ok(Value::Int(0)),
    }
}

/// `(set-window-hscroll WINDOW NCOLS)` -> integer.
pub(crate) fn builtin_set_window_hscroll(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-window-hscroll", &args, 2)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let cols = expect_fixnum(&args[1])?.max(0) as usize;
    if let Some(Window::Leaf { hscroll, .. }) = eval
        .frames
        .get_mut(fid)
        .and_then(|frame| frame.find_window_mut(wid))
    {
        *hscroll = cols;
    }
    Ok(Value::Int(cols as i64))
}

fn scroll_prefix_value(value: &Value) -> i64 {
    match value {
        Value::Int(n) => *n,
        Value::Float(f, _) => *f as i64,
        Value::Char(c) => *c as i64,
        Value::Symbol(id) if resolve_sym(*id) == "-" => -1,
        Value::Cons(cell) => {
            let car = {
                let pair = read_cons(*cell);
                pair.car
            };
            match car {
                Value::Int(n) => n,
                Value::Float(f, _) => f as i64,
                Value::Char(c) => c as i64,
                _ => 1,
            }
        }
        _ => 1,
    }
}

fn default_scroll_columns(eval: &super::eval::Evaluator, fid: FrameId, wid: WindowId) -> i64 {
    let char_width = eval.frames.get(fid).map(|f| f.char_width).unwrap_or(8.0);
    let window_cols = get_leaf(&eval.frames, fid, wid)
        .ok()
        .map(|leaf| {
            if char_width > 0.0 {
                (leaf.bounds().width / char_width).floor() as i64
            } else {
                80
            }
        })
        .unwrap_or(80);
    (window_cols - 2).max(1)
}

/// `(scroll-left &optional SET-MINIMUM ARG)` -> new horizontal scroll amount.
pub(crate) fn builtin_scroll_left(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("scroll-left", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, None)?;
    let base = eval
        .frames
        .get(fid)
        .and_then(|frame| frame.find_window(wid))
        .and_then(|window| match window {
            Window::Leaf { hscroll, .. } => Some(*hscroll as i64),
            _ => None,
        })
        .unwrap_or(0);
    let delta = match args.first() {
        None | Some(Value::Nil) => default_scroll_columns(eval, fid, wid),
        Some(value) => scroll_prefix_value(value),
    };
    let mut next = base as i128 + delta as i128;
    if next < 0 {
        next = 0;
    }
    let next = next.min(i64::MAX as i128) as i64;
    if let Some(Window::Leaf { hscroll, .. }) = eval
        .frames
        .get_mut(fid)
        .and_then(|frame| frame.find_window_mut(wid))
    {
        *hscroll = next as usize;
    }
    Ok(Value::Int(next))
}

/// `(scroll-right &optional SET-MINIMUM ARG)` -> new horizontal scroll amount.
pub(crate) fn builtin_scroll_right(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("scroll-right", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, None)?;
    let base = eval
        .frames
        .get(fid)
        .and_then(|frame| frame.find_window(wid))
        .and_then(|window| match window {
            Window::Leaf { hscroll, .. } => Some(*hscroll as i64),
            _ => None,
        })
        .unwrap_or(0);
    let delta = match args.first() {
        None | Some(Value::Nil) => default_scroll_columns(eval, fid, wid),
        Some(value) => scroll_prefix_value(value),
    };
    let mut next = base as i128 - delta as i128;
    if next < 0 {
        next = 0;
    }
    let next = next.min(i64::MAX as i128) as i64;
    if let Some(Window::Leaf { hscroll, .. }) = eval
        .frames
        .get_mut(fid)
        .and_then(|frame| frame.find_window_mut(wid))
    {
        *hscroll = next as usize;
    }
    Ok(Value::Int(next))
}

/// `(window-vscroll &optional WINDOW PIXELWISE)` -> integer.
///
/// Batch-mode GNU Emacs reports zero vertical scroll, including for minibuffer
/// windows; `PIXELWISE` is accepted but ignored.
pub(crate) fn builtin_window_vscroll(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-vscroll", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, _wid) = resolve_window_id(eval, args.first())?;
    Ok(Value::Int(0))
}

/// `(set-window-vscroll WINDOW VSCROLL &optional PIXELWISE PRESERVE)` -> integer.
///
/// We currently model batch semantics where visible vertical scrolling remains
/// zero; argument validation follows GNU Emacs (`WINDOW` live predicate and
/// `VSCROLL` as `numberp`).
pub(crate) fn builtin_set_window_vscroll(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-window-vscroll", &args, 2)?;
    expect_max_args("set-window-vscroll", &args, 4)?;
    let (_fid, _wid) = resolve_window_id(eval, args.first())?;
    match &args[1] {
        Value::Int(_) | Value::Float(_, _) | Value::Char(_) => Ok(Value::Int(0)),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *other],
        )),
    }
}

/// `(set-window-margins WINDOW LEFT-WIDTH &optional RIGHT-WIDTH)` -> changed-p.
pub(crate) fn builtin_set_window_margins(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-window-margins", &args, 2)?;
    expect_max_args("set-window-margins", &args, 3)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let left = expect_margin_width(&args[1])?;
    let right = if let Some(arg) = args.get(2) {
        expect_margin_width(arg)?
    } else {
        0
    };

    if let Some(Window::Leaf { margins, .. }) = eval
        .frames
        .get_mut(fid)
        .and_then(|frame| frame.find_window_mut(wid))
    {
        let next = (left, right);
        if *margins != next {
            *margins = next;
            return Ok(Value::True);
        }
    }
    Ok(Value::Nil)
}

/// `(window-margins &optional WINDOW)` -> margins pair or nil.
pub(crate) fn builtin_window_margins(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-margins", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let (left, right) = match w {
        Window::Leaf { margins, .. } => *margins,
        _ => (0, 0),
    };
    let left_v = if left == 0 {
        Value::Nil
    } else {
        Value::Int(left as i64)
    };
    let right_v = if right == 0 {
        Value::Nil
    } else {
        Value::Int(right as i64)
    };
    Ok(Value::cons(left_v, right_v))
}

/// `(window-fringes &optional WINDOW)` -> fringe tuple.
pub(crate) fn builtin_window_fringes(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-fringes", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, _wid) = resolve_window_id(eval, args.first())?;
    // Batch GNU Emacs startup reports zero-width fringes.
    Ok(Value::list(vec![
        Value::Int(0),
        Value::Int(0),
        Value::Nil,
        Value::Nil,
    ]))
}

/// `(set-window-fringes WINDOW LEFT &optional RIGHT OUTSIDE-MARGINS PERSISTENT)` -> nil.
pub(crate) fn builtin_set_window_fringes(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-window-fringes", &args, 2)?;
    expect_max_args("set-window-fringes", &args, 5)?;
    let (_fid, _wid) = resolve_window_id(eval, args.first())?;
    Ok(Value::Nil)
}

/// `(window-scroll-bars &optional WINDOW)` -> scroll-bar tuple.
pub(crate) fn builtin_window_scroll_bars(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-scroll-bars", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (_fid, _wid) = resolve_window_id(eval, args.first())?;
    // Batch GNU Emacs startup reports no scroll-bars with default sizing payload.
    Ok(Value::list(vec![
        Value::Nil,
        Value::Int(0),
        Value::True,
        Value::Nil,
        Value::Int(0),
        Value::True,
        Value::Nil,
    ]))
}

/// `(set-window-scroll-bars WINDOW &optional WIDTH VERTICAL-TYPE HEIGHT HORIZONTAL-TYPE)` -> nil.
pub(crate) fn builtin_set_window_scroll_bars(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-window-scroll-bars", &args, 1)?;
    expect_max_args("set-window-scroll-bars", &args, 6)?;
    let (_fid, _wid) = resolve_window_id(eval, args.first())?;
    Ok(Value::Nil)
}

/// `(window-mode-line-height &optional WINDOW)` -> integer.
pub(crate) fn builtin_window_mode_line_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-mode-line-height", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let height = if is_minibuffer_window(&eval.frames, fid, wid) {
        0
    } else {
        1
    };
    Ok(Value::Int(height))
}

/// `(window-header-line-height &optional WINDOW)` -> integer.
pub(crate) fn builtin_window_header_line_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-header-line-height", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let _ = resolve_window_id(eval, args.first())?;
    Ok(Value::Int(0))
}

/// `(window-pixel-height &optional WINDOW)` -> integer.
///
/// Batch GNU Emacs reports character-line units for this query.
pub(crate) fn builtin_window_pixel_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-pixel-height", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let ch = eval.frames.get(fid).map(|f| f.char_height).unwrap_or(16.0);
    Ok(Value::Int(window_height_lines(w, ch)))
}

/// `(window-pixel-width &optional WINDOW)` -> integer.
///
/// Batch GNU Emacs reports character-column units for this query.
pub(crate) fn builtin_window_pixel_width(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-pixel-width", &args, 1)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let cw = eval.frames.get(fid).map(|f| f.char_width).unwrap_or(8.0);
    Ok(Value::Int(window_width_cols(w, cw)))
}

/// `(window-body-height &optional WINDOW PIXELWISE)` -> integer.
pub(crate) fn builtin_window_body_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-body-height", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let _pixelwise = args.get(1);
    // Batch GNU Emacs returns character-height values even when PIXELWISE is non-nil.
    // The body area excludes one mode-line row for regular windows, but
    // minibuffer windows report their full single-line height.
    let body_lines = window_body_height_lines(&eval.frames, fid, wid, w);
    Ok(Value::Int(body_lines))
}

/// `(window-body-width &optional WINDOW PIXELWISE)` -> integer.
pub(crate) fn builtin_window_body_width(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-body-width", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let _pixelwise = args.get(1);
    // Batch GNU Emacs returns character-width values even when PIXELWISE is non-nil.
    let cw = eval.frames.get(fid).map(|f| f.char_width).unwrap_or(8.0);
    Ok(Value::Int(window_width_cols(w, cw)))
}

/// `(window-text-height &optional WINDOW PIXELWISE)` -> integer.
pub(crate) fn builtin_window_text_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-text-height", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let _pixelwise = args.get(1);
    Ok(Value::Int(window_body_height_lines(
        &eval.frames,
        fid,
        wid,
        w,
    )))
}

/// `(window-text-width &optional WINDOW PIXELWISE)` -> integer.
pub(crate) fn builtin_window_text_width(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-text-width", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let _pixelwise = args.get(1);
    let cw = eval.frames.get(fid).map(|f| f.char_width).unwrap_or(8.0);
    Ok(Value::Int(window_width_cols(w, cw)))
}

/// `(window-edges &optional WINDOW BODY ABSOLUTE)`.
///
/// GNU Emacs currently reports max arity 4; trailing args are accepted.
pub(crate) fn builtin_window_edges(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-edges", &args, 4)?;
    let _ = ensure_selected_frame_id(eval);
    let body = args.get(1).is_some_and(Value::is_truthy);
    let live_only = body;
    let (fid, wid) = resolve_window_id_or_window_error(eval, args.first(), live_only)?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    let (left, top, right, bottom) = if body {
        window_body_edges_cols_lines(
            &eval.frames,
            fid,
            wid,
            w,
            frame.char_width,
            frame.char_height,
        )
    } else {
        window_edges_cols_lines(w, frame.char_width, frame.char_height)
    };
    Ok(Value::list(vec![
        Value::Int(left),
        Value::Int(top),
        Value::Int(right),
        Value::Int(bottom),
    ]))
}

/// `(window-total-height &optional WINDOW ROUND)` -> integer.
///
pub(crate) fn builtin_window_total_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-total-height", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let ch = eval.frames.get(fid).map(|f| f.char_height).unwrap_or(16.0);
    Ok(Value::Int(window_height_lines(w, ch)))
}

/// `(window-total-width &optional WINDOW ROUND)` -> integer.
///
pub(crate) fn builtin_window_total_width(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-total-width", &args, 2)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    let cw = eval.frames.get(fid).map(|f| f.char_width).unwrap_or(8.0);
    Ok(Value::Int(window_width_cols(w, cw)))
}

/// `(window-list &optional FRAME MINIBUF ALL-FRAMES)` -> list of window objects.
pub(crate) fn builtin_window_list(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-list", &args, 3)?;
    let selected_fid = ensure_selected_frame_id(eval);
    // GNU Emacs validates ALL-FRAMES before FRAME mismatch checks.
    let all_frames_fid = match args.get(2) {
        None | Some(Value::Nil) => None,
        Some(arg) => {
            let Some(wid) = window_id_from_designator(arg) else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("windowp"), *arg],
                ));
            };
            if let Some(fid) = eval.frames.find_window_frame_id(wid) {
                Some(fid)
            } else if eval.frames.is_window_object_id(wid) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("window-live-p"), *arg],
                ));
            } else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("windowp"), *arg],
                ));
            }
        }
    };
    let mut fid = match args.first() {
        None | Some(Value::Nil) => selected_fid,
        Some(Value::Int(n)) => {
            let fid = FrameId(*n as u64);
            if eval.frames.get(fid).is_some() {
                fid
            } else {
                return Err(signal(
                    "error",
                    vec![Value::string("Window is on a different frame")],
                ));
            }
        }
        Some(Value::Frame(id)) => {
            let fid = FrameId(*id);
            if eval.frames.get(fid).is_some() {
                fid
            } else {
                return Err(signal(
                    "error",
                    vec![Value::string("Window is on a different frame")],
                ));
            }
        }
        Some(_) => {
            return Err(signal(
                "error",
                vec![Value::string("Window is on a different frame")],
            ));
        }
    };
    if let Some(all_frames_fid) = all_frames_fid {
        fid = all_frames_fid;
    }
    let include_minibuffer = matches!(args.get(1), Some(Value::True));
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    let mut ids: Vec<Value> = frame.window_list().into_iter().map(window_value).collect();
    if include_minibuffer {
        if let Some(minibuffer_wid) = frame.minibuffer_window {
            ids.push(window_value(minibuffer_wid));
        }
    }
    Ok(Value::list(ids))
}

/// `(window-list-1 &optional WINDOW MINIBUF ALL-FRAMES)` -> list of live windows.
pub(crate) fn builtin_window_list_1(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-list-1", &args, 3)?;
    let _ = ensure_selected_frame_id(eval);
    let (fid, start_wid) = match args.first() {
        None | Some(Value::Nil) => resolve_window_id_with_pred(eval, None, "window-live-p")?,
        Some(Value::Window(id)) => {
            let wid = WindowId(*id);
            if let Some(fid) = eval.frames.find_window_frame_id(wid) {
                (fid, wid)
            } else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("window-live-p"), args[0]],
                ));
            }
        }
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *other],
            ));
        }
    };

    // ALL-FRAMES matches GNU Emacs: nil/default => WINDOW's frame; t => all
    // frames; 'visible and 0 => visible/iconified frames (we only model
    // visibility); a frame object => that frame; anything else => WINDOW's frame.
    let mut frame_ids: Vec<FrameId> = match args.get(2) {
        None | Some(Value::Nil) => vec![fid],
        Some(Value::True) => {
            let mut ids = eval.frames.frame_list();
            ids.sort_by_key(|f| f.0);
            ids
        }
        Some(Value::Symbol(sym)) if resolve_sym(*sym) == "visible" => {
            let mut ids = eval.frames.frame_list();
            ids.sort_by_key(|f| f.0);
            ids.into_iter()
                .filter(|frame_id| {
                    eval.frames
                        .get(*frame_id)
                        .is_some_and(|frame| frame.visible)
                })
                .collect()
        }
        Some(Value::Int(0)) => {
            let mut ids = eval.frames.frame_list();
            ids.sort_by_key(|f| f.0);
            ids.into_iter()
                .filter(|frame_id| {
                    eval.frames
                        .get(*frame_id)
                        .is_some_and(|frame| frame.visible)
                })
                .collect()
        }
        Some(Value::Frame(frame_raw_id)) => {
            let frame_id = FrameId(*frame_raw_id);
            if eval.frames.get(frame_id).is_none() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("frame-live-p"), args[2]],
                ));
            }
            vec![frame_id]
        }
        Some(_) => vec![fid],
    };
    if frame_ids.is_empty() {
        frame_ids.push(fid);
    }

    if let Some(start_pos) = frame_ids.iter().position(|frame_id| *frame_id == fid) {
        frame_ids.rotate_left(start_pos);
    }

    let include_minibuffer = matches!(args.get(1), Some(Value::True));
    let mut seen_window_ids: HashSet<u64> = HashSet::new();
    let mut windows: Vec<Value> = Vec::new();

    for frame_id in frame_ids {
        let Some(frame) = eval.frames.get(frame_id) else {
            continue;
        };

        // GNU Emacs starts traversal at WINDOW when it appears in the returned list.
        let mut window_ids = frame.window_list();
        if frame_id == fid {
            if let Some(start_index) = window_ids.iter().position(|wid| *wid == start_wid) {
                window_ids.rotate_left(start_index);
            }
        }

        for window_id in window_ids {
            if seen_window_ids.insert(window_id.0) {
                windows.push(window_value(window_id));
            }
        }

        if include_minibuffer {
            if let Some(minibuffer_wid) = frame.minibuffer_window {
                if seen_window_ids.insert(minibuffer_wid.0) {
                    windows.push(window_value(minibuffer_wid));
                }
            }
        }
    }

    Ok(Value::list(windows))
}

/// `(get-buffer-window &optional BUFFER-OR-NAME ALL-FRAMES)` -> window or nil.
///
/// Batch-compatible behavior: search the selected frame for a window showing
/// the requested buffer.
pub(crate) fn builtin_get_buffer_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("get-buffer-window", &args, 2)?;
    let target = match args.first() {
        None | Some(Value::Nil) => return Ok(Value::Nil),
        Some(Value::Str(_)) => {
            let name_s = args[0].as_str().unwrap();
            match eval.buffers.find_buffer_by_name(name_s) {
                Some(id) => id,
                None => return Ok(Value::Nil),
            }
        }
        Some(Value::Buffer(id)) => {
            if eval.buffers.get(*id).is_none() {
                return Ok(Value::Nil);
            }
            *id
        }
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let fid = ensure_selected_frame_id(eval);
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;

    for wid in frame.window_list() {
        let matches = frame
            .find_window(wid)
            .and_then(|w| w.buffer_id())
            .is_some_and(|bid| bid == target);
        if matches {
            return Ok(window_value(wid));
        }
    }

    Ok(Value::Nil)
}

/// `(window-dedicated-p &optional WINDOW)` -> t or nil.
pub(crate) fn builtin_window_dedicated_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("window-dedicated-p", &args, 1)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let w = get_leaf(&eval.frames, fid, wid)?;
    match w {
        Window::Leaf { dedicated, .. } => Ok(Value::bool(*dedicated)),
        _ => Ok(Value::Nil),
    }
}

/// `(set-window-dedicated-p WINDOW FLAG)` -> FLAG.
pub(crate) fn builtin_set_window_dedicated_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-window-dedicated-p", &args, 2)?;
    let flag = args[1].is_truthy();
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    if let Some(w) = eval
        .frames
        .get_mut(fid)
        .and_then(|f| f.find_window_mut(wid))
    {
        if let Window::Leaf { dedicated, .. } = w {
            *dedicated = flag;
        }
    }
    Ok(Value::bool(flag))
}

/// `(windowp OBJ)` -> t if OBJ is a window object/designator that exists.
pub(crate) fn builtin_windowp(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("windowp", &args, 1)?;
    let wid = match window_id_from_designator(&args[0]) {
        Some(wid) => wid,
        None => return Ok(Value::Nil),
    };
    let found = eval.frames.is_window_object_id(wid);
    Ok(Value::bool(found))
}

/// `(window-valid-p OBJ)` -> t if OBJ is a live window.
pub(crate) fn builtin_window_valid_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("window-valid-p", &args, 1)?;
    let wid = match window_id_from_designator(&args[0]) {
        Some(wid) => wid,
        None => return Ok(Value::Nil),
    };
    Ok(Value::bool(eval.frames.is_valid_window_id(wid)))
}

/// `(window-live-p OBJ)` -> t if OBJ is a live leaf window.
pub(crate) fn builtin_window_live_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("window-live-p", &args, 1)?;
    let wid = match window_id_from_designator(&args[0]) {
        Some(wid) => wid,
        None => return Ok(Value::Nil),
    };
    let live = eval.frames.is_live_window_id(wid);
    Ok(Value::bool(live))
}

/// `(window-at X Y &optional FRAME)` -> window object or nil.
pub(crate) fn builtin_window_at(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("window-at", &args, 2)?;
    expect_max_args("window-at", &args, 3)?;
    let x = expect_number(&args[0])?;
    let y = expect_number(&args[1])?;
    let fid = resolve_frame_id(eval, args.get(2), "frame-live-p")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    let total_cols = frame_total_cols(frame) as f64;
    let total_lines = frame_total_lines(frame) as f64;
    if x < 0.0 || y < 0.0 || x >= total_cols || y >= total_lines {
        return Ok(Value::Nil);
    }

    let px = (x * frame.char_width as f64) as f32;
    let py = (y * frame.char_height as f64) as f32;
    if let Some(wid) = frame.window_at(px, py) {
        return Ok(window_value(wid));
    }

    if let (Some(minibuffer_wid), Some(minibuffer_leaf)) =
        (frame.minibuffer_window, frame.minibuffer_leaf.as_ref())
    {
        if minibuffer_leaf.bounds().contains(px, py) {
            return Ok(window_value(minibuffer_wid));
        }
    }

    Ok(Value::Nil)
}

// ===========================================================================
// Window manipulation
// ===========================================================================

pub(crate) fn split_window_internal_impl(
    eval: &mut super::eval::Evaluator,
    window: Value,
    side: Value,
) -> EvalResult {
    let (fid, wid) = resolve_window_id_or_error(eval, Some(&window))?;

    // Determine split direction from SIDE argument.
    let direction = match side {
        Value::Symbol(id) if resolve_sym(id) == "right" || resolve_sym(id) == "left" => {
            SplitDirection::Horizontal
        }
        _ => SplitDirection::Vertical,
    };

    // Use the same buffer as the window being split.
    let buf_id = {
        let w = get_leaf(&eval.frames, fid, wid)?;
        w.buffer_id().unwrap_or(BufferId(0))
    };

    let new_wid = eval
        .frames
        .split_window(fid, wid, direction, buf_id)
        .ok_or_else(|| signal("error", vec![Value::string("Cannot split window")]))?;
    Ok(window_value(new_wid))
}

/// `(delete-window &optional WINDOW)` -> nil.
pub(crate) fn builtin_delete_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("delete-window", &args, 1)?;
    let (fid, wid) = resolve_window_id_or_error(eval, args.first())?;
    if !eval.frames.delete_window(fid, wid) {
        return Err(signal(
            "error",
            vec![Value::string("Cannot delete sole window")],
        ));
    }
    let selected_buffer = eval
        .frames
        .get(fid)
        .and_then(|frame| frame.find_window(frame.selected_window))
        .and_then(|w| w.buffer_id());
    if let Some(buffer_id) = selected_buffer {
        eval.buffers.set_current(buffer_id);
    }
    Ok(Value::Nil)
}

/// `(delete-other-windows &optional WINDOW)` -> nil.
///
/// Deletes all windows in the frame except WINDOW (or selected window).
pub(crate) fn builtin_delete_other_windows(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("delete-other-windows", &args, 2)?;
    let (fid, keep_wid) = resolve_window_id_or_error(eval, args.first())?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;

    let all_ids: Vec<WindowId> = frame.window_list();
    let to_delete: Vec<WindowId> = all_ids.into_iter().filter(|&w| w != keep_wid).collect();

    for wid in to_delete {
        let _ = eval.frames.delete_window(fid, wid);
    }
    // Select the kept window.
    let selected_buffer = if let Some(f) = eval.frames.get_mut(fid) {
        f.select_window(keep_wid);
        f.find_window(keep_wid).and_then(|w| w.buffer_id())
    } else {
        None
    };
    if let Some(buffer_id) = selected_buffer {
        eval.buffers.set_current(buffer_id);
    }
    Ok(Value::Nil)
}

/// `(delete-window-internal WINDOW)` -> nil.
///
/// GNU Emacs exposes this primitive for low-level window internals. For the
/// compatibility surface we mirror the observable error behavior used by the
/// vm-compat coverage corpus.
pub(crate) fn builtin_delete_window_internal(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("delete-window-internal", &args, 1)?;

    let (fid, wid) = if args[0].is_nil() {
        resolve_window_id(eval, None)?
    } else {
        resolve_window_id_with_pred(eval, args.first(), "windowp")?
    };

    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    let is_minibuffer = frame.minibuffer_window == Some(wid);
    let is_sole_ordinary_window = frame.window_list().len() <= 1;

    if is_minibuffer || is_sole_ordinary_window {
        return Err(signal(
            "error",
            vec![Value::string(
                "Attempt to delete minibuffer or sole ordinary window",
            )],
        ));
    }

    Err(signal("error", vec![Value::string("Deletion failed")]))
}

/// `(delete-other-windows-internal &optional WINDOW ALL-FRAMES)` -> nil.
///
/// Deletes all ordinary windows in FRAME except WINDOW. ALL-FRAMES is accepted
/// for arity compatibility and currently ignored.
pub(crate) fn builtin_delete_other_windows_internal(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("delete-other-windows-internal", &args, 2)?;
    let (fid, keep_wid) = resolve_window_id_with_pred(eval, args.first(), "window-valid-p")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;

    let all_ids: Vec<WindowId> = frame.window_list();
    let to_delete: Vec<WindowId> = all_ids.into_iter().filter(|&w| w != keep_wid).collect();

    for wid in to_delete {
        let _ = eval.frames.delete_window(fid, wid);
    }
    let selected_buffer = if let Some(f) = eval.frames.get_mut(fid) {
        f.select_window(keep_wid);
        f.find_window(keep_wid).and_then(|w| w.buffer_id())
    } else {
        None
    };
    if let Some(buffer_id) = selected_buffer {
        eval.buffers.set_current(buffer_id);
    }
    Ok(Value::Nil)
}

/// `(select-window WINDOW &optional NORECORD)` -> WINDOW.
pub(crate) fn builtin_select_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("select-window", &args, 1)?;
    expect_max_args("select-window", &args, 2)?;
    let fid = ensure_selected_frame_id(eval);
    let wid = match args.first().and_then(window_id_from_designator) {
        Some(wid) => wid,
        None => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), args[0]],
            ));
        }
    };
    let record_selection = args.get(1).is_none_or(Value::is_nil);
    let selected_buffer = {
        let frame = eval
            .frames
            .get_mut(fid)
            .ok_or_else(|| signal("error", vec![Value::string("No selected frame")]))?;
        if !frame.select_window(wid) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), args[0]],
            ));
        }
        frame.find_window(wid).and_then(|w| w.buffer_id())
    };
    if record_selection {
        let _ = eval.frames.note_window_selected(wid);
    }
    if let Some(buffer_id) = selected_buffer {
        eval.buffers.set_current(buffer_id);
    }
    Ok(window_value(wid))
}

/// `(other-window COUNT &optional ALL-FRAMES)` -> nil.
///
/// Select another window in cyclic order.
pub(crate) fn builtin_other_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("other-window", &args, 1)?;
    expect_max_args("other-window", &args, 3)?;
    let count = expect_number_or_marker_count(&args[0])?;
    let Some(fid) = eval.frames.selected_frame().map(|f| f.id) else {
        return Ok(Value::Nil);
    };
    let Some(frame) = eval.frames.get(fid) else {
        return Ok(Value::Nil);
    };
    let list = frame.window_list();
    if list.is_empty() {
        return Ok(Value::Nil);
    }
    let cur = frame.selected_window;
    let cur_idx = list.iter().position(|w| *w == cur).unwrap_or(0);
    let len = list.len() as i64;
    let new_idx = ((cur_idx as i64 + count) % len + len) % len;
    let new_wid = list[new_idx as usize];
    let (selected_buffer, switched) = if let Some(frame) = eval.frames.get_mut(fid) {
        let switched = frame.select_window(new_wid);
        (
            frame.find_window(new_wid).and_then(|w| w.buffer_id()),
            switched,
        )
    } else {
        (None, false)
    };
    if switched {
        let _ = eval.frames.note_window_selected(new_wid);
    };
    if let Some(buffer_id) = selected_buffer {
        eval.buffers.set_current(buffer_id);
    }
    Ok(Value::Nil)
}

/// `(other-window-for-scrolling)` -> window object used for scrolling.
pub(crate) fn builtin_other_window_for_scrolling(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("other-window-for-scrolling", &args, 0)?;
    let fid = ensure_selected_frame_id(eval);
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("No selected frame")]))?;
    let windows = frame.window_list();
    if windows.len() <= 1 {
        return Err(signal(
            "error",
            vec![Value::string("There is no other window")],
        ));
    }
    let selected = frame.selected_window;
    let other = windows
        .into_iter()
        .find(|wid| *wid != selected)
        .unwrap_or(selected);
    Ok(window_value(other))
}

/// `(next-window &optional WINDOW MINIBUF ALL-FRAMES)` -> window object.
pub(crate) fn builtin_next_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("next-window", &args, 3)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    let list = frame.window_list();
    if list.is_empty() {
        return Ok(Value::Nil);
    }
    let idx = list.iter().position(|w| *w == wid).unwrap_or(0);
    let next = (idx + 1) % list.len();
    Ok(window_value(list[next]))
}

/// `(previous-window &optional WINDOW MINIBUF ALL-FRAMES)` -> window object.
pub(crate) fn builtin_previous_window(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("previous-window", &args, 3)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    let list = frame.window_list();
    if list.is_empty() {
        return Ok(Value::Nil);
    }
    let idx = list.iter().position(|w| *w == wid).unwrap_or(0);
    let prev = if idx == 0 { list.len() - 1 } else { idx - 1 };
    Ok(window_value(list[prev]))
}

/// `(set-window-buffer WINDOW BUFFER-OR-NAME &optional KEEP-MARGINS)` -> nil.
pub(crate) fn builtin_set_window_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-window-buffer", &args, 2)?;
    expect_max_args("set-window-buffer", &args, 3)?;
    let (fid, wid) = resolve_window_id(eval, args.first())?;
    let buf_id = match &args[1] {
        Value::Buffer(id) => {
            if eval.buffers.get(*id).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Attempt to display deleted buffer")],
                ));
            }
            *id
        }
        Value::Str(_) => {
            let name_s = args[1].as_str().unwrap();
            match eval.buffers.find_buffer_by_name(name_s) {
                Some(id) => id,
                None => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("bufferp"), Value::Nil],
                    ));
                }
            }
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    let keep_margins = args.get(2).is_some_and(|arg| !arg.is_nil());
    let target_point = eval
        .buffers
        .get(buf_id)
        .map(|buf| buf.point_char().saturating_add(1))
        .unwrap_or(1)
        .max(1);

    let mut old_state = None;
    if let Some(Window::Leaf {
        buffer_id,
        window_start,
        point,
        ..
    }) = eval
        .frames
        .get_mut(fid)
        .and_then(|f| f.find_window_mut(wid))
    {
        old_state = Some((*buffer_id, *window_start, *point));
    }
    if let Some((old_buffer_id, old_window_start, old_point)) = old_state {
        eval.frames
            .set_window_buffer_position(wid, old_buffer_id, old_window_start, old_point);
        if old_buffer_id != buf_id {
            let prev_raw = eval.frames.window_prev_buffers(wid);
            let prev_entries = list_to_vec(&prev_raw).ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), prev_raw],
                )
            })?;
            let old_buffer_value = Value::Buffer(old_buffer_id);
            let marker_buffer_name = eval.buffers.get(old_buffer_id).map(|buf| buf.name.clone());
            let old_window_start_pos = old_window_start.max(1) as i64;
            let old_point_pos = old_point.max(1) as i64;
            let history_entry = Value::list(vec![
                old_buffer_value,
                super::marker::make_marker_value(
                    marker_buffer_name.as_deref(),
                    Some(old_window_start_pos),
                    false,
                ),
                super::marker::make_marker_value(
                    marker_buffer_name.as_deref(),
                    Some(old_point_pos),
                    false,
                ),
            ]);
            let filtered_prev = prev_entries
                .into_iter()
                .filter(|entry| {
                    let Some(items) = list_to_vec(entry) else {
                        return true;
                    };
                    !matches!(items.first(), Some(first) if *first == old_buffer_value)
                })
                .collect::<Vec<_>>();
            let mut next_prev = Vec::with_capacity(filtered_prev.len() + 1);
            next_prev.push(history_entry);
            next_prev.extend(filtered_prev);
            eval.frames
                .set_window_prev_buffers(wid, Value::list(next_prev));
            eval.frames.set_window_next_buffers(wid, Value::Nil);
        }
    }

    let (next_window_start, next_point) = eval
        .frames
        .window_buffer_position(wid, buf_id)
        .unwrap_or((1, target_point));
    if let Some(Window::Leaf {
        buffer_id,
        window_start,
        point,
        margins,
        ..
    }) = eval
        .frames
        .get_mut(fid)
        .and_then(|f| f.find_window_mut(wid))
    {
        *buffer_id = buf_id;
        *window_start = next_window_start.max(1);
        *point = next_point.max(1);
        if !keep_margins {
            *margins = (0, 0);
        }
    }
    Ok(Value::Nil)
}

/// `(switch-to-buffer BUFFER-OR-NAME &optional NORECORD FORCE-SAME-WINDOW)` -> buffer.
pub(crate) fn builtin_switch_to_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("switch-to-buffer", &args, 1)?;
    expect_max_args("switch-to-buffer", &args, 3)?;
    let buf_id = match &args[0] {
        Value::Buffer(id) => {
            if eval.buffers.get(*id).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Attempt to display deleted buffer")],
                ));
            }
            *id
        }
        Value::Str(_) => {
            let name_s = args[0].as_str().unwrap();
            match eval.buffers.find_buffer_by_name(name_s) {
                Some(id) => id,
                None => eval.buffers.create_buffer(name_s),
            }
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    // Set the selected window's buffer.
    let fid = ensure_selected_frame_id(eval);
    let sel_wid = eval
        .frames
        .get(fid)
        .map(|f| f.selected_window)
        .ok_or_else(|| signal("error", vec![Value::string("No selected window")]))?;
    if let Some(w) = eval
        .frames
        .get_mut(fid)
        .and_then(|f| f.find_window_mut(sel_wid))
    {
        w.set_buffer(buf_id);
    }
    // Also switch the buffer manager's current buffer.
    eval.buffers.set_current(buf_id);
    Ok(Value::Buffer(buf_id))
}

/// `(display-buffer BUFFER-OR-NAME &optional ACTION FRAME)` -> window object or nil.
///
/// Simplified: displays the buffer in the selected window.
pub(crate) fn builtin_display_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("display-buffer", &args, 1)?;
    expect_max_args("display-buffer", &args, 3)?;
    let buf_id = match &args[0] {
        Value::Buffer(id) => {
            if eval.buffers.get(*id).is_none() {
                return Err(signal("error", vec![Value::string("Invalid buffer")]));
            }
            *id
        }
        Value::Str(_) => {
            let name_s = args[0].as_str().unwrap();
            match eval.buffers.find_buffer_by_name(name_s) {
                Some(id) => id,
                None => return Err(signal("error", vec![Value::string("Invalid buffer")])),
            }
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    let fid = ensure_selected_frame_id(eval);
    let sel_wid = eval
        .frames
        .get(fid)
        .map(|f| f.selected_window)
        .ok_or_else(|| signal("error", vec![Value::string("No selected window")]))?;
    if let Some(w) = eval
        .frames
        .get_mut(fid)
        .and_then(|f| f.find_window_mut(sel_wid))
    {
        w.set_buffer(buf_id);
    }
    Ok(window_value(sel_wid))
}

/// `(pop-to-buffer BUFFER-OR-NAME &optional ACTION NORECORD)` -> buffer.
///
/// Batch compatibility follows Emacs' noninteractive behavior: switch current
/// buffer and return the buffer object.
pub(crate) fn builtin_pop_to_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("pop-to-buffer", &args, 1)?;
    expect_max_args("pop-to-buffer", &args, 3)?;
    let buf_id = match &args[0] {
        Value::Buffer(id) => {
            if eval.buffers.get(*id).is_none() {
                return Err(signal("error", vec![Value::string("Invalid buffer")]));
            }
            *id
        }
        Value::Str(_) => {
            let name_s = args[0].as_str().unwrap();
            match eval.buffers.find_buffer_by_name(name_s) {
                Some(id) => id,
                None => eval.buffers.create_buffer(name_s),
            }
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    let fid = ensure_selected_frame_id(eval);
    let sel_wid = eval
        .frames
        .get(fid)
        .map(|f| f.selected_window)
        .ok_or_else(|| signal("error", vec![Value::string("No selected window")]))?;
    if let Some(w) = eval
        .frames
        .get_mut(fid)
        .and_then(|f| f.find_window_mut(sel_wid))
    {
        w.set_buffer(buf_id);
    }
    eval.buffers.set_current(buf_id);
    Ok(Value::Buffer(buf_id))
}

const MIN_FRAME_COLS: i64 = 10;
const MIN_FRAME_TEXT_LINES: i64 = 5;
const FRAME_TEXT_LINES_PARAM: &str = "neovm--frame-text-lines";

fn frame_total_cols(frame: &crate::window::Frame) -> i64 {
    frame
        .parameters
        .get("width")
        .and_then(Value::as_int)
        .unwrap_or(frame.columns() as i64)
}

fn frame_total_lines(frame: &crate::window::Frame) -> i64 {
    frame
        .parameters
        .get("height")
        .and_then(Value::as_int)
        .unwrap_or(frame.lines() as i64)
}

fn frame_text_lines(frame: &crate::window::Frame) -> i64 {
    frame
        .parameters
        .get(FRAME_TEXT_LINES_PARAM)
        .and_then(Value::as_int)
        .unwrap_or_else(|| frame_total_lines(frame))
}

fn clamp_frame_dimension(value: i64, minimum: i64) -> i64 {
    value.max(minimum).min(u32::MAX as i64)
}

fn set_frame_text_size(frame: &mut crate::window::Frame, cols: i64, text_lines: i64) {
    let cols = clamp_frame_dimension(cols, MIN_FRAME_COLS);
    let text_lines = clamp_frame_dimension(text_lines, MIN_FRAME_TEXT_LINES);
    let total_lines = text_lines.saturating_add(1).min(u32::MAX as i64);

    frame.width = cols as u32;
    frame.height = total_lines as u32;
    frame
        .parameters
        .insert("width".to_string(), Value::Int(cols));
    frame
        .parameters
        .insert("height".to_string(), Value::Int(total_lines));
    frame
        .parameters
        .insert(FRAME_TEXT_LINES_PARAM.to_string(), Value::Int(text_lines));
}

// ===========================================================================
// Scroll / frame visibility command shims
// ===========================================================================

fn recenter_missing_display_error() -> Flow {
    signal(
        "error",
        vec![Value::string(
            "‘recenter’ing a window that does not display current-buffer.",
        )],
    )
}

fn scroll_up_batch_error() -> Flow {
    signal("end-of-buffer", vec![])
}

fn scroll_down_batch_error() -> Flow {
    signal("beginning-of-buffer", vec![])
}

/// `(scroll-up-command &optional ARG)` — delegates to scroll-up.
pub(crate) fn builtin_scroll_up_command(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("scroll-up-command", &args, 1)?;
    builtin_scroll_up(eval, args)
}

/// `(scroll-down-command &optional ARG)` — delegates to scroll-down.
pub(crate) fn builtin_scroll_down_command(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("scroll-down-command", &args, 1)?;
    builtin_scroll_down(eval, args)
}

/// Compute scroll distance: if ARG is nil, use window height minus
/// next-screen-context-lines; otherwise use ARG as line count.
fn scroll_lines(eval: &mut super::eval::Evaluator, arg: Option<&Value>, direction: i64) -> i64 {
    if let Some(v) = arg {
        if !v.is_nil() {
            // Explicit line count.
            let n = match v {
                Value::Int(n) => *n,
                _ => 1,
            };
            return n * direction;
        }
    }
    // nil or absent: full window minus context lines.
    let wh = builtin_window_body_height(eval, vec![])
        .ok()
        .and_then(|v| match v {
            Value::Int(n) => Some(n),
            _ => None,
        })
        .unwrap_or(24);
    let ctx = eval
        .obarray
        .symbol_value("next-screen-context-lines")
        .and_then(|v| match v {
            Value::Int(n) => Some(*n),
            _ => None,
        })
        .unwrap_or(2);
    (wh - ctx).max(1) * direction
}

/// `(scroll-up &optional ARG)` — scroll text upward (forward in buffer).
///
/// Mirror GNU Emacs Fscroll_up (window.c): move point forward by ARG lines
/// (or a windowful if nil).  Signals end-of-buffer if already at end.
pub(crate) fn builtin_scroll_up(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_max_args("scroll-up", &args, 1)?;
    let arg = args.first().cloned();
    let lines = scroll_lines(eval, arg.as_ref(), 1);
    scroll_by_lines(eval, lines)
}

/// `(scroll-down &optional ARG)` — scroll text downward (backward in buffer).
///
/// Mirror GNU Emacs Fscroll_down (window.c): move point backward by ARG lines
/// (or a windowful if nil).  Signals beginning-of-buffer if already at start.
pub(crate) fn builtin_scroll_down(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("scroll-down", &args, 1)?;
    let arg = args.first().cloned();
    let lines = scroll_lines(eval, arg.as_ref(), -1);
    scroll_by_lines(eval, lines)
}

/// Move point by `lines` newlines (positive=forward, negative=backward).
/// Signals end-of-buffer or beginning-of-buffer on boundary.
fn scroll_by_lines(eval: &mut super::eval::Evaluator, lines: i64) -> EvalResult {
    let Some(current_id) = eval.buffers.current_buffer_id() else {
        return Ok(Value::Nil);
    };
    let Some(buf) = eval.buffers.get(current_id) else {
        return Ok(Value::Nil);
    };
    let text = buf.text.to_string();
    let pt = buf.pt.clamp(buf.begv, buf.zv);
    let bytes = text.as_bytes();
    let begv = buf.begv;
    let zv = buf.zv;

    let mut pos = pt;

    if lines > 0 {
        if pt >= zv {
            return Err(scroll_up_batch_error());
        }
        for _ in 0..lines {
            while pos < zv && bytes[pos] != b'\n' {
                pos += 1;
            }
            if pos < zv {
                pos += 1; // past newline
            }
        }
    } else {
        if pt <= begv {
            return Err(scroll_down_batch_error());
        }
        let target = (-lines) as usize;
        // First go to beginning of current line.
        while pos > begv && bytes[pos - 1] != b'\n' {
            pos -= 1;
        }
        for _ in 0..target {
            if pos <= begv {
                break;
            }
            pos -= 1; // before newline
            while pos > begv && bytes[pos - 1] != b'\n' {
                pos -= 1;
            }
        }
    }

    let _ = eval.buffers.goto_buffer_byte(current_id, pos);
    Ok(Value::Nil)
}

/// `(recenter-top-bottom &optional ARG)` — delegates to recenter.
pub(crate) fn builtin_recenter_top_bottom(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("recenter-top-bottom", &args, 1)?;
    builtin_recenter(eval, args)
}

/// `(recenter &optional ARG REDISPLAY)` — center point in window.
///
/// Mirror GNU Emacs Frecenter (window.c): adjust window-start so that
/// point appears at the center of the window, or at line ARG from the
/// top (or bottom if ARG is negative).
pub(crate) fn builtin_recenter(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_max_args("recenter", &args, 2)?;

    let wh = builtin_window_body_height(eval, vec![])
        .ok()
        .and_then(|v| match v {
            Value::Int(n) => Some(n),
            _ => None,
        })
        .unwrap_or(24);

    // Determine target line from top of window where point should appear.
    let target_line = match args.first() {
        Some(Value::Int(n)) => {
            if *n >= 0 {
                *n
            } else {
                // Negative: count from bottom.
                (wh + *n).max(0)
            }
        }
        Some(v) if !v.is_nil() => wh / 2, // non-integer truthy = center
        _ => wh / 2,                      // nil or absent = center
    };

    // Compute new window-start by moving backward target_line lines from point.
    let Some(current_id) = eval.buffers.current_buffer_id() else {
        return Ok(Value::Nil);
    };
    let Some(buf) = eval.buffers.get(current_id) else {
        return Ok(Value::Nil);
    };
    let text = buf.text.to_string();
    let pt = buf.pt.clamp(buf.begv, buf.zv);
    let bytes = text.as_bytes();
    let begv = buf.begv;

    // Go to beginning of current line.
    let mut pos = pt;
    while pos > begv && bytes[pos - 1] != b'\n' {
        pos -= 1;
    }
    // Move backward target_line lines.
    for _ in 0..target_line {
        if pos <= begv {
            break;
        }
        pos -= 1;
        while pos > begv && bytes[pos - 1] != b'\n' {
            pos -= 1;
        }
    }

    // Set window-start.
    let _ = ensure_selected_frame_id(eval);
    if let Ok((fid, wid)) = resolve_window_id(eval, None) {
        if let Some(clamped) = clamped_window_position(eval, fid, wid, pos as i64) {
            if let Some(Window::Leaf { window_start, .. }) = eval
                .frames
                .get_mut(fid)
                .and_then(|frame| frame.find_window_mut(wid))
            {
                *window_start = clamped;
            }
        }
    }

    Ok(Value::Nil)
}

/// `(iconify-frame &optional FRAME)` -> nil.
pub(crate) fn builtin_iconify_frame(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("iconify-frame", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "frame-live-p")?;
    let frame = eval
        .frames
        .get_mut(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    frame.visible = false;
    Ok(Value::Nil)
}

/// `(make-frame-visible &optional FRAME)` -> frame.
pub(crate) fn builtin_make_frame_visible(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("make-frame-visible", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "frame-live-p")?;
    let frame = eval
        .frames
        .get_mut(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    frame.visible = true;
    Ok(Value::Frame(frame.id.0))
}

// ===========================================================================
// Frame operations
// ===========================================================================

/// `(selected-frame)` -> frame object.
pub(crate) fn builtin_selected_frame(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("selected-frame", &args, 0)?;
    let fid = ensure_selected_frame_id(eval);
    Ok(Value::Frame(fid.0))
}

/// `(select-frame FRAME &optional NORECORD)` -> frame.
pub(crate) fn builtin_select_frame(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("select-frame", &args, 1)?;
    expect_max_args("select-frame", &args, 2)?;
    let fid = match &args[0] {
        Value::Int(n) => {
            let fid = FrameId(*n as u64);
            if eval.frames.get(fid).is_none() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("frame-live-p"), Value::Int(*n)],
                ));
            }
            fid
        }
        Value::Frame(id) => {
            let fid = FrameId(*id);
            if eval.frames.get(fid).is_none() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("frame-live-p"), Value::Frame(*id)],
                ));
            }
            fid
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *other],
            ));
        }
    };
    if !eval.frames.select_frame(fid) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), args[0]],
        ));
    }
    if args.get(1).is_none_or(Value::is_nil) {
        if let Some(selected_wid) = eval.frames.get(fid).map(|f| f.selected_window) {
            let _ = eval.frames.note_window_selected(selected_wid);
        }
    }
    if let Some(buf_id) = eval
        .frames
        .get(fid)
        .and_then(|f| f.find_window(f.selected_window))
        .and_then(|w| w.buffer_id())
    {
        eval.buffers.set_current(buf_id);
    }
    Ok(Value::Frame(fid.0))
}

/// `(select-frame-set-input-focus FRAME &optional NORECORD)` -> nil.
pub(crate) fn builtin_select_frame_set_input_focus(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("select-frame-set-input-focus", &args, 1)?;
    expect_max_args("select-frame-set-input-focus", &args, 2)?;
    let fid = match &args[0] {
        Value::Int(n) => {
            let fid = FrameId(*n as u64);
            if eval.frames.get(fid).is_none() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("frame-live-p"), Value::Int(*n)],
                ));
            }
            fid
        }
        Value::Frame(id) => {
            let fid = FrameId(*id);
            if eval.frames.get(fid).is_none() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("frame-live-p"), Value::Frame(*id)],
                ));
            }
            fid
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *other],
            ));
        }
    };
    if !eval.frames.select_frame(fid) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), args[0]],
        ));
    }
    if args.get(1).is_none_or(Value::is_nil) {
        if let Some(selected_wid) = eval.frames.get(fid).map(|f| f.selected_window) {
            let _ = eval.frames.note_window_selected(selected_wid);
        }
    }
    if let Some(buf_id) = eval
        .frames
        .get(fid)
        .and_then(|f| f.find_window(f.selected_window))
        .and_then(|w| w.buffer_id())
    {
        eval.buffers.set_current(buf_id);
    }
    Ok(Value::Nil)
}

/// `(frame-list)` -> list of frame objects.
pub(crate) fn builtin_frame_list(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("frame-list", &args, 0)?;
    let _ = ensure_selected_frame_id(eval);
    let ids: Vec<Value> = eval
        .frames
        .frame_list()
        .into_iter()
        .map(|fid| Value::Frame(fid.0))
        .collect();
    Ok(Value::list(ids))
}

/// `(visible-frame-list)` -> list of visible frame objects.
pub(crate) fn builtin_visible_frame_list(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("visible-frame-list", &args, 0)?;
    let _ = ensure_selected_frame_id(eval);
    let mut frame_ids = eval.frames.frame_list();
    frame_ids.sort_by_key(|fid| fid.0);
    let visible = frame_ids
        .into_iter()
        .filter(|fid| eval.frames.get(*fid).is_some_and(|frame| frame.visible))
        .map(|fid| Value::Frame(fid.0))
        .collect::<Vec<_>>();
    Ok(Value::list(visible))
}

/// `(frame-char-height &optional FRAME)` -> integer.
pub(crate) fn builtin_frame_char_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-char-height", &args, 1)?;
    let _ = resolve_frame_id(eval, args.first(), "framep")?;
    Ok(Value::Int(1))
}

/// `(frame-char-width &optional FRAME)` -> integer.
pub(crate) fn builtin_frame_char_width(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-char-width", &args, 1)?;
    let _ = resolve_frame_id(eval, args.first(), "framep")?;
    Ok(Value::Int(1))
}

/// `(frame-native-height &optional FRAME)` -> integer.
pub(crate) fn builtin_frame_native_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-native-height", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "framep")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    Ok(Value::Int(frame_total_lines(frame)))
}

/// `(frame-native-width &optional FRAME)` -> integer.
pub(crate) fn builtin_frame_native_width(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-native-width", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "framep")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    Ok(Value::Int(frame_total_cols(frame)))
}

/// `(frame-text-cols &optional FRAME)` -> integer.
pub(crate) fn builtin_frame_text_cols(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-text-cols", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "framep")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    Ok(Value::Int(frame_total_cols(frame)))
}

/// `(frame-text-lines &optional FRAME)` -> integer.
pub(crate) fn builtin_frame_text_lines(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-text-lines", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "framep")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    Ok(Value::Int(frame_text_lines(frame)))
}

/// `(frame-text-width &optional FRAME)` -> integer.
pub(crate) fn builtin_frame_text_width(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_frame_text_cols(eval, args)
}

/// `(frame-text-height &optional FRAME)` -> integer.
pub(crate) fn builtin_frame_text_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_frame_text_lines(eval, args)
}

/// `(frame-total-cols &optional FRAME)` -> integer.
pub(crate) fn builtin_frame_total_cols(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-total-cols", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "framep")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    Ok(Value::Int(frame_total_cols(frame)))
}

/// `(frame-total-lines &optional FRAME)` -> integer.
pub(crate) fn builtin_frame_total_lines(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-total-lines", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "framep")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    Ok(Value::Int(frame_total_lines(frame)))
}

/// `(frame-position &optional FRAME)` -> (X . Y).
pub(crate) fn builtin_frame_position(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-position", &args, 1)?;
    let _ = resolve_frame_id(eval, args.first(), "frame-live-p")?;
    Ok(Value::cons(Value::Int(0), Value::Int(0)))
}

/// `(set-frame-height FRAME HEIGHT &optional PRETEND PIXELWISE)` -> nil.
pub(crate) fn builtin_set_frame_height(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-frame-height", &args, 2)?;
    expect_max_args("set-frame-height", &args, 4)?;
    let fid = resolve_frame_id(eval, Some(&args[0]), "frame-live-p")?;
    let text_lines = expect_int(&args[1])?;

    let cols = {
        let frame = eval
            .frames
            .get(fid)
            .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
        frame_total_cols(frame)
    };
    let frame = eval
        .frames
        .get_mut(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    set_frame_text_size(frame, cols, text_lines);
    Ok(Value::Nil)
}

/// `(set-frame-width FRAME WIDTH &optional PRETEND PIXELWISE)` -> nil.
pub(crate) fn builtin_set_frame_width(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-frame-width", &args, 2)?;
    expect_max_args("set-frame-width", &args, 4)?;
    let fid = resolve_frame_id(eval, Some(&args[0]), "frame-live-p")?;
    let cols = expect_int(&args[1])?;

    let text_lines = {
        let frame = eval
            .frames
            .get(fid)
            .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
        frame_text_lines(frame)
    };
    let frame = eval
        .frames
        .get_mut(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    set_frame_text_size(frame, cols, text_lines);
    Ok(Value::Nil)
}

/// `(set-frame-size FRAME WIDTH HEIGHT &optional PIXELWISE)` -> nil.
pub(crate) fn builtin_set_frame_size(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-frame-size", &args, 3)?;
    expect_max_args("set-frame-size", &args, 4)?;
    let fid = resolve_frame_id(eval, Some(&args[0]), "frame-live-p")?;
    let cols = expect_int(&args[1])?;
    let text_lines = expect_int(&args[2])?;

    let frame = eval
        .frames
        .get_mut(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    set_frame_text_size(frame, cols, text_lines);
    Ok(Value::Nil)
}

/// `(set-frame-position FRAME X Y)` -> t.
pub(crate) fn builtin_set_frame_position(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-frame-position", &args, 3)?;
    let _ = resolve_frame_id(eval, Some(&args[0]), "frame-live-p")?;
    let _ = expect_int(&args[1])?;
    let _ = expect_int(&args[2])?;
    Ok(Value::True)
}

/// `(make-frame &optional PARAMETERS)` -> frame id.
///
/// Creates a new frame.  PARAMETERS is an alist; we currently
/// only honour `width`, `height`, and `name`.
pub(crate) fn builtin_make_frame(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("make-frame", &args, 1)?;
    let mut width: u32 = 800;
    let mut height: u32 = 600;
    let mut name = String::from("F");

    // Parse optional alist parameters.
    if let Some(params) = args.first() {
        if let Some(items) = super::value::list_to_vec(params) {
            for item in &items {
                if let Value::Cons(cell) = item {
                    let pair = read_cons(*cell);
                    if let Value::Symbol(key) = &pair.car {
                        match resolve_sym(*key) {
                            "width" => {
                                if let Some(n) = pair.cdr.as_int() {
                                    width = n as u32;
                                }
                            }
                            "height" => {
                                if let Some(n) = pair.cdr.as_int() {
                                    height = n as u32;
                                }
                            }
                            "name" => {
                                if let Some(s) = pair.cdr.as_str() {
                                    name = s.to_string();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Use the current buffer (or BufferId(0) as fallback) for the initial window.
    let buf_id = eval
        .buffers
        .current_buffer()
        .map(|b| b.id)
        .unwrap_or(BufferId(0));
    let fid = eval.frames.create_frame(&name, width, height, buf_id);
    Ok(Value::Frame(fid.0))
}

/// `(delete-frame &optional FRAME FORCE)` -> nil.
pub(crate) fn builtin_delete_frame(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("delete-frame", &args, 2)?;
    let fid = resolve_frame_id(eval, args.first(), "framep")?;
    if !eval.frames.delete_frame(fid) {
        return Err(signal("error", vec![Value::string("Cannot delete frame")]));
    }
    Ok(Value::Nil)
}

/// `(frame-parameter FRAME PARAMETER)` -> value or nil.
pub(crate) fn builtin_frame_parameter(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("frame-parameter", &args, 2)?;
    expect_max_args("frame-parameter", &args, 2)?;
    let fid = resolve_frame_id(eval, Some(&args[0]), "framep")?;
    let param_name = match &args[1] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        _ => return Ok(Value::Nil),
    };
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;

    // Check built-in properties first.
    match param_name.as_str() {
        "name" => return Ok(Value::string(frame.name.clone())),
        "title" => return Ok(Value::string(frame.title.clone())),
        // In Emacs, frame parameter width/height are text columns/lines.
        // For the bootstrap batch frame, explicit parameter overrides preserve
        // the 80x25 report shape.
        "width" => {
            return Ok(frame
                .parameters
                .get("width")
                .cloned()
                .unwrap_or(Value::Int(frame.columns() as i64)));
        }
        "height" => {
            return Ok(frame
                .parameters
                .get("height")
                .cloned()
                .unwrap_or(Value::Int(frame.lines() as i64)));
        }
        "visibility" => {
            return Ok(if frame.visible {
                Value::True
            } else {
                Value::Nil
            });
        }
        _ => {}
    }
    // User-set parameters.
    Ok(frame
        .parameters
        .get(&param_name)
        .cloned()
        .unwrap_or(Value::Nil))
}

/// `(frame-parameters &optional FRAME)` -> alist.
pub(crate) fn builtin_frame_parameters(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-parameters", &args, 1)?;
    let fid = resolve_frame_id(eval, args.first(), "framep")?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    let mut pairs: Vec<Value> = Vec::new();
    // Built-in parameters.
    pairs.push(Value::cons(
        Value::symbol("name"),
        Value::string(frame.name.clone()),
    ));
    pairs.push(Value::cons(
        Value::symbol("title"),
        Value::string(frame.title.clone()),
    ));
    let width = frame
        .parameters
        .get("width")
        .cloned()
        .unwrap_or(Value::Int(frame.columns() as i64));
    let height = frame
        .parameters
        .get("height")
        .cloned()
        .unwrap_or(Value::Int(frame.lines() as i64));
    pairs.push(Value::cons(Value::symbol("width"), width));
    pairs.push(Value::cons(Value::symbol("height"), height));
    pairs.push(Value::cons(
        Value::symbol("visibility"),
        Value::bool(frame.visible),
    ));
    // User parameters.
    for (k, v) in &frame.parameters {
        pairs.push(Value::cons(Value::symbol(k.clone()), *v));
    }
    Ok(Value::list(pairs))
}

/// `(modify-frame-parameters FRAME ALIST)` -> nil.
pub(crate) fn builtin_modify_frame_parameters(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("modify-frame-parameters", &args, 2)?;
    expect_max_args("modify-frame-parameters", &args, 2)?;
    let fid = resolve_frame_id(eval, Some(&args[0]), "frame-live-p")?;
    let items = super::value::list_to_vec(&args[1]).unwrap_or_default();

    let frame = eval
        .frames
        .get_mut(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;

    for item in items {
        if let Value::Cons(cell) = &item {
            let pair = read_cons(*cell);
            if let Value::Symbol(key) = &pair.car {
                match resolve_sym(*key) {
                    "name" => {
                        if let Some(s) = pair.cdr.as_str() {
                            frame.name = s.to_string();
                        }
                    }
                    "title" => {
                        if let Some(s) = pair.cdr.as_str() {
                            frame.title = s.to_string();
                        }
                    }
                    "width" => {
                        if let Some(n) = pair.cdr.as_int() {
                            frame.width = n as u32;
                        }
                    }
                    "height" => {
                        if let Some(n) = pair.cdr.as_int() {
                            frame.height = n as u32;
                        }
                    }
                    "visibility" => {
                        frame.visible = pair.cdr.is_truthy();
                    }
                    _ => {
                        frame
                            .parameters
                            .insert(resolve_sym(*key).to_owned(), pair.cdr);
                    }
                }
            }
        }
    }
    Ok(Value::Nil)
}

/// `(frame-visible-p FRAME)` -> t or nil.
pub(crate) fn builtin_frame_visible_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("frame-visible-p", &args, 1)?;
    let fid = match args.first() {
        Some(Value::Int(n)) => FrameId(*n as u64),
        Some(Value::Frame(id)) => FrameId(*id),
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *other],
            ));
        }
        None => unreachable!("expect_args enforced"),
    };
    let frame = eval.frames.get(fid).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), args[0]],
        )
    })?;
    Ok(Value::bool(frame.visible))
}

/// `(framep OBJ)` -> t if OBJ is a frame object or frame id that exists.
pub(crate) fn builtin_framep(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("framep", &args, 1)?;
    let id = match &args[0] {
        Value::Frame(id) => *id,
        Value::Int(n) => *n as u64,
        _ => return Ok(Value::Nil),
    };
    let Some(frame) = eval.frames.get(FrameId(id)) else {
        return Ok(Value::Nil);
    };
    Ok(frame
        .parameters
        .get("window-system")
        .copied()
        .unwrap_or(Value::True))
}

/// `(frame-live-p OBJ)` -> t if OBJ is a live frame object or frame id.
pub(crate) fn builtin_frame_live_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("frame-live-p", &args, 1)?;
    let id = match &args[0] {
        Value::Frame(id) => *id,
        Value::Int(n) => *n as u64,
        _ => return Ok(Value::Nil),
    };
    Ok(Value::bool(eval.frames.get(FrameId(id)).is_some()))
}

// ===========================================================================
// Bootstrap variables
// ===========================================================================

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    use crate::emacs_core::value::Value;

    // window.c:9483 — DEFVAR_LISP
    obarray.set_symbol_value(
        "window-persistent-parameters",
        Value::list(vec![Value::cons(Value::symbol("clone-of"), Value::True)]),
    );
    obarray.set_symbol_value("recenter-redisplay", Value::symbol("tty"));
    obarray.set_symbol_value("window-combination-resize", Value::Nil);
    obarray.set_symbol_value("window-combination-limit", Value::Nil);
    obarray.set_symbol_value("window-sides-vertical", Value::Nil);
    obarray.set_symbol_value("window-sides-slots", Value::Nil);
    obarray.set_symbol_value("window-resize-pixelwise", Value::Nil);
    obarray.set_symbol_value("fit-window-to-buffer-horizontally", Value::Nil);
    obarray.set_symbol_value("fit-frame-to-buffer", Value::Nil);
    obarray.set_symbol_value(
        "fit-frame-to-buffer-margins",
        Value::list(vec![
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
        ]),
    );
    obarray.set_symbol_value("fit-frame-to-buffer-sizes", Value::Nil);
    obarray.set_symbol_value("window-min-height", Value::Int(4));
    obarray.set_symbol_value("window-min-width", Value::Int(10));
    obarray.set_symbol_value("window-safe-min-height", Value::Int(1));
    obarray.set_symbol_value("window-safe-min-width", Value::Int(2));
    obarray.set_symbol_value("scroll-preserve-screen-position", Value::Nil);
    obarray.set_symbol_value("next-screen-context-lines", Value::Int(2));
    obarray.set_symbol_value("fast-but-imprecise-scrolling", Value::Nil);
    obarray.set_symbol_value("scroll-error-top-bottom", Value::Nil);
    obarray.set_symbol_value(
        "temp-buffer-max-height",
        Value::Float(1.0 / 3.0, next_float_id()), // (/ (frame-height) 3) approximation
    );
    obarray.set_symbol_value("temp-buffer-max-width", Value::Nil);
    obarray.set_symbol_value("even-window-sizes", Value::symbol("width-only"));
    obarray.set_symbol_value("auto-window-vscroll", Value::True);
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "window_cmds_test.rs"]
mod tests;
