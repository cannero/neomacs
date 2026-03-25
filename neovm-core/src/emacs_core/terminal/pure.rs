//! Terminal/TTY builtins extracted from display.rs and builtins.rs.
//!
//! Provides the singleton terminal handle, terminal parameter storage,
//! and all terminal/tty query builtins.  Neomacs is always a GUI
//! application so terminal-related queries return sensible defaults.

use crate::emacs_core::error::{EvalResult, Flow, signal};
use crate::emacs_core::value::*;
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Thread-local terminal state
// ---------------------------------------------------------------------------

thread_local! {
    static TERMINAL_PARAMS: RefCell<Vec<(Value, Value)>> = const { RefCell::new(Vec::new()) };
    static TERMINAL_HANDLE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static TERMINAL_RUNTIME: RefCell<TerminalRuntime> = const { RefCell::new(TerminalRuntime::inactive()) };
}

pub(crate) const TERMINAL_NAME: &str = "initial_terminal";
pub(crate) const TERMINAL_ID: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalRuntime {
    active: bool,
    tty_type: Option<String>,
    color_cells: i64,
    controlling_tty: bool,
}

impl TerminalRuntime {
    const fn inactive() -> Self {
        Self {
            active: false,
            tty_type: None,
            color_cells: 0,
            controlling_tty: false,
        }
    }

    fn supports_color(&self) -> bool {
        self.color_cells > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRuntimeConfig {
    pub tty_type: Option<String>,
    pub color_cells: i64,
    pub controlling_tty: bool,
}

impl TerminalRuntimeConfig {
    pub fn inactive() -> Self {
        Self {
            tty_type: None,
            color_cells: 0,
            controlling_tty: false,
        }
    }

    pub fn interactive(tty_type: Option<String>, color_cells: i64) -> Self {
        Self {
            tty_type,
            color_cells: color_cells.max(0),
            controlling_tty: true,
        }
    }
}

pub fn configure_terminal_runtime(config: TerminalRuntimeConfig) {
    TERMINAL_RUNTIME.with(|slot| {
        *slot.borrow_mut() = TerminalRuntime {
            active: config.controlling_tty || config.tty_type.is_some() || config.color_cells > 0,
            tty_type: config.tty_type,
            color_cells: config.color_cells.max(0),
            controlling_tty: config.controlling_tty,
        };
    });
}

pub fn reset_terminal_runtime() {
    TERMINAL_RUNTIME.with(|slot| *slot.borrow_mut() = TerminalRuntime::inactive());
}

fn terminal_runtime() -> TerminalRuntime {
    TERMINAL_RUNTIME.with(|slot| slot.borrow().clone())
}

pub(crate) fn terminal_runtime_active() -> bool {
    terminal_runtime().active
}

pub(crate) fn terminal_runtime_color_cells() -> i64 {
    terminal_runtime().color_cells
}

pub(crate) fn terminal_runtime_supports_color() -> bool {
    terminal_runtime().supports_color()
}

/// Clear cached terminal thread-locals (called from `reset_display_thread_locals`).
pub(crate) fn reset_terminal_thread_locals() {
    TERMINAL_PARAMS.with(|slot| slot.borrow_mut().clear());
    TERMINAL_HANDLE.with(|slot| *slot.borrow_mut() = None);
    reset_terminal_runtime();
}

/// Reset only the terminal handle (stale ObjId safety on heap reset).
/// Does NOT reset terminal params or runtime config.
pub(crate) fn reset_terminal_handle() {
    TERMINAL_HANDLE.with(|slot| *slot.borrow_mut() = None);
}

/// Collect GC roots from terminal thread-locals.
pub(crate) fn collect_terminal_gc_roots(roots: &mut Vec<Value>) {
    TERMINAL_PARAMS.with(|slot| {
        for (k, v) in slot.borrow().iter() {
            roots.push(*k);
            roots.push(*v);
        }
    });
    TERMINAL_HANDLE.with(|slot| {
        if let Some(v) = *slot.borrow() {
            roots.push(v);
        }
    });
}

// ---------------------------------------------------------------------------
// Terminal handle helpers
// ---------------------------------------------------------------------------

pub(crate) fn terminal_handle_value() -> Value {
    TERMINAL_HANDLE.with(|slot| {
        let mut borrow = slot.borrow_mut();
        if borrow.is_none() {
            *borrow = Some(Value::vector(vec![Value::symbol("--neovm-terminal--")]));
        }
        (*borrow).unwrap()
    })
}

pub(crate) fn is_terminal_handle(value: &Value) -> bool {
    match value {
        Value::Vector(v) => TERMINAL_HANDLE.with(|slot| {
            if let Some(Value::Vector(handle_id)) = slot.borrow().as_ref() {
                v == handle_id
            } else {
                false
            }
        }),
        _ => false,
    }
}

pub(crate) fn terminal_handle_id(value: &Value) -> Option<u64> {
    if is_terminal_handle(value) {
        Some(TERMINAL_ID)
    } else {
        None
    }
}

pub(crate) fn print_terminal_handle(value: &Value) -> Option<String> {
    terminal_handle_id(value).map(|id| format!("#<terminal {id} on {TERMINAL_NAME}>"))
}

// ---------------------------------------------------------------------------
// Terminal designator predicates
// ---------------------------------------------------------------------------

pub(crate) fn terminal_designator_p(value: &Value) -> bool {
    value.is_nil() || is_terminal_handle(value)
}

pub(crate) fn terminal_designator_eval_p(
    eval: &mut crate::emacs_core::eval::Context,
    value: &Value,
) -> bool {
    terminal_designator_p(value) || crate::emacs_core::display::live_frame_designator_p(eval, value)
}

pub(crate) fn terminal_designator_in_state_p(
    frames: &crate::window::FrameManager,
    value: &Value,
) -> bool {
    terminal_designator_p(value)
        || crate::emacs_core::display::live_frame_designator_p_in_state(frames, value)
}

pub(crate) fn expect_terminal_designator(value: &Value) -> Result<(), Flow> {
    if terminal_designator_p(value) {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), *value],
        ))
    }
}

pub(crate) fn expect_terminal_designator_eval(
    eval: &mut crate::emacs_core::eval::Context,
    value: &Value,
) -> Result<(), Flow> {
    if terminal_designator_eval_p(eval, value) {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), *value],
        ))
    }
}

pub(crate) fn expect_terminal_designator_in_state(
    frames: &crate::window::FrameManager,
    value: &Value,
) -> Result<(), Flow> {
    if terminal_designator_in_state_p(frames, value) {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), *value],
        ))
    }
}

// ---------------------------------------------------------------------------
// Terminal parameter helpers
// ---------------------------------------------------------------------------

fn terminal_parameter_default_value(key: &Value) -> Option<Value> {
    match key.as_symbol_name() {
        Some("normal-erase-is-backspace") => Some(Value::Int(0)),
        Some("keyboard-coding-saved-meta-mode") => Some(Value::list(vec![Value::True])),
        _ => None,
    }
}

fn terminal_parameter_default_entries() -> Vec<(Value, Value)> {
    vec![
        (Value::symbol("normal-erase-is-backspace"), Value::Int(0)),
        (
            Value::symbol("keyboard-coding-saved-meta-mode"),
            Value::list(vec![Value::True]),
        ),
    ]
}

fn lookup_terminal_parameter_value(params: &[(Value, Value)], key: &Value) -> Value {
    params
        .iter()
        .find_map(|(stored_key, stored_value)| {
            if eq_value(stored_key, key) {
                Some(*stored_value)
            } else {
                None
            }
        })
        .or_else(|| terminal_parameter_default_value(key))
        .unwrap_or(Value::Nil)
}

fn terminal_parameters_with_defaults(params: &[(Value, Value)]) -> Vec<(Value, Value)> {
    let mut merged = terminal_parameter_default_entries();
    for (key, value) in params {
        if let Some((_, existing_value)) = merged
            .iter_mut()
            .find(|(existing_key, _)| eq_value(existing_key, key))
        {
            *existing_value = *value;
        } else {
            merged.push((*key, *value));
        }
    }
    merged
}

fn expect_symbol_key(value: &Value) -> Result<Value, Flow> {
    match value {
        Value::Nil | Value::True | Value::Symbol(_) | Value::Keyword(_) => Ok(*value),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// Alist helper
// ---------------------------------------------------------------------------

pub(crate) fn make_alist(pairs: Vec<(Value, Value)>) -> Value {
    let entries: Vec<Value> = pairs.into_iter().map(|(k, v)| Value::cons(k, v)).collect();
    Value::list(entries)
}

// ---------------------------------------------------------------------------
// Argument helpers (local copies — identical to display.rs)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Terminal builtins
// ---------------------------------------------------------------------------

/// (terminal-name &optional TERMINAL) -> "initial_terminal"
pub(crate) fn builtin_terminal_name(args: Vec<Value>) -> EvalResult {
    expect_max_args("terminal-name", &args, 1)?;
    if let Some(term) = args.first() {
        if !term.is_nil() {
            expect_terminal_designator(term)?;
        }
    }
    Ok(Value::string(TERMINAL_NAME))
}

/// Context-aware variant of `terminal-name`.
///
/// Accepts live frame designators in addition to terminal designators.
pub(crate) fn builtin_terminal_name_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("terminal-name", &args, 1)?;
    if let Some(term) = args.first() {
        if !term.is_nil() {
            expect_terminal_designator_eval(eval, term)?;
        }
    }
    Ok(Value::string(TERMINAL_NAME))
}


/// (terminal-list) -> list containing one opaque terminal handle.
pub(crate) fn builtin_terminal_list(args: Vec<Value>) -> EvalResult {
    expect_max_args("terminal-list", &args, 0)?;
    Ok(Value::list(vec![terminal_handle_value()]))
}

/// (selected-terminal) -> currently selected terminal handle.
#[cfg(test)]
pub(crate) fn builtin_selected_terminal(args: Vec<Value>) -> EvalResult {
    expect_args("selected-terminal", &args, 0)?;
    Ok(terminal_handle_value())
}

/// (frame-terminal &optional FRAME) -> opaque terminal handle.
pub(crate) fn builtin_frame_terminal(args: Vec<Value>) -> EvalResult {
    expect_max_args("frame-terminal", &args, 1)?;
    if let Some(frame) = args.first() {
        crate::emacs_core::display::expect_frame_designator(frame)?;
    }
    Ok(terminal_handle_value())
}

/// Context-aware variant of `frame-terminal`.
///
/// Accepts live frame designators in addition to nil.
pub(crate) fn builtin_frame_terminal_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-terminal", &args, 1)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !crate::emacs_core::display::live_frame_designator_p(eval, frame) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(terminal_handle_value())
}


/// (terminal-live-p TERMINAL) -> t
pub(crate) fn builtin_terminal_live_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("terminal-live-p", &args, 1, 1)?;
    Ok(Value::bool(terminal_designator_p(&args[0])))
}

/// Context-aware variant of `terminal-live-p`.
///
/// In GNU Emacs, terminal-live-p returns the terminal type symbol
/// (e.g. 'x, 'w32) for GUI terminals, or t for TTY.  This is used
/// by framep-on-display to determine the window system type.
pub(crate) fn builtin_terminal_live_p_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("terminal-live-p", &args, 1, 1)?;
    if !terminal_designator_eval_p(eval, &args[0]) {
        return Ok(Value::Nil);
    }
    // Return the window system type so framep-on-display works correctly.
    if crate::emacs_core::display::x_window_system_active(eval) {
        Ok(Value::symbol(
            crate::emacs_core::display::gui_window_system_symbol(),
        ))
    } else {
        Ok(Value::True)
    }
}


/// (terminal-parameter TERMINAL PARAMETER) -> value
pub(crate) fn builtin_terminal_parameter(args: Vec<Value>) -> EvalResult {
    expect_args("terminal-parameter", &args, 2)?;
    expect_terminal_designator(&args[0])?;
    let key = expect_symbol_key(&args[1])?;
    TERMINAL_PARAMS.with(|slot| Ok(lookup_terminal_parameter_value(&slot.borrow(), &key)))
}

/// Context-aware variant of `terminal-parameter`.
///
/// Accepts live frame designators in addition to terminal designators.
pub(crate) fn builtin_terminal_parameter_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("terminal-parameter", &args, 2)?;
    expect_terminal_designator_eval(eval, &args[0])?;
    let key = expect_symbol_key(&args[1])?;
    TERMINAL_PARAMS.with(|slot| Ok(lookup_terminal_parameter_value(&slot.borrow(), &key)))
}


/// (terminal-parameters &optional TERMINAL) -> alist of terminal parameters
pub(crate) fn builtin_terminal_parameters(args: Vec<Value>) -> EvalResult {
    expect_max_args("terminal-parameters", &args, 1)?;
    if let Some(term) = args.first() {
        if !term.is_nil() {
            expect_terminal_designator(term)?;
        }
    }
    TERMINAL_PARAMS.with(|slot| {
        let merged = terminal_parameters_with_defaults(&slot.borrow());
        Ok(make_alist(merged))
    })
}

/// Context-aware variant of `terminal-parameters`.
///
/// Accepts live frame designators in addition to terminal designators.
pub(crate) fn builtin_terminal_parameters_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("terminal-parameters", &args, 1)?;
    if let Some(term) = args.first() {
        if !term.is_nil() {
            expect_terminal_designator_eval(eval, term)?;
        }
    }
    TERMINAL_PARAMS.with(|slot| {
        let merged = terminal_parameters_with_defaults(&slot.borrow());
        Ok(make_alist(merged))
    })
}


/// (set-terminal-parameter TERMINAL PARAMETER VALUE) -> previous value
pub(crate) fn builtin_set_terminal_parameter(args: Vec<Value>) -> EvalResult {
    expect_args("set-terminal-parameter", &args, 3)?;
    expect_terminal_designator(&args[0])?;
    if matches!(args[1], Value::Str(_)) {
        return Ok(Value::Nil);
    }
    let key = args[1];
    TERMINAL_PARAMS.with(|slot| {
        let mut params = slot.borrow_mut();
        if let Some((_, stored_value)) = params
            .iter_mut()
            .find(|(stored_key, _)| eq_value(stored_key, &key))
        {
            let previous = *stored_value;
            *stored_value = args[2];
            return Ok(previous);
        }

        let previous = terminal_parameter_default_value(&key).unwrap_or(Value::Nil);
        params.push((key, args[2]));
        Ok(previous)
    })
}

/// Context-aware variant of `set-terminal-parameter`.
///
/// Accepts live frame designators in addition to terminal designators.
pub(crate) fn builtin_set_terminal_parameter_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-terminal-parameter", &args, 3)?;
    expect_terminal_designator_eval(eval, &args[0])?;
    if matches!(args[1], Value::Str(_)) {
        return Ok(Value::Nil);
    }
    let key = args[1];
    TERMINAL_PARAMS.with(|slot| {
        let mut params = slot.borrow_mut();
        if let Some((_, stored_value)) = params
            .iter_mut()
            .find(|(stored_key, _)| eq_value(stored_key, &key))
        {
            let previous = *stored_value;
            *stored_value = args[2];
            return Ok(previous);
        }

        let previous = terminal_parameter_default_value(&key).unwrap_or(Value::Nil);
        params.push((key, args[2]));
        Ok(previous)
    })
}


// ---------------------------------------------------------------------------
// TTY builtins (we are not a TTY, so these return nil)
// ---------------------------------------------------------------------------

/// (tty-type &optional TERMINAL) -> nil
pub(crate) fn builtin_tty_type(args: Vec<Value>) -> EvalResult {
    expect_max_args("tty-type", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator(terminal)?;
    }
    Ok(terminal_runtime()
        .tty_type
        .map(Value::string)
        .unwrap_or(Value::Nil))
}

/// Context-aware variant of `tty-type`.
pub(crate) fn builtin_tty_type_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-type", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_eval(eval, terminal)?;
    }
    Ok(terminal_runtime()
        .tty_type
        .map(Value::string)
        .unwrap_or(Value::Nil))
}

pub(crate) fn builtin_tty_type_in_state(
    frames: &crate::window::FrameManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-type", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_in_state(frames, terminal)?;
    }
    Ok(terminal_runtime()
        .tty_type
        .map(Value::string)
        .unwrap_or(Value::Nil))
}

/// (tty-top-frame &optional TERMINAL) -> nil
pub(crate) fn builtin_tty_top_frame(args: Vec<Value>) -> EvalResult {
    expect_max_args("tty-top-frame", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator(terminal)?;
    }
    Ok(Value::Nil)
}

/// Context-aware variant of `tty-top-frame`.
pub(crate) fn builtin_tty_top_frame_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-top-frame", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_eval(eval, terminal)?;
    }
    Ok(if terminal_runtime().active {
        eval.frames
            .selected_frame()
            .map(|frame| Value::Frame(frame.id.0))
            .unwrap_or(Value::Nil)
    } else {
        Value::Nil
    })
}


/// (tty-display-color-p &optional TERMINAL) -> nil
pub(crate) fn builtin_tty_display_color_p(args: Vec<Value>) -> EvalResult {
    expect_max_args("tty-display-color-p", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator(terminal)?;
    }
    Ok(Value::bool(terminal_runtime().supports_color()))
}

/// Context-aware variant of `tty-display-color-p`.
pub(crate) fn builtin_tty_display_color_p_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-display-color-p", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_eval(eval, terminal)?;
    }
    Ok(Value::bool(terminal_runtime().supports_color()))
}


/// (tty-display-color-cells &optional TERMINAL) -> 0
pub(crate) fn builtin_tty_display_color_cells(args: Vec<Value>) -> EvalResult {
    expect_max_args("tty-display-color-cells", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator(terminal)?;
    }
    Ok(Value::Int(terminal_runtime().color_cells))
}

/// Context-aware variant of `tty-display-color-cells`.
pub(crate) fn builtin_tty_display_color_cells_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-display-color-cells", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_eval(eval, terminal)?;
    }
    Ok(Value::Int(terminal_runtime().color_cells))
}


/// (tty-no-underline &optional TERMINAL) -> nil
pub(crate) fn builtin_tty_no_underline(args: Vec<Value>) -> EvalResult {
    expect_max_args("tty-no-underline", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator(terminal)?;
    }
    Ok(Value::Nil)
}

/// Context-aware variant of `tty-no-underline`.
pub(crate) fn builtin_tty_no_underline_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-no-underline", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_eval(eval, terminal)?;
    }
    Ok(Value::Nil)
}


/// (controlling-tty-p &optional TERMINAL) -> nil
pub(crate) fn builtin_controlling_tty_p(args: Vec<Value>) -> EvalResult {
    expect_max_args("controlling-tty-p", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator(terminal)?;
    }
    Ok(Value::bool(terminal_runtime().controlling_tty))
}

/// Context-aware variant of `controlling-tty-p`.
pub(crate) fn builtin_controlling_tty_p_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("controlling-tty-p", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_eval(eval, terminal)?;
    }
    Ok(Value::bool(terminal_runtime().controlling_tty))
}


/// (suspend-tty &optional TTY) -> error in GUI/non-text terminal context.
pub(crate) fn builtin_suspend_tty(args: Vec<Value>) -> EvalResult {
    expect_max_args("suspend-tty", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator(terminal)?;
    }
    Err(signal(
        "error",
        vec![Value::string(
            "Attempt to suspend a non-text terminal device",
        )],
    ))
}

/// Context-aware variant of `suspend-tty`.
pub(crate) fn builtin_suspend_tty_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("suspend-tty", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_eval(eval, terminal)?;
    }
    Err(signal(
        "error",
        vec![Value::string(
            "Attempt to suspend a non-text terminal device",
        )],
    ))
}

pub(crate) fn builtin_suspend_tty_in_state(
    frames: &crate::window::FrameManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("suspend-tty", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_in_state(frames, terminal)?;
    }
    Err(signal(
        "error",
        vec![Value::string(
            "Attempt to suspend a non-text terminal device",
        )],
    ))
}

/// (resume-tty &optional TTY) -> error in GUI/non-text terminal context.
pub(crate) fn builtin_resume_tty(args: Vec<Value>) -> EvalResult {
    expect_max_args("resume-tty", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator(terminal)?;
    }
    Err(signal(
        "error",
        vec![Value::string(
            "Attempt to resume a non-text terminal device",
        )],
    ))
}

/// Context-aware variant of `resume-tty`.
pub(crate) fn builtin_resume_tty_eval(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("resume-tty", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_eval(eval, terminal)?;
    }
    Err(signal(
        "error",
        vec![Value::string(
            "Attempt to resume a non-text terminal device",
        )],
    ))
}

pub(crate) fn builtin_resume_tty_in_state(
    frames: &crate::window::FrameManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("resume-tty", &args, 1)?;
    if let Some(terminal) = args.first() {
        expect_terminal_designator_in_state(frames, terminal)?;
    }
    Err(signal(
        "error",
        vec![Value::string(
            "Attempt to resume a non-text terminal device",
        )],
    ))
}

// ---------------------------------------------------------------------------
// Builtins moved from builtins.rs
// ---------------------------------------------------------------------------

/// (delete-terminal &optional TERMINAL FORCE) -> nil or error
pub(crate) fn builtin_delete_terminal(args: Vec<Value>) -> EvalResult {
    expect_range_args("delete-terminal", &args, 0, 2)?;
    if args.first().is_some_and(|term| !term.is_nil()) {
        return Ok(Value::Nil);
    }
    Err(signal(
        "error",
        vec![Value::string(
            "Attempt to delete the sole active display terminal",
        )],
    ))
}

/// (make-terminal-frame PARMS) -> error (no TTY support)
pub(crate) fn builtin_make_terminal_frame(args: Vec<Value>) -> EvalResult {
    expect_args("make-terminal-frame", &args, 1)?;
    if !args[0].is_nil() && !matches!(args[0], Value::Cons(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), args[0]],
        ));
    }
    Err(signal(
        "error",
        vec![Value::string("Unknown terminal type")],
    ))
}
