//! Terminal/TTY builtins extracted from display.rs and builtins.rs.
//!
//! Provides the terminal runtime owner, terminal parameter storage,
//! and all terminal/tty query builtins.

use crate::emacs_core::error::{EvalResult, Flow, signal};
use crate::emacs_core::value::*;
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Thread-local terminal state
// ---------------------------------------------------------------------------

thread_local! {
    static TERMINAL_MANAGER: RefCell<TerminalManager> = RefCell::new(TerminalManager::new());
}

pub(crate) const TERMINAL_NAME: &str = "initial_terminal";
pub(crate) const TERMINAL_ID: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalRuntime {
    active: bool,
    tty_type: Option<String>,
    color_cells: i64,
    controlling_tty: bool,
    suspended: bool,
}

impl TerminalRuntime {
    const fn inactive() -> Self {
        Self {
            active: false,
            tty_type: None,
            color_cells: 0,
            controlling_tty: false,
            suspended: false,
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

pub trait TerminalHost {
    fn suspend_tty(&mut self) -> Result<(), String>;
    fn resume_tty(&mut self) -> Result<(), String>;
    fn delete_terminal(&mut self) -> Result<(), String> {
        Ok(())
    }
}

struct TerminalRecord {
    id: u64,
    name: String,
    handle: Value,
    params: Vec<(Value, Value)>,
    runtime: TerminalRuntime,
    deleted: bool,
    host: Option<Box<dyn TerminalHost>>,
}

impl TerminalRecord {
    fn new(id: u64, name: String) -> Self {
        Self {
            id,
            name,
            handle: terminal_handle_for_id(id),
            params: Vec::new(),
            runtime: TerminalRuntime::inactive(),
            deleted: false,
            host: None,
        }
    }

    fn is_live(&self) -> bool {
        !self.deleted
    }

    fn is_active(&self) -> bool {
        if !self.is_live() {
            return false;
        }
        if self.runtime.controlling_tty || self.runtime.tty_type.is_some() {
            self.runtime.active && !self.runtime.suspended
        } else {
            true
        }
    }
}

struct TerminalManager {
    terminals: Vec<TerminalRecord>,
}

impl TerminalManager {
    fn new() -> Self {
        let mut this = Self {
            terminals: Vec::new(),
        };
        this.ensure_initial_terminal();
        this
    }

    fn ensure_initial_terminal(&mut self) -> &mut TerminalRecord {
        if let Some(idx) = self
            .terminals
            .iter()
            .position(|terminal| terminal.id == TERMINAL_ID)
        {
            if self.terminals[idx].deleted {
                self.terminals[idx].deleted = false;
                self.terminals[idx].runtime = TerminalRuntime::inactive();
                self.terminals[idx].host = None;
            }
            return &mut self.terminals[idx];
        }
        self.terminals
            .push(TerminalRecord::new(TERMINAL_ID, TERMINAL_NAME.to_string()));
        self.terminals.last_mut().expect("initial terminal present")
    }

    fn reset_handles(&mut self) {
        for terminal in &mut self.terminals {
            terminal.handle = terminal_handle_for_id(terminal.id);
        }
    }

    fn get(&self, id: u64) -> Option<&TerminalRecord> {
        self.terminals.iter().find(|terminal| terminal.id == id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut TerminalRecord> {
        self.terminals.iter_mut().find(|terminal| terminal.id == id)
    }

    fn find_by_handle(&self, value: &Value) -> Option<&TerminalRecord> {
        self.terminals
            .iter()
            .find(|terminal| eq_value(&terminal.handle, value))
    }

    fn find_by_handle_mut(&mut self, value: &Value) -> Option<&mut TerminalRecord> {
        self.terminals
            .iter_mut()
            .find(|terminal| eq_value(&terminal.handle, value))
    }

    fn live_terminals(&self) -> impl Iterator<Item = &TerminalRecord> {
        self.terminals.iter().filter(|terminal| terminal.is_live())
    }

    fn active_live_terminal_count(&self) -> usize {
        self.live_terminals()
            .filter(|terminal| terminal.is_active())
            .count()
    }

    fn ensure_terminal(
        &mut self,
        id: u64,
        name: String,
        runtime: TerminalRuntime,
    ) -> &mut TerminalRecord {
        if let Some(idx) = self.terminals.iter().position(|terminal| terminal.id == id) {
            let terminal = &mut self.terminals[idx];
            terminal.name = name;
            terminal.deleted = false;
            terminal.runtime = runtime;
            return terminal;
        }
        self.terminals.push(TerminalRecord {
            id,
            name,
            handle: terminal_handle_for_id(id),
            params: Vec::new(),
            runtime,
            deleted: false,
            host: None,
        });
        self.terminals.last_mut().expect("terminal present")
    }
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
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        let terminal = manager.ensure_initial_terminal();
        terminal.runtime = TerminalRuntime {
            active: config.controlling_tty || config.tty_type.is_some() || config.color_cells > 0,
            tty_type: config.tty_type,
            color_cells: config.color_cells.max(0),
            controlling_tty: config.controlling_tty,
            suspended: false,
        };
    });
}

pub(crate) fn ensure_terminal_runtime_owner(
    id: u64,
    name: impl Into<String>,
    config: TerminalRuntimeConfig,
) -> Value {
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        let runtime = TerminalRuntime {
            active: config.controlling_tty || config.tty_type.is_some() || config.color_cells > 0,
            tty_type: config.tty_type,
            color_cells: config.color_cells.max(0),
            controlling_tty: config.controlling_tty,
            suspended: false,
        };
        manager.ensure_terminal(id, name.into(), runtime).handle
    })
}

pub fn reset_terminal_runtime() {
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        manager.ensure_initial_terminal().runtime = TerminalRuntime::inactive();
    });
}

pub fn set_terminal_host(host: Box<dyn TerminalHost>) {
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        manager.ensure_initial_terminal().host = Some(host);
    });
}

pub fn reset_terminal_host() {
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        manager.ensure_initial_terminal().host = None;
    });
}

fn terminal_runtime() -> TerminalRuntime {
    TERMINAL_MANAGER.with(|slot| {
        slot.borrow()
            .get(TERMINAL_ID)
            .map(|terminal| terminal.runtime.clone())
            .unwrap_or_else(TerminalRuntime::inactive)
    })
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

fn terminal_runtime_suspended() -> bool {
    terminal_runtime().suspended
}

fn set_terminal_runtime_suspended(suspended: bool) {
    TERMINAL_MANAGER.with(|slot| {
        slot.borrow_mut()
            .ensure_initial_terminal()
            .runtime
            .suspended = suspended;
    });
}

fn with_terminal_host<R>(
    f: impl FnOnce(&mut dyn TerminalHost) -> Result<R, String>,
) -> Result<R, Flow> {
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        let Some(host) = manager.ensure_initial_terminal().host.as_deref_mut() else {
            return Err(signal(
                "error",
                vec![Value::string("TTY terminal host unavailable")],
            ));
        };
        f(host).map_err(|message| signal("error", vec![Value::string(message)]))
    })
}

/// Clear cached terminal thread-locals (called from `reset_display_thread_locals`).
pub(crate) fn reset_terminal_thread_locals() {
    TERMINAL_MANAGER.with(|slot| *slot.borrow_mut() = TerminalManager::new());
}

/// Reset only the terminal handle (stale ObjId safety on heap reset).
/// Does NOT reset terminal params or runtime config.
pub(crate) fn reset_terminal_handle() {
    TERMINAL_MANAGER.with(|slot| slot.borrow_mut().reset_handles());
}

/// Collect GC roots from terminal thread-locals.
pub(crate) fn collect_terminal_gc_roots(roots: &mut Vec<Value>) {
    TERMINAL_MANAGER.with(|slot| {
        for terminal in &slot.borrow().terminals {
            roots.push(terminal.handle);
            for (k, v) in &terminal.params {
                roots.push(*k);
                roots.push(*v);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Terminal handle helpers
// ---------------------------------------------------------------------------

fn terminal_handle_for_id(id: u64) -> Value {
    Value::vector(vec![
        Value::symbol("--neovm-terminal--"),
        Value::Int(id as i64),
    ])
}

pub(crate) fn terminal_handle_value() -> Value {
    terminal_handle_value_for_id(TERMINAL_ID).unwrap_or_else(|| terminal_handle_for_id(TERMINAL_ID))
}

pub(crate) fn terminal_handle_value_for_id(id: u64) -> Option<Value> {
    TERMINAL_MANAGER.with(|slot| slot.borrow().get(id).map(|terminal| terminal.handle))
}

pub(crate) fn is_terminal_handle(value: &Value) -> bool {
    terminal_handle_id(value).is_some()
}

pub(crate) fn terminal_handle_id(value: &Value) -> Option<u64> {
    TERMINAL_MANAGER.with(|slot| {
        slot.borrow()
            .find_by_handle(value)
            .map(|terminal| terminal.id)
    })
}

pub(crate) fn print_terminal_handle(value: &Value) -> Option<String> {
    TERMINAL_MANAGER.with(|slot| {
        slot.borrow()
            .find_by_handle(value)
            .map(|terminal| format!("#<terminal {} on {}>", terminal.id, terminal.name))
    })
}

// ---------------------------------------------------------------------------
// Terminal designator predicates
// ---------------------------------------------------------------------------

pub(crate) fn terminal_designator_p(value: &Value) -> bool {
    value.is_nil() || is_terminal_handle(value)
}

fn live_terminal_id_by_handle(value: &Value) -> Option<u64> {
    TERMINAL_MANAGER.with(|slot| {
        slot.borrow()
            .find_by_handle(value)
            .filter(|terminal| terminal.is_live())
            .map(|terminal| terminal.id)
    })
}

fn selected_terminal_id(eval: &crate::emacs_core::eval::Context) -> Option<u64> {
    eval.frames
        .selected_frame()
        .map(|frame| frame.terminal_id)
        .or_else(|| {
            TERMINAL_MANAGER.with(|slot| {
                slot.borrow()
                    .get(TERMINAL_ID)
                    .filter(|terminal| terminal.is_live())
                    .map(|terminal| terminal.id)
            })
        })
}

fn decode_terminal_id_eval(eval: &crate::emacs_core::eval::Context, value: &Value) -> Option<u64> {
    if value.is_nil() {
        return selected_terminal_id(eval);
    }
    if let Some(id) = live_terminal_id_by_handle(value) {
        return Some(id);
    }
    match value {
        Value::Frame(fid) => eval
            .frames
            .get(crate::window::FrameId(*fid as u64))
            .and_then(|frame| {
                TERMINAL_MANAGER.with(|slot| {
                    slot.borrow()
                        .get(frame.terminal_id)
                        .filter(|terminal| terminal.is_live())
                        .map(|terminal| terminal.id)
                })
            }),
        _ => None,
    }
}

pub(crate) fn terminal_designator_eval_p(
    eval: &mut crate::emacs_core::eval::Context,
    value: &Value,
) -> bool {
    decode_terminal_id_eval(eval, value).is_some()
}

pub(crate) fn terminal_designator_in_state_p(
    frames: &crate::window::FrameManager,
    value: &Value,
) -> bool {
    if value.is_nil() {
        return frames.selected_frame().is_some()
            || terminal_handle_value_for_id(TERMINAL_ID).is_some();
    }
    if let Some(id) = terminal_handle_id(value) {
        return TERMINAL_MANAGER.with(|slot| {
            slot.borrow()
                .get(id)
                .is_some_and(|terminal| terminal.is_live())
        });
    }
    match value {
        Value::Frame(fid) => frames.get(crate::window::FrameId(*fid as u64)).is_some(),
        _ => false,
    }
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

fn terminal_name_for_id(id: u64) -> Option<String> {
    TERMINAL_MANAGER.with(|slot| slot.borrow().get(id).map(|terminal| terminal.name.clone()))
}

fn terminal_runtime_for_id(id: u64) -> TerminalRuntime {
    TERMINAL_MANAGER.with(|slot| {
        slot.borrow()
            .get(id)
            .map(|terminal| terminal.runtime.clone())
            .unwrap_or_else(TerminalRuntime::inactive)
    })
}

fn terminal_params_for_id(id: u64) -> Vec<(Value, Value)> {
    TERMINAL_MANAGER.with(|slot| {
        slot.borrow()
            .get(id)
            .map(|terminal| terminal.params.clone())
            .unwrap_or_default()
    })
}

fn update_terminal_param(id: u64, key: Value, value: Value) -> Value {
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        let Some(terminal) = manager.get_mut(id) else {
            return Value::Nil;
        };
        if let Some((_, stored_value)) = terminal
            .params
            .iter_mut()
            .find(|(stored_key, _)| eq_value(stored_key, &key))
        {
            let previous = *stored_value;
            *stored_value = value;
            return previous;
        }
        let previous = terminal_parameter_default_value(&key).unwrap_or(Value::Nil);
        terminal.params.push((key, value));
        previous
    })
}

fn with_terminal_host_for_id<R>(
    id: u64,
    f: impl FnOnce(&mut dyn TerminalHost) -> Result<R, String>,
) -> Result<R, Flow> {
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        let Some(host) = manager
            .get_mut(id)
            .and_then(|terminal| terminal.host.as_deref_mut())
        else {
            return Err(signal(
                "error",
                vec![Value::string("TTY terminal host unavailable")],
            ));
        };
        f(host).map_err(|message| signal("error", vec![Value::string(message)]))
    })
}

fn delete_terminal_record(id: u64) {
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        if let Some(terminal) = manager.get_mut(id) {
            terminal.deleted = true;
            terminal.runtime = TerminalRuntime::inactive();
            terminal.host = None;
        }
    });
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
///
/// Accepts live frame designators in addition to terminal designators.
pub(crate) fn builtin_terminal_name(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("terminal-name", &args, 1)?;
    let designator = args.first().copied().unwrap_or(Value::Nil);
    let Some(terminal_id) = decode_terminal_id_eval(eval, &designator) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), designator],
        ));
    };
    Ok(Value::string(
        terminal_name_for_id(terminal_id).unwrap_or_else(|| TERMINAL_NAME.to_string()),
    ))
}

/// (terminal-list) -> list of live terminal handles.
pub(crate) fn builtin_terminal_list(args: Vec<Value>) -> EvalResult {
    expect_max_args("terminal-list", &args, 0)?;
    let terminals = TERMINAL_MANAGER.with(|slot| {
        slot.borrow()
            .live_terminals()
            .map(|terminal| terminal.handle)
            .collect::<Vec<_>>()
    });
    Ok(Value::list(terminals))
}

/// (selected-terminal) -> currently selected terminal handle.
#[cfg(test)]
pub(crate) fn builtin_selected_terminal(args: Vec<Value>) -> EvalResult {
    expect_args("selected-terminal", &args, 0)?;
    Ok(terminal_handle_value())
}

/// (frame-terminal &optional FRAME) -> opaque terminal handle.
///
/// Accepts live frame designators in addition to nil.
pub(crate) fn builtin_frame_terminal(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("frame-terminal", &args, 1)?;
    let terminal_id = if let Some(frame) = args.first() {
        if frame.is_nil() {
            selected_terminal_id(eval)
        } else {
            match frame {
                Value::Frame(fid) => eval
                    .frames
                    .get(crate::window::FrameId(*fid as u64))
                    .map(|frame| frame.terminal_id),
                _ => None,
            }
        }
    } else {
        selected_terminal_id(eval)
    };
    let Some(terminal_id) = terminal_id else {
        let bad = args.first().copied().unwrap_or(Value::Nil);
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), bad],
        ));
    };
    Ok(terminal_handle_value_for_id(terminal_id).unwrap_or_else(terminal_handle_value))
}

/// (terminal-live-p TERMINAL) -> t
///
/// In GNU Emacs, terminal-live-p returns the terminal type symbol
/// (e.g. 'x, 'w32) for GUI terminals, or t for TTY.  This is used
/// by framep-on-display to determine the window system type.
pub(crate) fn builtin_terminal_live_p(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("terminal-live-p", &args, 1, 1)?;
    let Some(terminal_id) = decode_terminal_id_eval(eval, &args[0]) else {
        return Ok(Value::Nil);
    };
    let runtime = terminal_runtime_for_id(terminal_id);
    // Return the window system type so framep-on-display works correctly.
    if runtime.controlling_tty || runtime.tty_type.is_some() {
        Ok(Value::True)
    } else if crate::emacs_core::display::x_window_system_active(eval) {
        Ok(Value::symbol(
            crate::emacs_core::display::gui_window_system_symbol(),
        ))
    } else {
        Ok(Value::True)
    }
}

/// (terminal-parameter TERMINAL PARAMETER) -> value
///
/// Accepts live frame designators in addition to terminal designators.
pub(crate) fn builtin_terminal_parameter(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("terminal-parameter", &args, 2)?;
    let Some(terminal_id) = decode_terminal_id_eval(eval, &args[0]) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), args[0]],
        ));
    };
    let key = expect_symbol_key(&args[1])?;
    Ok(lookup_terminal_parameter_value(
        &terminal_params_for_id(terminal_id),
        &key,
    ))
}

/// (terminal-parameters &optional TERMINAL) -> alist of terminal parameters
///
/// Accepts live frame designators in addition to terminal designators.
pub(crate) fn builtin_terminal_parameters(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("terminal-parameters", &args, 1)?;
    let designator = args.first().copied().unwrap_or(Value::Nil);
    let Some(terminal_id) = decode_terminal_id_eval(eval, &designator) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), designator],
        ));
    };
    let merged = terminal_parameters_with_defaults(&terminal_params_for_id(terminal_id));
    Ok(make_alist(merged))
}

/// (set-terminal-parameter TERMINAL PARAMETER VALUE) -> previous value
///
/// Accepts live frame designators in addition to terminal designators.
pub(crate) fn builtin_set_terminal_parameter(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-terminal-parameter", &args, 3)?;
    let Some(terminal_id) = decode_terminal_id_eval(eval, &args[0]) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), args[0]],
        ));
    };
    if matches!(args[1], Value::Str(_)) {
        return Ok(Value::Nil);
    }
    let key = args[1];
    Ok(update_terminal_param(terminal_id, key, args[2]))
}

// ---------------------------------------------------------------------------
// TTY builtins (we are not a TTY, so these return nil)
// ---------------------------------------------------------------------------

/// (tty-type &optional TERMINAL) -> nil
pub(crate) fn builtin_tty_type(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-type", &args, 1)?;
    let designator = args.first().copied().unwrap_or(Value::Nil);
    let Some(terminal_id) = decode_terminal_id_eval(eval, &designator) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), designator],
        ));
    };
    Ok(terminal_runtime_for_id(terminal_id)
        .tty_type
        .map(Value::string)
        .unwrap_or(Value::Nil))
}

/// (tty-top-frame &optional TERMINAL) -> nil
pub(crate) fn builtin_tty_top_frame(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-top-frame", &args, 1)?;
    let designator = args.first().copied().unwrap_or(Value::Nil);
    let Some(terminal_id) = decode_terminal_id_eval(eval, &designator) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), designator],
        ));
    };
    let runtime = terminal_runtime_for_id(terminal_id);
    if !runtime.active {
        return Ok(Value::Nil);
    }
    let top = eval
        .frames
        .frame_list()
        .into_iter()
        .find_map(|frame_id| {
            eval.frames
                .get(frame_id)
                .filter(|frame| frame.terminal_id == terminal_id)
                .map(|frame| Value::Frame(frame.id.0))
        })
        .unwrap_or(Value::Nil);
    Ok(top)
}

/// (tty-display-color-p &optional TERMINAL) -> nil
pub(crate) fn builtin_tty_display_color_p(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-display-color-p", &args, 1)?;
    let designator = args.first().copied().unwrap_or(Value::Nil);
    let Some(terminal_id) = decode_terminal_id_eval(eval, &designator) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), designator],
        ));
    };
    Ok(Value::bool(
        terminal_runtime_for_id(terminal_id).supports_color(),
    ))
}

/// (tty-display-color-cells &optional TERMINAL) -> 0
pub(crate) fn builtin_tty_display_color_cells(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-display-color-cells", &args, 1)?;
    let designator = args.first().copied().unwrap_or(Value::Nil);
    let Some(terminal_id) = decode_terminal_id_eval(eval, &designator) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), designator],
        ));
    };
    Ok(Value::Int(terminal_runtime_for_id(terminal_id).color_cells))
}

/// (tty-no-underline &optional TERMINAL) -> nil
pub(crate) fn builtin_tty_no_underline(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("tty-no-underline", &args, 1)?;
    if let Some(terminal) = args.first()
        && decode_terminal_id_eval(eval, terminal).is_none()
    {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), *terminal],
        ));
    }
    Ok(Value::Nil)
}

/// (controlling-tty-p &optional TERMINAL) -> nil
pub(crate) fn builtin_controlling_tty_p(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("controlling-tty-p", &args, 1)?;
    let designator = args.first().copied().unwrap_or(Value::Nil);
    let Some(terminal_id) = decode_terminal_id_eval(eval, &designator) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), designator],
        ));
    };
    Ok(Value::bool(
        terminal_runtime_for_id(terminal_id).controlling_tty,
    ))
}

/// (suspend-tty &optional TTY) -> error in GUI/non-text terminal context.
pub(crate) fn builtin_suspend_tty(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("suspend-tty", &args, 1)?;
    let designator = args.first().copied().unwrap_or(Value::Nil);
    let Some(terminal_id) = decode_terminal_id_eval(eval, &designator) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), designator],
        ));
    };
    let runtime = terminal_runtime_for_id(terminal_id);
    if !runtime.active {
        return Err(signal(
            "error",
            vec![Value::string(
                "Attempt to suspend a non-text terminal device",
            )],
        ));
    }

    if runtime.suspended {
        return Ok(Value::Nil);
    }

    let terminal = terminal_handle_value_for_id(terminal_id).unwrap_or_else(terminal_handle_value);
    let hook_sym =
        crate::emacs_core::hook_runtime::hook_symbol_by_name(eval, "suspend-tty-functions");
    let _ = crate::emacs_core::hook_runtime::run_named_hook(eval, hook_sym, &[terminal])?;
    with_terminal_host_for_id(terminal_id, |host| host.suspend_tty())?;
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        if let Some(terminal) = manager.get_mut(terminal_id) {
            terminal.runtime.suspended = true;
        }
    });
    Ok(Value::Nil)
}

/// (resume-tty &optional TTY) -> error in GUI/non-text terminal context.
pub(crate) fn builtin_resume_tty(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("resume-tty", &args, 1)?;
    let designator = args.first().copied().unwrap_or(Value::Nil);
    let Some(terminal_id) = decode_terminal_id_eval(eval, &designator) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("terminal-live-p"), designator],
        ));
    };
    let runtime = terminal_runtime_for_id(terminal_id);
    if !runtime.active {
        return Err(signal(
            "error",
            vec![Value::string(
                "Attempt to resume a non-text terminal device",
            )],
        ));
    }

    if !runtime.suspended {
        return Ok(Value::Nil);
    }

    with_terminal_host_for_id(terminal_id, |host| host.resume_tty())?;
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        if let Some(terminal) = manager.get_mut(terminal_id) {
            terminal.runtime.suspended = false;
        }
    });
    let terminal = terminal_handle_value_for_id(terminal_id).unwrap_or_else(terminal_handle_value);
    let hook_sym =
        crate::emacs_core::hook_runtime::hook_symbol_by_name(eval, "resume-tty-functions");
    let _ = crate::emacs_core::hook_runtime::run_named_hook(eval, hook_sym, &[terminal])?;
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// Builtins moved from builtins.rs
// ---------------------------------------------------------------------------

/// (delete-terminal &optional TERMINAL FORCE) -> nil or error
pub(crate) fn builtin_delete_terminal(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("delete-terminal", &args, 0, 2)?;
    let designator = args.first().copied().unwrap_or(Value::Nil);
    let Some(terminal_id) = decode_terminal_id_eval(eval, &designator) else {
        return Ok(Value::Nil);
    };
    let force = args.get(1).copied().unwrap_or(Value::Nil);
    let active_live_count =
        TERMINAL_MANAGER.with(|slot| slot.borrow().active_live_terminal_count());
    if force.is_nil() && active_live_count <= 1 {
        return Err(signal(
            "error",
            vec![Value::string(
                "Attempt to delete the sole active display terminal",
            )],
        ));
    }
    let terminal = terminal_handle_value_for_id(terminal_id).unwrap_or_else(terminal_handle_value);
    let hook_sym =
        crate::emacs_core::hook_runtime::hook_symbol_by_name(eval, "delete-terminal-functions");
    let _ = crate::emacs_core::hook_runtime::safe_run_named_hook(eval, hook_sym, &[terminal])?;
    TERMINAL_MANAGER.with(|slot| {
        let mut manager = slot.borrow_mut();
        let Some(host) = manager
            .get_mut(terminal_id)
            .and_then(|terminal| terminal.host.as_deref_mut())
        else {
            return Ok(());
        };
        host.delete_terminal()
            .map_err(|message| signal("error", vec![Value::string(message)]))
    })?;

    let frames_to_delete = eval
        .frames
        .frame_list()
        .into_iter()
        .filter(|frame_id| {
            eval.frames
                .get(*frame_id)
                .is_some_and(|frame| frame.terminal_id == terminal_id)
        })
        .collect::<Vec<_>>();
    for frame_id in frames_to_delete {
        let _ = crate::emacs_core::window_cmds::delete_frame_owned(
            eval,
            frame_id,
            crate::emacs_core::window_cmds::DeleteFrameHookMode::DeferSafe,
            false,
        )?;
    }
    delete_terminal_record(terminal_id);
    eval.command_loop
        .keyboard
        .delete_terminal_kboard(terminal_id);
    if eval.frames.selected_frame().is_none() {
        if let Some(next_selected) = eval.frames.frame_list().into_iter().next() {
            let _ = eval.frames.select_frame(next_selected);
        }
    }
    eval.sync_keyboard_terminal_owner();
    Ok(Value::Nil)
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
