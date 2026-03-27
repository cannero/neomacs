//! Dispnew builtins extracted from display.rs and builtins.rs.
//!
//! Provides cursor visibility state, window designator helpers,
//! and all dispnew-related builtins (redraw, ding, termscript,
//! send-string-to-terminal, internal-show-cursor, force-window-update).

use crate::emacs_core::display::{expect_frame_designator, live_frame_designator_p};
use crate::emacs_core::error::{EvalResult, Flow, signal};
use crate::emacs_core::terminal::pure::{
    expect_terminal_designator, expect_terminal_designator_eval,
    expect_terminal_designator_in_state,
};
use crate::emacs_core::value::*;
use crate::window::WindowId;
use std::cell::{Cell, RefCell};

// ---------------------------------------------------------------------------
// Thread-local cursor state
// ---------------------------------------------------------------------------

thread_local! {
    static CURSOR_VISIBLE_WINDOWS: RefCell<Vec<(u64, bool)>> = const { RefCell::new(Vec::new()) };
    static CURSOR_VISIBLE: Cell<bool> = const { Cell::new(true) };
}

/// Reset cursor visibility state (called from `reset_display_thread_locals`).
pub(crate) fn reset_dispnew_thread_locals() {
    CURSOR_VISIBLE_WINDOWS.with(|slot| slot.borrow_mut().clear());
    CURSOR_VISIBLE.with(|slot| slot.set(true));
}

// ---------------------------------------------------------------------------
// Argument helpers (local copies — originals are pub(crate) in display.rs)
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

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

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

// ---------------------------------------------------------------------------
// Window designator helpers
// ---------------------------------------------------------------------------

fn expect_window_designator(value: &Value) -> Result<(), Flow> {
    if value.is_nil() {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("windowp"), *value],
        ))
    }
}

fn live_window_designator_p(eval: &mut crate::emacs_core::eval::Context, value: &Value) -> bool {
    match value {
        Value::Window(id) => eval.frames.find_window_frame_id(WindowId(*id)).is_some(),
        Value::Int(id) if *id >= 0 => eval
            .frames
            .find_window_frame_id(WindowId(*id as u64))
            .is_some(),
        _ => false,
    }
}

fn expect_window_designator_eval(
    eval: &mut crate::emacs_core::eval::Context,
    value: &Value,
) -> Result<(), Flow> {
    if value.is_nil() || live_window_designator_p(eval, value) {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("windowp"), *value],
        ))
    }
}

fn live_window_designator_p_in_state(frames: &crate::window::FrameManager, value: &Value) -> bool {
    match value {
        Value::Window(id) => frames.find_window_frame_id(WindowId(*id)).is_some(),
        Value::Int(id) if *id >= 0 => frames.find_window_frame_id(WindowId(*id as u64)).is_some(),
        _ => false,
    }
}

fn expect_window_designator_in_state(
    frames: &crate::window::FrameManager,
    value: &Value,
) -> Result<(), Flow> {
    if value.is_nil() || live_window_designator_p_in_state(frames, value) {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("windowp"), *value],
        ))
    }
}

fn window_id_from_window_designator(value: &Value) -> Option<WindowId> {
    match value {
        Value::Window(id) => Some(WindowId(*id)),
        Value::Int(id) if *id >= 0 => Some(WindowId(*id as u64)),
        _ => None,
    }
}

fn selected_window_id(eval: &mut crate::emacs_core::eval::Context) -> Option<WindowId> {
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(eval);
    eval.frames.get(frame_id).map(|frame| frame.selected_window)
}

fn selected_window_id_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
) -> Option<WindowId> {
    let frame_id =
        crate::emacs_core::window_cmds::ensure_selected_frame_id_in_state(frames, buffers);
    frames.get(frame_id).map(|frame| frame.selected_window)
}

fn resolve_internal_show_cursor_window_id(
    eval: &mut crate::emacs_core::eval::Context,
    value: &Value,
) -> Option<WindowId> {
    if value.is_nil() {
        selected_window_id(eval)
    } else {
        window_id_from_window_designator(value)
    }
}

fn resolve_internal_show_cursor_window_id_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    value: &Value,
) -> Option<WindowId> {
    if value.is_nil() {
        selected_window_id_in_state(frames, buffers)
    } else {
        window_id_from_window_designator(value)
    }
}

fn set_window_cursor_visible(window_id: WindowId, visible: bool) {
    CURSOR_VISIBLE_WINDOWS.with(|slot| {
        let mut states = slot.borrow_mut();
        if let Some((_, existing)) = states
            .iter_mut()
            .find(|(stored_window_id, _)| *stored_window_id == window_id.0)
        {
            *existing = visible;
        } else {
            states.push((window_id.0, visible));
        }
    });
}

fn window_cursor_visible(window_id: WindowId) -> bool {
    CURSOR_VISIBLE_WINDOWS.with(|slot| {
        slot.borrow()
            .iter()
            .find_map(|(stored_window_id, visible)| {
                if *stored_window_id == window_id.0 {
                    Some(*visible)
                } else {
                    None
                }
            })
            .unwrap_or(true)
    })
}

// ---------------------------------------------------------------------------
// Dispnew builtins
// ---------------------------------------------------------------------------

/// (redraw-frame &optional FRAME) -> nil
pub(crate) fn builtin_redraw_frame_inner(args: Vec<Value>) -> EvalResult {
    expect_range_args("redraw-frame", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_designator(frame)?;
    }
    Ok(Value::Nil)
}

/// Context-aware variant of `redraw-frame`.
///
/// Accepts live frame designators in addition to nil.
pub(crate) fn builtin_redraw_frame(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("redraw-frame", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !live_frame_designator_p(eval, frame) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(Value::Nil)
}

/// (redraw-display) -> nil
pub(crate) fn builtin_redraw_display(args: Vec<Value>) -> EvalResult {
    expect_args("redraw-display", &args, 0)?;
    Ok(Value::Nil)
}

/// (open-termscript FILE) -> error
///
/// NeoVM does not support terminal script logging.
pub(crate) fn builtin_open_termscript(args: Vec<Value>) -> EvalResult {
    expect_args("open-termscript", &args, 1)?;
    Err(signal(
        "error",
        vec![Value::string("Current frame is not on a tty device")],
    ))
}

/// (ding &optional ARG) -> nil
pub(crate) fn builtin_ding(args: Vec<Value>) -> EvalResult {
    expect_range_args("ding", &args, 0, 1)?;
    Ok(Value::Nil)
}

/// (send-string-to-terminal STRING &optional TERMINAL) -> nil
pub(crate) fn builtin_send_string_to_terminal_inner(args: Vec<Value>) -> EvalResult {
    expect_range_args("send-string-to-terminal", &args, 1, 2)?;
    match &args[0] {
        Value::Str(_) => {
            if let Some(terminal) = args.get(1) {
                expect_terminal_designator(terminal)?;
            }
            Ok(Value::Nil)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

/// Context-aware variant of `send-string-to-terminal`.
///
/// Accepts live frame designators for the optional TERMINAL argument.
pub(crate) fn builtin_send_string_to_terminal(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("send-string-to-terminal", &args, 1, 2)?;
    match &args[0] {
        Value::Str(_) => {
            if let Some(terminal) = args.get(1) {
                expect_terminal_designator_eval(eval, terminal)?;
            }
            Ok(Value::Nil)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

/// (internal-show-cursor WINDOW SHOW) -> nil
pub(crate) fn builtin_internal_show_cursor_inner(args: Vec<Value>) -> EvalResult {
    expect_args("internal-show-cursor", &args, 2)?;
    expect_window_designator(&args[0])?;
    CURSOR_VISIBLE.with(|slot| slot.set(!args[1].is_nil()));
    Ok(Value::Nil)
}

/// Context-aware variant of `internal-show-cursor`.
///
/// Accepts live window designators in addition to nil.
pub(crate) fn builtin_internal_show_cursor(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal-show-cursor", &args, 2)?;
    expect_window_designator_eval(eval, &args[0])?;
    let visible = !args[1].is_nil();
    if let Some(window_id) = resolve_internal_show_cursor_window_id(eval, &args[0]) {
        set_window_cursor_visible(window_id, visible);
    } else {
        CURSOR_VISIBLE.with(|slot| slot.set(visible));
    }
    Ok(Value::Nil)
}

/// (internal-show-cursor-p &optional WINDOW) -> t/nil
pub(crate) fn builtin_internal_show_cursor_p_inner(args: Vec<Value>) -> EvalResult {
    expect_range_args("internal-show-cursor-p", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_designator(window)?;
    }
    Ok(Value::bool(CURSOR_VISIBLE.with(|slot| slot.get())))
}

/// Context-aware variant of `internal-show-cursor-p`.
///
/// Accepts live window designators in addition to nil.
pub(crate) fn builtin_internal_show_cursor_p(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("internal-show-cursor-p", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_designator_eval(eval, window)?;
    }
    let query_window = args.first().unwrap_or(&Value::Nil);
    if let Some(window_id) = resolve_internal_show_cursor_window_id(eval, query_window) {
        return Ok(Value::bool(window_cursor_visible(window_id)));
    }
    Ok(Value::bool(CURSOR_VISIBLE.with(|slot| slot.get())))
}

/// (force-window-update &optional OBJECT) -> t/nil
pub(crate) fn builtin_force_window_update(args: Vec<Value>) -> EvalResult {
    expect_max_args("force-window-update", &args, 1)?;
    if args.first().is_some_and(|v| !v.is_nil()) {
        Ok(Value::Nil)
    } else {
        Ok(Value::True)
    }
}

/// (frame--z-order-lessp A B) -> t/nil
///
/// Internal frame sorting predicate.  In NeoVM all frames have equal
/// z-order so this always returns nil.
pub(crate) fn builtin_frame_z_order_lessp(args: Vec<Value>) -> EvalResult {
    expect_args("frame--z-order-lessp", &args, 2)?;
    Ok(Value::Nil)
}
