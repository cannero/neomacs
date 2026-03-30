//! Keyboard macro support -- macro metadata, counter state, and Lisp entry points.
//!
//! Provides Emacs-compatible keyboard macro functionality:
//! - `start-kbd-macro` / `end-kbd-macro` -- record key sequences
//! - `call-last-kbd-macro` -- replay the last recorded macro
//! - `execute-kbd-macro` -- execute a macro N times
//! - `name-last-kbd-macro` -- bind a macro to a symbol
//! - `insert-kbd-macro` -- insert macro definition as Lisp text
//! - `kbd-macro-query` -- interactive query during playback
//! - `store-kbd-macro-event` -- add event to the keyboard runtime's current recording
//! - `kmacro-set-counter` / `kmacro-add-counter` / `kmacro-set-format` -- counter ops
//! - `executing-kbd-macro-p` / `defining-kbd-macro-p` -- predicates
//! - `last-kbd-macro` -- retrieve last macro value
//! - `kmacro-p` -- predicate for macro values

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::*;
use crate::gc::GcTrace;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Argument helpers (local copies, matching builtins.rs convention)
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

fn prefix_numeric_value(value: &Value) -> i64 {
    match value {
        Value::Nil => 1,
        Value::Symbol(id) if resolve_sym(*id) == "-" => -1,
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        Value::Cons(cell) => read_cons(*cell).car.as_int().unwrap_or(1),
        _ => 1,
    }
}

// ---------------------------------------------------------------------------
// KmacroManager
// ---------------------------------------------------------------------------

/// Metadata manager for keyboard macros.
///
/// GNU keeps the live recording/playback state on the keyboard runtime
/// (`current_kboard` plus global execution vars) and layers richer kmacro UI
/// state on top. NeoVM mirrors that split: the keyboard owner handles current
/// recording/playback, while this manager keeps only the higher-level ring and
/// counter metadata.
#[derive(Clone, Debug)]
pub struct KmacroManager {
    /// Ring of previously saved macros (most recent first).
    pub macro_ring: Vec<Vec<Value>>,
    /// Keyboard macro counter (for `kmacro-insert-counter`).
    pub counter: i64,
    /// Format string for the counter (printf-style, default "%d").
    pub counter_format: String,
}

impl Default for KmacroManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for KmacroManager {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for macro_entry in &self.macro_ring {
            for value in macro_entry {
                roots.push(*value);
            }
        }
    }
}

impl KmacroManager {
    /// Create a new manager with default state.
    pub fn new() -> Self {
        Self {
            macro_ring: Vec::new(),
            counter: 0,
            counter_format: "%d".to_string(),
        }
    }

    /// Format the counter using the current format string.
    pub fn format_counter(&self) -> String {
        // Support basic %d / %o / %x / %X formats.
        // For anything more complex, fall back to decimal.
        let fmt = &self.counter_format;
        if fmt.contains("%d") {
            fmt.replace("%d", &self.counter.to_string())
        } else if fmt.contains("%o") {
            fmt.replace("%o", &format!("{:o}", self.counter))
        } else if fmt.contains("%x") {
            fmt.replace("%x", &format!("{:x}", self.counter))
        } else if fmt.contains("%X") {
            fmt.replace("%X", &format!("{:X}", self.counter))
        } else {
            // Fallback: just print the number.
            self.counter.to_string()
        }
    }
}

// ===========================================================================
// Builtins (evaluator-dependent)
// ===========================================================================

/// (defining-kbd-macro APPEND &optional NO-EXEC) -> nil
///
/// Compatibility subset:
/// - starts keyboard macro recording (like `start-kbd-macro`)
/// - when APPEND is non-nil with no prior macro, signal
///   `(wrong-type-argument arrayp nil)`
/// - when already recording, signal `(error "Already defining kbd macro")`
/// - NO-EXEC is accepted for arity compatibility and currently ignored
pub(crate) fn builtin_defining_kbd_macro(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("defining-kbd-macro", &args, 1)?;
    expect_max_args("defining-kbd-macro", &args, 2)?;
    let append = args[0].is_truthy();
    let no_exec = args.get(1).is_some_and(Value::is_truthy);
    start_kbd_macro_impl(eval, append, no_exec)?;
    Ok(Value::Nil)
}

fn last_kbd_macro_or_array_error(eval: &super::eval::Context) -> Result<Vec<Value>, Flow> {
    eval.command_loop
        .last_kbd_macro()
        .map(|events| events.to_vec())
        .ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("arrayp"), Value::Nil],
            )
        })
}

fn execute_kbd_macro_iteration(eval: &mut super::eval::Context) -> EvalResult {
    eval.execute_kbd_macro_iteration_via_command_loop()
}

fn execute_kbd_macro_events_with_runtime_state(
    eval: &mut super::eval::Context,
    macro_events: &[Value],
    count: i64,
    loopfunc: Value,
) -> EvalResult {
    eval.with_executing_kbd_macro_runtime(macro_events.to_vec(), |eval| {
        let mut repeat = count;
        let mut success_count = 0usize;
        loop {
            eval.reset_executing_kbd_macro_runtime_iteration();

            if !loopfunc.is_nil() {
                let cont = eval.apply(loopfunc, vec![])?;
                if !cont.is_truthy() {
                    break;
                }
            }

            execute_kbd_macro_iteration(eval)?;
            success_count += 1;
            eval.note_executing_kbd_macro_iteration(success_count);

            if repeat == 0 {
                continue;
            }
            repeat -= 1;
            if repeat == 0 {
                break;
            }
        }

        Ok(Value::Nil)
    })
}

fn start_kbd_macro_impl(
    eval: &mut super::eval::Context,
    append: bool,
    no_exec: bool,
) -> EvalResult {
    let initial_events = if append {
        Some(last_kbd_macro_or_array_error(eval)?)
    } else {
        None
    };

    if let Some(ref initial_events) = initial_events
        && !no_exec
    {
        execute_kbd_macro_events_with_runtime_state(eval, initial_events, 1, Value::Nil)?;
    }

    eval.start_kbd_macro_runtime(initial_events.as_deref(), append)?;
    Ok(Value::Nil)
}

pub(crate) fn plan_call_last_kbd_macro(
    last_kbd_macro: Option<&[Value]>,
    args: &[Value],
) -> Result<(Vec<Value>, i64, Value), Flow> {
    expect_max_args("call-last-kbd-macro", args, 2)?;
    let repeat = args.first().map_or(1i64, prefix_numeric_value);
    let loopfunc = args.get(1).copied().unwrap_or(Value::Nil);

    let macro_keys = last_kbd_macro
        .map(|events| events.to_vec())
        .ok_or_else(|| {
            signal(
                "error",
                vec![Value::string("No keyboard macro has been defined")],
            )
        })?;

    Ok((macro_keys, repeat, loopfunc))
}

pub(crate) fn plan_execute_kbd_macro(
    eval: &super::eval::Context,
    args: &[Value],
) -> Result<(Vec<Value>, i64, Value), Flow> {
    expect_min_args("execute-kbd-macro", args, 1)?;
    expect_max_args("execute-kbd-macro", args, 3)?;
    let count = args.get(1).map_or(1, prefix_numeric_value);
    let loopfunc = args.get(2).copied().unwrap_or(Value::Nil);
    Ok((resolve_macro_events(eval, &args[0])?, count, loopfunc))
}

/// (start-kbd-macro &optional APPEND NO-EXEC) -> nil
///
/// Start recording a keyboard macro.  With non-nil APPEND, append to
/// the last macro instead of starting a new one.  Signals an error if
/// already recording.  With APPEND and nil NO-EXEC, replay the previous
/// macro before starting the new appended definition, matching GNU Emacs.
pub(crate) fn builtin_start_kbd_macro(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("start-kbd-macro", &args, 2)?;
    let append = args.first().is_some_and(|v| v.is_truthy());
    let no_exec = args.get(1).is_some_and(Value::is_truthy);
    start_kbd_macro_impl(eval, append, no_exec)?;
    Ok(Value::Nil)
}

/// (end-kbd-macro &optional REPEAT LOOPFUNC) -> nil
///
/// Stop recording a keyboard macro and optionally replay it.
pub(crate) fn builtin_end_kbd_macro(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("end-kbd-macro", &args, 2)?;
    let repeat = if let Some(value) = args.first() {
        expect_int(value).unwrap_or(1)
    } else {
        1
    };
    let loopfunc = args.get(1).copied().unwrap_or(Value::Nil);
    let recorded = eval.end_kbd_macro_runtime()?;
    if repeat == 0 {
        execute_kbd_macro_events_with_runtime_state(eval, &recorded, repeat, loopfunc)?;
    } else if repeat > 1 {
        execute_kbd_macro_events_with_runtime_state(eval, &recorded, repeat - 1, loopfunc)?;
    }
    Ok(Value::Nil)
}

/// (call-last-kbd-macro &optional REPEAT LOOPFUNC) -> nil
///
/// Execute the last keyboard macro.
pub(crate) fn builtin_call_last_kbd_macro(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if eval.command_loop.keyboard.kboard.defining_kbd_macro {
        return Err(signal(
            "error",
            vec![Value::string(
                "Can't execute anonymous macro while defining one",
            )],
        ));
    }

    let (macro_keys, repeat, loopfunc) =
        plan_call_last_kbd_macro(eval.command_loop.last_kbd_macro(), &args)?;
    let previous_last_command = eval.eval_symbol("last-command").unwrap_or(Value::Nil);
    let macro_value = Value::vector(macro_keys.clone());
    eval.assign("this-command", previous_last_command);
    eval.assign("real-this-command", macro_value);
    let result = execute_kbd_macro_events_with_runtime_state(eval, &macro_keys, repeat, loopfunc);
    if let Ok(last_command) = eval.eval_symbol("last-command") {
        eval.assign("this-command", last_command);
    }
    result
}

/// (execute-kbd-macro MACRO &optional COUNT LOOPFUNC) -> nil
///
/// Execute MACRO (a vector, string, or symbol) COUNT times.
/// If MACRO is a symbol, its function definition is used.
pub(crate) fn builtin_execute_kbd_macro(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let (macro_events, count, loopfunc) = plan_execute_kbd_macro(eval, &args)?;
    execute_kbd_macro_events_with_runtime_state(eval, &macro_events, count, loopfunc)
}

/// (name-last-kbd-macro SYMBOL) -> nil
///
/// Bind the last keyboard macro to SYMBOL as its function definition.
/// Signals an error if no macro has been recorded.
fn name_last_kbd_macro_impl(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
    call_name: &str,
) -> EvalResult {
    expect_args(call_name, &args, 1)?;

    let name = match &args[0] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            ));
        }
    };

    let macro_val = match eval.command_loop.last_kbd_macro() {
        Some(keys) => Value::vector(keys.to_vec()),
        None => {
            return Err(signal(
                "error",
                vec![Value::string("No keyboard macro has been defined")],
            ));
        }
    };

    eval.obarray.set_symbol_function(&name, macro_val);
    Ok(Value::Nil)
}

/// (name-last-kbd-macro SYMBOL) -> nil
///
/// Bind the last keyboard macro to SYMBOL as its function definition.
/// Signals an error if no macro has been recorded.
pub(crate) fn builtin_name_last_kbd_macro(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    name_last_kbd_macro_impl(eval, args, "name-last-kbd-macro")
}

/// (kmacro-name-last-macro SYMBOL) -> nil
///
/// Alias entry point used in startup wrappers for arity payload parity.
pub(crate) fn builtin_kmacro_name_last_macro(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    name_last_kbd_macro_impl(eval, args, "kmacro-name-last-macro")
}

/// (defining-kbd-macro-p) -> non-nil when keyboard macro recording is active.
pub(crate) fn builtin_defining_kbd_macro_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("defining-kbd-macro-p", &args, 0)?;
    Ok(Value::bool(
        eval.command_loop.keyboard.kboard.defining_kbd_macro,
    ))
}

/// (executing-kbd-macro-p) -> non-nil when keyboard macro execution is active.
pub(crate) fn builtin_executing_kbd_macro_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("executing-kbd-macro-p", &args, 0)?;
    Ok(Value::bool(
        eval.command_loop
            .keyboard
            .kboard
            .executing_kbd_macro
            .is_some(),
    ))
}

/// (last-kbd-macro) -> last recorded macro vector or nil.
pub(crate) fn builtin_last_kbd_macro(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("last-kbd-macro", &args, 0)?;
    match eval.command_loop.last_kbd_macro() {
        Some(keys) => Ok(Value::vector(keys.to_vec())),
        None => Ok(Value::Nil),
    }
}

/// (kmacro-p OBJECT) -> non-nil when OBJECT is a keyboard macro value.
///
/// Compatibility subset: accepts vector and string macro encodings.
pub(crate) fn builtin_kmacro_p(args: Vec<Value>) -> EvalResult {
    expect_args("kmacro-p", &args, 1)?;
    Ok(Value::bool(matches!(
        args[0],
        Value::Vector(_) | Value::Str(_)
    )))
}

/// (kmacro-set-counter COUNTER &optional FORMAT-START) -> nil
pub(crate) fn builtin_kmacro_set_counter(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("kmacro-set-counter", &args, 1)?;
    expect_max_args("kmacro-set-counter", &args, 2)?;
    eval.kmacro.counter = expect_int(&args[0])?;
    Ok(Value::Nil)
}

/// (kmacro-add-counter DELTA) -> nil
pub(crate) fn builtin_kmacro_add_counter(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("kmacro-add-counter", &args, 1)?;
    eval.kmacro.counter += expect_int(&args[0])?;
    Ok(Value::Nil)
}

/// (kmacro-set-format FORMAT) -> nil
pub(crate) fn builtin_kmacro_set_format(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("kmacro-set-format", &args, 1)?;
    let format = match &args[0] {
        Value::Str(id) => {
            let s = crate::emacs_core::value::with_heap(|h| h.get_string(*id).to_owned());
            if s.is_empty() { "%d".to_string() } else { s }
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    eval.kmacro.counter_format = format;
    Ok(Value::Nil)
}

/// (store-kbd-macro-event EVENT) -> nil
///
/// Add EVENT to the keyboard macro currently being recorded.
/// If not currently recording, this is a no-op.
pub(crate) fn builtin_store_kbd_macro_event(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("store-kbd-macro-event", &args, 1)?;
    eval.store_kbd_macro_runtime_event(args[0]);
    Ok(Value::Nil)
}

// ===========================================================================
// Internal helpers
// ===========================================================================

fn indirect_macro_function(eval: &super::eval::Context, value: &Value) -> Value {
    let mut current = *value;
    let mut seen = HashSet::new();

    loop {
        let Some(symbol_id) = (match current {
            Value::Symbol(id) | Value::Keyword(id) => Some(id),
            Value::True => Some(super::intern::intern("t")),
            Value::Nil => None,
            _ => None,
        }) else {
            return current;
        };

        if !seen.insert(symbol_id) {
            return current;
        }

        current = eval
            .obarray()
            .symbol_function_id(symbol_id)
            .copied()
            .unwrap_or(Value::Nil);
    }
}

/// Resolve a macro value the GNU `execute-kbd-macro` way:
/// follow symbol function indirections, then require a final string or vector.
fn resolve_macro_events(eval: &super::eval::Context, value: &Value) -> Result<Vec<Value>, Flow> {
    match indirect_macro_function(eval, value) {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            Ok(items.clone())
        }
        Value::Str(id) => {
            // Each character in the string becomes a Char event.
            let s = with_heap(|h| h.get_string(id).to_owned());
            Ok(s.chars().map(Value::Char).collect())
        }
        _ => Err(signal(
            "error",
            vec![Value::string("Keyboard macros must be strings or vectors")],
        )),
    }
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "kmacro_test.rs"]
mod tests;
