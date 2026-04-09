//! Built-in primitive functions.
//!
//! All functions here take pre-evaluated `Vec<Value>` arguments and return `EvalResult`.
//! The evaluator dispatches here after evaluating the argument expressions.

use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

/// Debug flag: when true, log every dispatch_builtin call name.
/// Activated after window-setup-hook completes during startup.
static TRACE_ALL_BUILTINS: AtomicBool = AtomicBool::new(false);

/// Check if post-startup tracing is active.
pub(crate) fn is_post_startup_tracing() -> bool {
    TRACE_ALL_BUILTINS.load(Ordering::Relaxed)
}

pub(super) use super::error::{EvalResult, Flow, signal};
pub(super) use super::intern::{SymId, intern, intern_uninterned, resolve_sym};
pub(super) use super::keyboard::pure::{
    KEY_CHAR_ALT, KEY_CHAR_CODE_MASK, KEY_CHAR_CTRL, KEY_CHAR_HYPER, KEY_CHAR_META, KEY_CHAR_SHIFT,
    KEY_CHAR_SUPER, basic_char_code, describe_single_key_value, event_modifier_bit,
    event_modifier_prefix, key_sequence_values, resolve_control_code, symbol_has_modifier_prefix,
};
pub(super) use super::string_escape::{
    bytes_to_storage_string, bytes_to_unibyte_storage_string, decode_storage_char_codes,
    encode_char_code_for_string_storage, encode_nonunicode_char_for_storage, storage_char_len,
    storage_string_display_width, storage_substring,
};
pub(super) use super::value::*;
pub(super) use ::regex::Regex;
pub(super) use std::cell::RefCell;
pub(super) use std::collections::{HashMap, HashSet};

/// Reset all thread-local state in builtins (called from Context::new).
pub(crate) fn reset_builtins_thread_locals() {
    collections::reset_collections_thread_locals();
    stubs::reset_stubs_thread_locals();
    hooks::reset_hooks_thread_locals();
    symbols::reset_symbols_thread_locals();
}

pub use stubs::{NeomacsMonitorInfo, neomacs_monitor_info_snapshot, set_neomacs_monitor_info};

/// Expect exactly N arguments.
pub(super) fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

/// Expect at least N arguments.
pub(super) fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

/// Expect at most N arguments.
pub(super) fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

pub(super) fn expect_range_args(
    name: &str,
    args: &[Value],
    min: usize,
    max: usize,
) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

/// Extract an integer, signaling wrong-type-argument if not.
pub(super) fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *value],
        )),
    }
}

pub(super) fn expect_fixnum(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *value],
        )),
    }
}

pub(super) fn expect_char_table_index(value: &Value) -> Result<i64, Flow> {
    let idx = expect_fixnum(value)?;
    if !(0..=0x3F_FFFF).contains(&idx) {
        maybe_trace_characterp_nil(value, "expect_char_table_index");
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *value],
        ));
    }
    Ok(idx)
}

pub(super) fn expect_char_equal_code(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) if (0..=KEY_CHAR_CODE_MASK).contains(&n) => Ok(n),
        other => {
            maybe_trace_characterp_nil(value, "expect_char_equal_code");
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *value],
            ))
        }
    }
}

pub(super) fn expect_character_code(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(c) if (0..=0x3F_FFFF).contains(&c) => Ok(c as i64),
        other => {
            maybe_trace_characterp_nil(value, "expect_character_code");
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *value],
            ))
        }
    }
}

fn maybe_trace_characterp_nil(value: &Value, source: &str) {
    if !value.is_nil() {
        return;
    }
    if std::env::var("NEOVM_TRACE_CHARACTERP_NIL").unwrap_or_default() != "1" {
        return;
    }
    eprintln!(
        "NEOVM_TRACE_CHARACTERP_NIL source={source}\n{}",
        std::backtrace::Backtrace::force_capture()
    );
}

pub(super) fn char_equal_folded(code: i64) -> Option<String> {
    char::from_u32(code as u32).map(|ch| ch.to_lowercase().collect())
}

/// Extract an integer/marker-ish position value.
///
/// GNU Emacs accepts marker designators anywhere `integer-or-marker-p`
/// is allowed, using the marker's current position.
pub(super) fn expect_integer_or_marker(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        _ if super::marker::is_marker(value) => super::marker::marker_position_as_int(value),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

pub(super) fn expect_integer_or_marker_eval(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        _ if super::marker::is_marker(value) => {
            super::marker::marker_position_as_int_eval(eval, value)
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

/// Extract a non-negative integer, signaling `wholenump` on failure.
pub(super) fn expect_wholenump(value: &Value) -> Result<i64, Flow> {
    let n = match value.kind() {
        ValueKind::Fixnum(n) => n,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("wholenump"), *value],
            ));
        }
    };
    if n < 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *value],
        ));
    }
    Ok(n)
}

pub(super) enum NumberOrMarker {
    Int(i64),
    Float(f64),
}

pub(super) fn expect_number_or_marker(value: &Value) -> Result<NumberOrMarker, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(NumberOrMarker::Int(n)),
        ValueKind::Float => Ok(NumberOrMarker::Float(value.xfloat())),
        // Bignums lower into f64 for the comparison/numeric path,
        // matching GNU's XFLOATINT behaviour. Callers that need
        // exact arithmetic dispatch on the Value::kind() directly.
        ValueKind::Veclike(VecLikeType::Bignum) => {
            Ok(NumberOrMarker::Float(value.as_bignum().unwrap().to_f64()))
        }
        _ if super::marker::is_marker(value) => Ok(NumberOrMarker::Int(
            super::marker::marker_position_as_int(value)?,
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *value],
        )),
    }
}

pub(super) fn expect_number_or_marker_eval(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<NumberOrMarker, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(NumberOrMarker::Int(n)),
        ValueKind::Float => Ok(NumberOrMarker::Float(value.xfloat())),
        ValueKind::Veclike(VecLikeType::Bignum) => {
            Ok(NumberOrMarker::Float(value.as_bignum().unwrap().to_f64()))
        }
        _ if super::marker::is_marker(value) => Ok(NumberOrMarker::Int(
            super::marker::marker_position_as_int_eval(eval, value)?,
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *value],
        )),
    }
}

/// Extract a number as f64.
pub(super) fn expect_number(value: &Value) -> Result<f64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n as f64),
        ValueKind::Float => Ok(value.xfloat()),
        ValueKind::Veclike(VecLikeType::Bignum) => Ok(value.as_bignum().unwrap().to_f64()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *value],
        )),
    }
}

pub(super) fn expect_number_or_marker_f64(value: &Value) -> Result<f64, Flow> {
    match expect_number_or_marker(value)? {
        NumberOrMarker::Int(n) => Ok(n as f64),
        NumberOrMarker::Float(f) => Ok(f),
    }
}

pub(super) fn expect_number_or_marker_f64_eval(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<f64, Flow> {
    match expect_number_or_marker_eval(eval, value)? {
        NumberOrMarker::Int(n) => Ok(n as f64),
        NumberOrMarker::Float(f) => Ok(f),
    }
}

pub(super) fn expect_integer_or_marker_after_number_check(value: &Value) -> Result<i64, Flow> {
    match expect_number_or_marker(value)? {
        NumberOrMarker::Int(n) => Ok(n),
        NumberOrMarker::Float(_) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

pub(super) fn expect_integer_or_marker_after_number_check_eval(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<i64, Flow> {
    match expect_number_or_marker_eval(eval, value)? {
        NumberOrMarker::Int(n) => Ok(n),
        NumberOrMarker::Float(_) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

/// True if any arg is a float (triggers float arithmetic).
pub(super) fn has_float(args: &[Value]) -> bool {
    args.iter().any(|v| v.is_float())
}

/// True if any arg is a bignum (triggers GMP arithmetic).
///
/// Mirrors GNU `arith_driver` (`src/data.c:3215`), which switches to
/// `bignum_arith_driver` whenever a non-fixnum integer appears in the
/// argument stream.
pub(super) fn has_bignum(args: &[Value]) -> bool {
    args.iter().any(|v| v.is_bignum())
}

pub(super) fn normalize_string_start_arg(
    string: &str,
    start: Option<&Value>,
) -> Result<usize, Flow> {
    let Some(start_val) = start else {
        return Ok(0);
    };
    if start_val.is_nil() {
        return Ok(0);
    }

    let raw_start = expect_int(start_val)?;
    let len = string.chars().count() as i64;
    let normalized = if raw_start < 0 {
        len.checked_add(raw_start)
    } else {
        Some(raw_start)
    };

    let Some(start_idx) = normalized else {
        return Err(signal(
            "args-out-of-range",
            vec![Value::string(string), Value::fixnum(raw_start)],
        ));
    };

    if !(0..=len).contains(&start_idx) {
        return Err(signal(
            "args-out-of-range",
            vec![Value::string(string), Value::fixnum(raw_start)],
        ));
    }

    let start_char_idx = start_idx as usize;
    if start_char_idx == len as usize {
        return Ok(string.len());
    }

    Ok(string
        .char_indices()
        .nth(start_char_idx)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(string.len()))
}

pub(super) fn string_byte_to_char_index(s: &str, byte_idx: usize) -> Option<usize> {
    s.get(..byte_idx).map(|prefix| prefix.chars().count())
}

// Re-export sibling modules so submodules can use `super::eval`, `super::marker`, etc.
pub(super) use super::autoload;
pub(super) use super::builtins_extra;
pub(super) use super::ccl;
pub(super) use super::charset;
pub(super) use super::chartable;
pub(super) use super::editfns;
pub(super) use super::error;
pub(super) use super::eval;
pub(super) use super::fileio;
pub(super) use super::kbd;
pub(super) use super::keymap;
pub(super) use super::load;
pub(super) use super::marker;
pub(super) use super::navigation;
pub(super) use super::print;
pub(super) use super::regex;
pub(super) use super::subr_info;
pub(super) use super::syntax;
pub(super) use super::terminal;
pub(super) use super::textprop;
pub(super) use super::value;
pub(super) use super::window_cmds;

// --- Submodules ---
mod arithmetic;
pub(crate) mod collections;
mod cons_list;
mod misc_pure;
mod strings;
mod types;

pub(crate) use arithmetic::*;
pub(crate) use collections::*;
pub use cons_list::lambda_params_to_value;
pub use cons_list::lambda_to_closure_vector;
pub use cons_list::parse_lambda_params_from_value;
pub(crate) use cons_list::*;
pub(crate) use misc_pure::*;
pub(crate) use strings::*;
pub(crate) use types::*;

mod buffers;
pub(crate) mod higher_order;
mod hooks;
pub(crate) mod keymaps;
mod misc_eval;
pub(crate) mod search;
mod stubs;
pub(crate) mod symbols;

pub(crate) use buffers::*;
pub(crate) use higher_order::*;
pub(crate) use hooks::*;
pub(crate) use keymaps::*;
pub(crate) use misc_eval::*;
pub(crate) use search::*;
pub(crate) use stubs::*;
pub(crate) use symbols::*;

// ===========================================================================
// Helpers
// ===========================================================================

pub(super) fn expect_string(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(value.as_str().unwrap().to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

pub(super) fn expect_string_comparison_operand(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(value.as_str().unwrap().to_owned()),
        _ => value.as_symbol_name().map(str::to_owned).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *value],
            )
        }),
    }
}

pub(super) fn expect_strict_string(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(value.as_str().unwrap().to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

// Search / regex builtins are defined at the end of this file.

/// Try to dispatch a builtin function by name. Returns None if not a known builtin.
pub(crate) fn dispatch_builtin(
    eval: &mut super::eval::Context,
    name: &str,
    args: Vec<Value>,
) -> Option<EvalResult> {
    dispatch_builtin_by_id(eval, intern(name), args)
}

/// Try to dispatch a builtin function by its canonical symbol id.
pub(crate) fn dispatch_builtin_by_id(
    eval: &mut super::eval::Context,
    sym_id: SymId,
    args: Vec<Value>,
) -> Option<EvalResult> {
    eval.dispatch_subr_id(sym_id, args)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuiltinNoEvalPlaceholder {
    Nil,
    FixnumZero,
    WindowLineHeight,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuiltinNoEvalPolicy {
    Native,
    RequiresEvalState,
    Placeholder(BuiltinNoEvalPlaceholder),
}

static BUILTIN_NO_EVAL_POLICIES: OnceLock<Mutex<Vec<Option<BuiltinNoEvalPolicy>>>> =
    OnceLock::new();

fn builtin_no_eval_policies() -> &'static Mutex<Vec<Option<BuiltinNoEvalPolicy>>> {
    BUILTIN_NO_EVAL_POLICIES.get_or_init(|| Mutex::new(Vec::new()))
}

fn record_builtin_no_eval_policy(name: &str, policy: BuiltinNoEvalPolicy) {
    let sym_id = intern(name);
    let mut policies = builtin_no_eval_policies()
        .lock()
        .expect("builtin no-eval policy registry poisoned");
    let index = sym_id.0 as usize;
    if policies.len() <= index {
        policies.resize(index + 1, None);
    }
    policies[index] = Some(policy);
}

fn builtin_no_eval_policy(sym_id: SymId) -> BuiltinNoEvalPolicy {
    builtin_no_eval_policies()
        .lock()
        .expect("builtin no-eval policy registry poisoned")
        .get(sym_id.0 as usize)
        .copied()
        .flatten()
        .unwrap_or(BuiltinNoEvalPolicy::Native)
}

fn dispatch_builtin_stateless_placeholder(
    policy: BuiltinNoEvalPolicy,
    args: &[Value],
) -> Option<EvalResult> {
    let value = match policy {
        BuiltinNoEvalPolicy::Placeholder(BuiltinNoEvalPlaceholder::Nil) => Value::NIL,
        BuiltinNoEvalPolicy::Placeholder(BuiltinNoEvalPlaceholder::FixnumZero) => Value::fixnum(0),
        BuiltinNoEvalPolicy::Placeholder(BuiltinNoEvalPlaceholder::WindowLineHeight) => {
            if args.len() == 2 && args[1].as_symbol_name() == Some("window") {
                Value::NIL
            } else {
                return None;
            }
        }
        BuiltinNoEvalPolicy::Native | BuiltinNoEvalPolicy::RequiresEvalState => return None,
    };
    Some(Ok(value))
}

#[cfg(test)]
pub(crate) fn dispatch_builtin_without_eval_state(
    name: &str,
    args: Vec<Value>,
) -> Option<EvalResult> {
    use crate::emacs_core::eval::Context;

    thread_local! {
        static CTX: std::cell::RefCell<Context> = std::cell::RefCell::new(Context::new());
    }

    CTX.with(|cell| {
        let ctx = &mut *cell.borrow_mut();
        let sym_id = intern(name);
        let policy = builtin_no_eval_policy(sym_id);
        if let Some(result) = dispatch_builtin_stateless_placeholder(policy, &args) {
            return Some(result);
        }
        if policy == BuiltinNoEvalPolicy::RequiresEvalState {
            return None;
        }
        dispatch_builtin_by_id(ctx, sym_id, args)
    })
}

#[cfg(test)]
mod tests;

// -----------------------------------------------------------------------
// Wrapper functions for builtins that need tracing or non-standard access
// -----------------------------------------------------------------------

fn defsubr_run_hooks(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let hook_names: Vec<String> = args
        .iter()
        .filter_map(|a| a.as_symbol_name().map(|s| s.to_string()))
        .collect();
    let dominated_by_noise = hook_names
        .iter()
        .all(|h| h == "custom-define-hook" || h == "change-major-mode-hook");
    if dominated_by_noise {
        tracing::debug!(hooks = ?hook_names, "run-hooks");
    } else {
        tracing::info!(hooks = ?hook_names, "run-hooks called");
    }
    let result = builtin_run_hooks(eval, args);
    if !dominated_by_noise {
        tracing::info!(hooks = ?hook_names, "run-hooks returned");
    }
    if hook_names.iter().any(|h| h == "window-setup-hook") {
        tracing::info!("Enabling post-startup builtin tracing");
        TRACE_ALL_BUILTINS.store(true, Ordering::Relaxed);
    }
    result
}

fn defsubr_load(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let file_name = args.first().map(|a| format!("{}", a)).unwrap_or_default();
    tracing::info!(file = %file_name, "load called");
    let result = builtin_load(eval, args);
    tracing::info!(file = %file_name, ok = result.is_ok(), "load returned");
    result
}

fn defsubr_message(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let msg_preview: String = args
        .first()
        .map(|a| {
            let s = format!("{}", a);
            if s.len() > 120 {
                format!("{}...", &s[..120])
            } else {
                s
            }
        })
        .unwrap_or_default();
    tracing::info!(msg = %msg_preview, "message");
    builtin_message(eval, args)
}

fn defsubr_coding_system_aliases(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_aliases(&eval.coding_systems, args)
}
fn defsubr_coding_system_plist(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_plist(&eval.coding_systems, args)
}
fn defsubr_coding_system_put(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_put(&mut eval.coding_systems, args)
}
fn defsubr_coding_system_base(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_base(&eval.coding_systems, args)
}
fn defsubr_coding_system_eol_type(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_eol_type(&eval.coding_systems, args)
}
fn defsubr_detect_coding_string(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_detect_coding_string(&eval.coding_systems, args)
}
fn defsubr_detect_coding_region(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_detect_coding_region(&eval.coding_systems, args)
}
fn defsubr_keyboard_coding_system(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_keyboard_coding_system(&eval.coding_systems, args)
}
fn defsubr_terminal_coding_system(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_terminal_coding_system(&eval.coding_systems, args)
}
fn defsubr_coding_system_priority_list(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    super::coding::builtin_coding_system_priority_list(&eval.coding_systems, args)
}

fn defsubr_coding_system_p(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_p(&eval.coding_systems, args)
}
fn defsubr_check_coding_system(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_check_coding_system(&eval.coding_systems, args)
}
fn defsubr_check_coding_systems_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    super::coding::builtin_check_coding_systems_region(&eval.coding_systems, args)
}
fn defsubr_define_coding_system_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    super::coding::builtin_define_coding_system_internal(&mut eval.coding_systems, args)
}
fn defsubr_define_coding_system_alias(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    super::coding::builtin_define_coding_system_alias(&mut eval.coding_systems, args)
}
fn defsubr_set_coding_system_priority(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    super::coding::builtin_set_coding_system_priority(&mut eval.coding_systems, args)
}
fn defsubr_set_keyboard_coding_system_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    super::coding::builtin_set_keyboard_coding_system_internal(&mut eval.coding_systems, args)
}
fn defsubr_set_safe_terminal_coding_system_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    super::coding::builtin_set_safe_terminal_coding_system_internal(&mut eval.coding_systems, args)
}
fn defsubr_set_terminal_coding_system_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    super::coding::builtin_set_terminal_coding_system_internal(&mut eval.coding_systems, args)
}

type BuiltinFn = fn(&mut super::eval::Context, Vec<Value>) -> EvalResult;

#[derive(Clone, Copy)]
struct BuiltinRegistration {
    name: &'static str,
    func: BuiltinFn,
    min_args: u16,
    max_args: Option<u16>,
    no_eval_policy: BuiltinNoEvalPolicy,
}

impl BuiltinRegistration {
    const fn requires_eval_state(
        name: &'static str,
        func: BuiltinFn,
        min_args: u16,
        max_args: Option<u16>,
    ) -> Self {
        Self {
            name,
            func,
            min_args,
            max_args,
            no_eval_policy: BuiltinNoEvalPolicy::RequiresEvalState,
        }
    }

    const fn placeholder(
        name: &'static str,
        func: BuiltinFn,
        min_args: u16,
        max_args: Option<u16>,
        placeholder: BuiltinNoEvalPlaceholder,
    ) -> Self {
        Self {
            name,
            func,
            min_args,
            max_args,
            no_eval_policy: BuiltinNoEvalPolicy::Placeholder(placeholder),
        }
    }
}

fn register_builtin(ctx: &mut super::eval::Context, builtin: BuiltinRegistration) {
    if builtin.no_eval_policy != BuiltinNoEvalPolicy::Native {
        record_builtin_no_eval_policy(builtin.name, builtin.no_eval_policy);
    }
    ctx.defsubr(
        builtin.name,
        builtin.func,
        builtin.min_args,
        builtin.max_args,
    );
}

/// Register all builtins via defsubr — function pointer dispatch.
///
/// This replaces the giant match-by-name block in dispatch_builtin.
/// Each registered builtin is called via a direct function pointer,
/// matching GNU Emacs's defsubr/funcall_subr architecture.
pub(crate) fn init_builtins(ctx: &mut super::eval::Context) {
    use super::error::*;
    use super::eval::Context;
    use super::value::*;
    ctx.defsubr("apply", builtin_apply, 1, None);
    ctx.defsubr("funcall", builtin_funcall, 1, None);
    ctx.defsubr(
        "funcall-interactively",
        builtin_funcall_interactively,
        0,
        None,
    );
    ctx.defsubr(
        "funcall-with-delayed-message",
        builtin_funcall_with_delayed_message,
        3,
        Some(3),
    );
    ctx.defsubr("defalias", builtin_defalias, 2, Some(3));
    ctx.defsubr("provide", builtin_provide, 1, Some(2));
    ctx.defsubr("require", builtin_require, 1, Some(3));
    ctx.defsubr("mapcan", builtin_mapcan, 2, Some(2));
    ctx.defsubr("mapcar", builtin_mapcar, 2, Some(2));
    ctx.defsubr("mapc", builtin_mapc, 2, Some(2));
    ctx.defsubr("mapconcat", builtin_mapconcat, 2, Some(3));
    ctx.defsubr("sort", builtin_sort, 1, None);
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("functionp", builtin_functionp, 1, Some(1)),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("defvaralias", builtin_defvaralias, 2, Some(3)),
    );
    ctx.defsubr("boundp", builtin_boundp, 1, Some(1));
    ctx.defsubr("default-boundp", builtin_default_boundp, 1, Some(1));
    ctx.defsubr(
        "default-toplevel-value",
        builtin_default_toplevel_value,
        1,
        Some(1),
    );
    ctx.defsubr("fboundp", builtin_fboundp, 1, Some(1));
    ctx.defsubr(
        "internal-make-var-non-special",
        builtin_internal_make_var_non_special,
        1,
        Some(1),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "indirect-variable",
            builtin_indirect_variable,
            1,
            Some(1),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("handler-bind-1", builtin_handler_bind_1, 1, None),
    );
    ctx.defsubr("symbol-value", builtin_symbol_value, 1, Some(1));
    ctx.defsubr("symbol-function", builtin_symbol_function, 1, Some(1));
    ctx.defsubr("set", builtin_set, 2, Some(2));
    ctx.defsubr("fset", builtin_fset, 2, Some(2));
    ctx.defsubr("makunbound", builtin_makunbound, 1, Some(1));
    ctx.defsubr("fmakunbound", builtin_fmakunbound, 1, Some(1));
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("macroexpand", builtin_macroexpand, 1, Some(2)),
    );
    ctx.defsubr("get", builtin_get, 2, Some(2));
    ctx.defsubr("put", builtin_put, 3, Some(3));
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("setplist", builtin_setplist, 2, Some(2)),
    );
    ctx.defsubr("symbol-plist", builtin_symbol_plist_fn, 1, Some(1));
    ctx.defsubr("indirect-function", builtin_indirect_function, 1, Some(2));
    ctx.defsubr("signal", super::errors::builtin_signal, 2, Some(2));
    ctx.defsubr(
        "getenv-internal",
        super::process::builtin_getenv_internal,
        1,
        Some(2),
    );
    ctx.defsubr("special-variable-p", builtin_special_variable_p, 1, Some(1));
    ctx.defsubr("intern", builtin_intern_fn, 1, Some(2));
    ctx.defsubr("intern-soft", builtin_intern_soft, 1, Some(2));
    ctx.defsubr("run-hook-with-args", builtin_run_hook_with_args, 1, None);
    ctx.defsubr(
        "run-hook-with-args-until-success",
        builtin_run_hook_with_args_until_success,
        0,
        None,
    );
    ctx.defsubr(
        "run-hook-with-args-until-failure",
        builtin_run_hook_with_args_until_failure,
        1,
        None,
    );
    ctx.defsubr("run-hook-wrapped", builtin_run_hook_wrapped, 2, None);
    ctx.defsubr(
        "run-window-configuration-change-hook",
        super::window_cmds::builtin_run_window_configuration_change_hook,
        0,
        None,
    );
    ctx.defsubr(
        "run-window-scroll-functions",
        super::window_cmds::builtin_run_window_scroll_functions,
        0,
        None,
    );
    ctx.defsubr("featurep", builtin_featurep, 1, Some(2));
    ctx.defsubr("garbage-collect", builtin_garbage_collect, 0, Some(0));
    ctx.defsubr("eval", builtin_eval, 1, Some(2));
    ctx.defsubr("get-buffer-create", builtin_get_buffer_create, 1, Some(2));
    ctx.defsubr("get-buffer", builtin_get_buffer, 1, Some(1));
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "make-indirect-buffer",
            builtin_make_indirect_buffer,
            2,
            Some(4),
        ),
    );
    ctx.defsubr("find-buffer", builtin_find_buffer, 2, Some(2));
    ctx.defsubr("buffer-live-p", builtin_buffer_live_p, 1, Some(1));
    ctx.defsubr(
        "barf-if-buffer-read-only",
        builtin_barf_if_buffer_read_only,
        0,
        Some(1),
    );
    ctx.defsubr(
        "bury-buffer-internal",
        builtin_bury_buffer_internal,
        1,
        Some(1),
    );
    ctx.defsubr("get-file-buffer", builtin_get_file_buffer, 1, Some(1));
    ctx.defsubr("kill-buffer", builtin_kill_buffer, 0, Some(1));
    ctx.defsubr("set-buffer", builtin_set_buffer, 1, Some(1));
    ctx.defsubr("current-buffer", builtin_current_buffer, 0, Some(0));
    ctx.defsubr("buffer-name", builtin_buffer_name, 0, Some(1));
    ctx.defsubr("buffer-file-name", builtin_buffer_file_name, 0, Some(1));
    ctx.defsubr("buffer-base-buffer", builtin_buffer_base_buffer, 0, Some(1));
    ctx.defsubr("buffer-last-name", builtin_buffer_last_name, 0, Some(1));
    ctx.defsubr("rename-buffer", builtin_rename_buffer, 1, Some(2));
    ctx.defsubr("buffer-string", builtin_buffer_string, 0, Some(0));
    ctx.defsubr(
        "buffer-line-statistics",
        builtin_buffer_line_statistics,
        0,
        None,
    );
    ctx.defsubr(
        "buffer-text-pixel-size",
        builtin_buffer_text_pixel_size,
        0,
        Some(4),
    );
    ctx.defsubr(
        "base64-encode-region",
        super::fns::builtin_base64_encode_region,
        2,
        Some(3),
    );
    ctx.defsubr(
        "base64-decode-region",
        super::fns::builtin_base64_decode_region,
        0,
        None,
    );
    ctx.defsubr(
        "base64url-encode-region",
        super::fns::builtin_base64url_encode_region,
        2,
        Some(3),
    );
    ctx.defsubr("md5", super::fns::builtin_md5, 1, Some(5));
    ctx.defsubr("secure-hash", super::fns::builtin_secure_hash, 2, Some(5));
    ctx.defsubr("buffer-hash", super::fns::builtin_buffer_hash, 0, Some(1));
    ctx.defsubr("buffer-substring", builtin_buffer_substring, 2, Some(2));
    ctx.defsubr(
        "compare-buffer-substrings",
        builtin_compare_buffer_substrings,
        6,
        Some(6),
    );
    ctx.defsubr("point", builtin_point, 0, Some(0));
    ctx.defsubr("point-min", builtin_point_min, 0, Some(0));
    ctx.defsubr("point-max", builtin_point_max, 0, Some(0));
    ctx.defsubr("goto-char", builtin_goto_char, 1, Some(1));
    ctx.defsubr("field-beginning", builtin_field_beginning, 0, Some(3));
    ctx.defsubr("field-end", builtin_field_end, 0, Some(3));
    ctx.defsubr("field-string", builtin_field_string, 0, Some(1));
    ctx.defsubr(
        "field-string-no-properties",
        builtin_field_string_no_properties,
        0,
        Some(1),
    );
    ctx.defsubr("constrain-to-field", builtin_constrain_to_field, 2, Some(5));
    ctx.defsubr("insert", builtin_insert, 0, None);
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "insert-and-inherit",
            builtin_insert_and_inherit,
            0,
            None,
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "insert-before-markers-and-inherit",
            builtin_insert_before_markers_and_inherit,
            0,
            None,
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "insert-buffer-substring",
            builtin_insert_buffer_substring,
            1,
            Some(3),
        ),
    );
    ctx.defsubr("insert-char", builtin_insert_char, 1, Some(3));
    ctx.defsubr("insert-byte", builtin_insert_byte, 2, Some(3));
    ctx.defsubr(
        "replace-region-contents",
        builtin_replace_region_contents,
        3,
        Some(6),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "set-buffer-multibyte",
            builtin_set_buffer_multibyte,
            1,
            Some(1),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "kill-all-local-variables",
            builtin_kill_all_local_variables,
            0,
            Some(1),
        ),
    );
    ctx.defsubr("buffer-swap-text", builtin_buffer_swap_text, 1, Some(1));
    ctx.defsubr(
        "delete-region",
        super::editfns::builtin_delete_region,
        2,
        Some(2),
    );
    ctx.defsubr(
        "delete-and-extract-region",
        super::editfns::builtin_delete_and_extract_region,
        2,
        Some(2),
    );
    ctx.defsubr(
        "subst-char-in-region",
        builtin_subst_char_in_region,
        4,
        Some(5),
    );
    ctx.defsubr("delete-field", builtin_delete_field, 0, Some(1));
    ctx.defsubr(
        "delete-all-overlays",
        builtin_delete_all_overlays,
        0,
        Some(1),
    );
    ctx.defsubr(
        "erase-buffer",
        super::editfns::builtin_erase_buffer,
        0,
        Some(0),
    );
    ctx.defsubr("buffer-enable-undo", builtin_buffer_enable_undo, 0, Some(1));
    ctx.defsubr("buffer-size", builtin_buffer_size, 0, Some(1));
    ctx.defsubr("narrow-to-region", builtin_narrow_to_region, 2, Some(2));
    ctx.defsubr("widen", builtin_widen, 0, Some(0));
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "internal--labeled-narrow-to-region",
            builtin_internal_labeled_narrow_to_region,
            3,
            Some(3),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "internal--labeled-widen",
            builtin_internal_labeled_widen,
            1,
            Some(1),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    ctx.defsubr("buffer-modified-p", builtin_buffer_modified_p, 0, Some(1));
    ctx.defsubr(
        "set-buffer-modified-p",
        builtin_set_buffer_modified_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "buffer-modified-tick",
        builtin_buffer_modified_tick,
        0,
        None,
    );
    ctx.defsubr(
        "buffer-chars-modified-tick",
        builtin_buffer_chars_modified_tick,
        0,
        None,
    );
    ctx.defsubr("buffer-list", builtin_buffer_list, 0, Some(1));
    ctx.defsubr("other-buffer", builtin_other_buffer, 0, Some(3));
    ctx.defsubr(
        "generate-new-buffer-name",
        builtin_generate_new_buffer_name,
        1,
        Some(2),
    );
    ctx.defsubr("char-after", builtin_char_after, 0, Some(1));
    ctx.defsubr("char-before", builtin_char_before, 0, Some(1));
    ctx.defsubr("byte-to-position", builtin_byte_to_position, 1, Some(1));
    ctx.defsubr("position-bytes", builtin_position_bytes, 1, Some(1));
    ctx.defsubr("get-byte", builtin_get_byte, 0, Some(2));
    ctx.defsubr("buffer-local-value", builtin_buffer_local_value, 2, Some(2));
    ctx.defsubr(
        "local-variable-if-set-p",
        builtin_local_variable_if_set_p,
        1,
        Some(2),
    );
    ctx.defsubr(
        "variable-binding-locus",
        builtin_variable_binding_locus,
        1,
        Some(1),
    );
    ctx.defsubr("interactive-form", builtin_interactive_form, 1, Some(1));
    ctx.defsubr(
        "command-modes",
        super::interactive::builtin_command_modes,
        1,
        Some(1),
    );
    ctx.defsubr("search-forward", builtin_search_forward, 1, Some(4));
    ctx.defsubr("search-backward", builtin_search_backward, 1, Some(4));
    ctx.defsubr("re-search-forward", builtin_re_search_forward, 1, Some(4));
    ctx.defsubr("re-search-backward", builtin_re_search_backward, 1, Some(4));
    ctx.defsubr("looking-at", builtin_looking_at, 1, Some(2));
    ctx.defsubr("posix-looking-at", builtin_posix_looking_at, 1, Some(2));
    ctx.defsubr("string-match", builtin_string_match, 2, Some(4));
    ctx.defsubr("string-match-p", builtin_string_match_p, 0, None);
    ctx.defsubr("posix-string-match", builtin_posix_string_match, 2, Some(4));
    ctx.defsubr("match-beginning", builtin_match_beginning, 1, Some(1));
    ctx.defsubr("match-end", builtin_match_end, 1, Some(1));
    ctx.defsubr("match-data", builtin_match_data, 0, Some(3));
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "match-data--translate",
            builtin_match_data_translate,
            1,
            Some(1),
        ),
    );
    ctx.defsubr("set-match-data", builtin_set_match_data, 1, Some(2));
    ctx.defsubr("replace-match", builtin_replace_match, 1, Some(5));
    ctx.defsubr(
        "find-charset-region",
        super::charset::builtin_find_charset_region,
        0,
        None,
    );
    ctx.defsubr(
        "charset-after",
        super::charset::builtin_charset_after,
        0,
        Some(1),
    );
    ctx.defsubr(
        "format-mode-line",
        super::xdisp::builtin_format_mode_line_ctx,
        1,
        Some(4),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "window-line-height",
            super::xdisp::builtin_window_line_height,
            0,
            Some(2),
            BuiltinNoEvalPlaceholder::WindowLineHeight,
        ),
    );
    ctx.defsubr(
        "posn-at-point",
        super::xdisp::builtin_posn_at_point,
        0,
        Some(2),
    );
    ctx.defsubr("posn-at-x-y", super::xdisp::builtin_posn_at_x_y, 2, Some(4));
    ctx.defsubr(
        "coordinates-in-window-p",
        super::window_cmds::builtin_coordinates_in_window_p,
        2,
        Some(2),
    );
    ctx.defsubr(
        "tool-bar-height",
        super::xdisp::builtin_tool_bar_height_ctx,
        0,
        Some(2),
    );
    ctx.defsubr(
        "tab-bar-height",
        super::xdisp::builtin_tab_bar_height_ctx,
        0,
        Some(2),
    );
    ctx.defsubr("list-fonts", super::font::builtin_list_fonts, 1, Some(4));
    ctx.defsubr("find-font", super::font::builtin_find_font, 1, Some(2));
    ctx.defsubr(
        "font-family-list",
        super::font::builtin_font_family_list,
        0,
        Some(1),
    );
    ctx.defsubr("font-info", super::font::builtin_font_info, 1, Some(2));
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("new-fontset", builtin_new_fontset, 2, Some(2)),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "set-fontset-font",
            builtin_set_fontset_font,
            3,
            Some(5),
        ),
    );
    ctx.defsubr(
        "insert-file-contents",
        super::fileio::builtin_insert_file_contents,
        1,
        Some(5),
    );
    ctx.defsubr(
        "write-region",
        super::fileio::builtin_write_region,
        3,
        Some(7),
    );
    ctx.defsubr(
        "file-name-completion",
        super::dired::builtin_file_name_completion,
        2,
        Some(3),
    );
    ctx.defsubr(
        "set-visited-file-modtime",
        super::fileio::builtin_set_visited_file_modtime,
        0,
        Some(1),
    );
    ctx.defsubr("make-keymap", builtin_make_keymap, 0, Some(1));
    ctx.defsubr("make-sparse-keymap", builtin_make_sparse_keymap, 0, Some(1));
    ctx.defsubr("copy-keymap", builtin_copy_keymap, 1, Some(1));
    ctx.defsubr("define-key", builtin_define_key, 3, Some(4));
    ctx.defsubr("lookup-key", builtin_lookup_key, 2, Some(3));
    ctx.defsubr("global-set-key", builtin_global_set_key, 0, None);
    ctx.defsubr("local-set-key", builtin_local_set_key, 0, None);
    ctx.defsubr("use-local-map", builtin_use_local_map, 1, Some(1));
    ctx.defsubr("use-global-map", builtin_use_global_map, 1, Some(1));
    ctx.defsubr("current-local-map", builtin_current_local_map, 0, Some(0));
    ctx.defsubr("current-global-map", builtin_current_global_map, 0, Some(0));
    ctx.defsubr(
        "current-active-maps",
        builtin_current_active_maps,
        0,
        Some(2),
    );
    ctx.defsubr(
        "current-minor-mode-maps",
        builtin_current_minor_mode_maps,
        0,
        Some(0),
    );
    ctx.defsubr("keymap-parent", builtin_keymap_parent, 1, Some(1));
    ctx.defsubr("set-keymap-parent", builtin_set_keymap_parent, 2, Some(2));
    ctx.defsubr("keymapp", builtin_keymapp, 1, Some(1));
    ctx.defsubr("accessible-keymaps", builtin_accessible_keymaps, 1, Some(2));
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("map-keymap", builtin_map_keymap, 2, Some(3)),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "map-keymap-internal",
            builtin_map_keymap_internal,
            2,
            Some(2),
        ),
    );
    ctx.defsubr(
        "print--preprocess",
        super::process::builtin_print_preprocess,
        1,
        Some(1),
    );
    ctx.defsubr(
        "format-network-address",
        super::process::builtin_format_network_address,
        1,
        Some(2),
    );
    ctx.defsubr(
        "network-interface-list",
        super::process::builtin_network_interface_list,
        0,
        Some(2),
    );
    ctx.defsubr(
        "network-interface-info",
        super::process::builtin_network_interface_info,
        1,
        Some(1),
    );
    ctx.defsubr(
        "signal-names",
        super::process::builtin_signal_names,
        0,
        Some(0),
    );
    ctx.defsubr(
        "accept-process-output",
        super::process::builtin_accept_process_output,
        0,
        Some(4),
    );
    ctx.defsubr(
        "list-system-processes",
        super::process::builtin_list_system_processes,
        0,
        Some(0),
    );
    ctx.defsubr(
        "num-processors",
        super::process::builtin_num_processors,
        0,
        Some(1),
    );
    ctx.defsubr(
        "make-process",
        super::process::builtin_make_process,
        0,
        None,
    );
    ctx.defsubr(
        "make-network-process",
        super::process::builtin_make_network_process,
        0,
        None,
    );
    ctx.defsubr(
        "make-pipe-process",
        super::process::builtin_make_pipe_process,
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-boot",
        super::process::builtin_gnutls_boot,
        3,
        Some(3),
    );
    ctx.defsubr(
        "make-serial-process",
        super::process::builtin_make_serial_process,
        0,
        None,
    );
    ctx.defsubr(
        "serial-process-configure",
        super::process::builtin_serial_process_configure,
        0,
        None,
    );
    ctx.defsubr(
        "call-process",
        super::process::builtin_call_process,
        1,
        None,
    );
    ctx.defsubr(
        "call-process-region",
        super::process::builtin_call_process_region,
        3,
        None,
    );
    ctx.defsubr(
        "continue-process",
        super::process::builtin_continue_process,
        0,
        Some(2),
    );
    ctx.defsubr(
        "delete-process",
        super::process::builtin_delete_process,
        0,
        Some(1),
    );
    ctx.defsubr(
        "interrupt-process",
        super::process::builtin_interrupt_process,
        0,
        Some(2),
    );
    ctx.defsubr(
        "kill-process",
        super::process::builtin_kill_process,
        0,
        Some(2),
    );
    ctx.defsubr(
        "quit-process",
        super::process::builtin_quit_process,
        0,
        Some(2),
    );
    ctx.defsubr(
        "signal-process",
        super::process::builtin_signal_process,
        2,
        Some(3),
    );
    ctx.defsubr(
        "stop-process",
        super::process::builtin_stop_process,
        0,
        Some(2),
    );
    ctx.defsubr(
        "get-process",
        super::process::builtin_get_process,
        1,
        Some(1),
    );
    ctx.defsubr(
        "get-buffer-process",
        super::process::builtin_get_buffer_process,
        1,
        Some(1),
    );
    ctx.defsubr(
        "process-attributes",
        super::process::builtin_process_attributes,
        1,
        Some(1),
    );
    ctx.defsubr(
        "start-process",
        super::process::builtin_start_process,
        3,
        None,
    );
    ctx.defsubr(
        "start-file-process",
        super::process::builtin_start_file_process,
        3,
        None,
    );
    ctx.defsubr(
        "start-process-shell-command",
        super::process::builtin_start_process_shell_command,
        3,
        Some(3),
    );
    ctx.defsubr(
        "start-file-process-shell-command",
        super::process::builtin_start_file_process_shell_command,
        3,
        Some(3),
    );
    ctx.defsubr(
        "open-network-stream",
        super::process::builtin_open_network_stream,
        4,
        Some(5),
    );
    ctx.defsubr("processp", super::process::builtin_processp, 1, Some(1));
    ctx.defsubr("process-id", super::process::builtin_process_id, 1, Some(1));
    ctx.defsubr(
        "process-command",
        super::process::builtin_process_command,
        1,
        Some(1),
    );
    ctx.defsubr(
        "process-contact",
        super::process::builtin_process_contact,
        1,
        Some(3),
    );
    ctx.defsubr(
        "process-filter",
        super::process::builtin_process_filter,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-filter",
        super::process::builtin_set_process_filter,
        2,
        Some(2),
    );
    ctx.defsubr(
        "process-sentinel",
        super::process::builtin_process_sentinel,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-sentinel",
        super::process::builtin_set_process_sentinel,
        0,
        None,
    );
    ctx.defsubr(
        "process-coding-system",
        super::process::builtin_process_coding_system,
        0,
        None,
    );
    ctx.defsubr(
        "process-datagram-address",
        super::process::builtin_process_datagram_address,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-buffer",
        super::process::builtin_set_process_buffer,
        2,
        Some(2),
    );
    ctx.defsubr(
        "set-process-thread",
        super::process::builtin_set_process_thread,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-window-size",
        super::process::builtin_set_process_window_size,
        3,
        Some(3),
    );
    ctx.defsubr(
        "process-tty-name",
        super::process::builtin_process_tty_name,
        1,
        Some(2),
    );
    ctx.defsubr(
        "process-plist",
        super::process::builtin_process_plist,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-plist",
        super::process::builtin_set_process_plist,
        0,
        None,
    );
    ctx.defsubr(
        "process-mark",
        super::process::builtin_process_mark,
        0,
        None,
    );
    ctx.defsubr(
        "process-type",
        super::process::builtin_process_type,
        1,
        Some(1),
    );
    ctx.defsubr(
        "process-thread",
        super::process::builtin_process_thread,
        0,
        None,
    );
    ctx.defsubr(
        "process-running-child-p",
        super::process::builtin_process_running_child_p,
        0,
        Some(1),
    );
    ctx.defsubr(
        "process-send-region",
        super::process::builtin_process_send_region,
        3,
        Some(3),
    );
    ctx.defsubr(
        "process-send-eof",
        super::process::builtin_process_send_eof,
        0,
        Some(1),
    );
    ctx.defsubr(
        "process-send-string",
        super::process::builtin_process_send_string,
        2,
        Some(2),
    );
    ctx.defsubr(
        "process-status",
        super::process::builtin_process_status,
        1,
        Some(1),
    );
    ctx.defsubr(
        "process-exit-status",
        super::process::builtin_process_exit_status,
        0,
        None,
    );
    ctx.defsubr(
        "process-list",
        super::process::builtin_process_list,
        0,
        Some(0),
    );
    ctx.defsubr(
        "process-name",
        super::process::builtin_process_name,
        1,
        Some(1),
    );
    ctx.defsubr(
        "process-buffer",
        super::process::builtin_process_buffer,
        1,
        Some(1),
    );
    ctx.defsubr("sleep-for", super::timer::builtin_sleep_for, 1, Some(2));
    // Timer functions (run-at-time, run-with-timer, run-with-idle-timer,
    // cancel-timer, timerp, timer-activate) are NOT C primitives in GNU
    // Emacs — they're defined in timer.el as Elisp functions.
    // The C layer only provides timer-check (in keyboard.rs) which reads
    // timer-list / timer-idle-list and calls timer-event-handler.
    // Registering them as Rust builtins would shadow the Elisp definitions
    // and create an incompatible parallel timer system.
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "add-variable-watcher",
            super::advice::builtin_add_variable_watcher,
            2,
            Some(2),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "remove-variable-watcher",
            super::advice::builtin_remove_variable_watcher,
            2,
            Some(2),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "get-variable-watchers",
            super::advice::builtin_get_variable_watchers,
            1,
            Some(1),
        ),
    );
    ctx.defsubr(
        "modify-syntax-entry",
        super::syntax::builtin_modify_syntax_entry,
        2,
        Some(3),
    );
    ctx.defsubr(
        "syntax-table",
        super::syntax::builtin_syntax_table,
        0,
        Some(0),
    );
    ctx.defsubr(
        "set-syntax-table",
        super::syntax::builtin_set_syntax_table,
        1,
        Some(1),
    );
    ctx.defsubr(
        "char-syntax",
        super::syntax::builtin_char_syntax,
        1,
        Some(1),
    );
    ctx.defsubr(
        "matching-paren",
        super::syntax::builtin_matching_paren,
        1,
        Some(1),
    );
    ctx.defsubr(
        "forward-comment",
        super::syntax::builtin_forward_comment,
        1,
        Some(1),
    );
    ctx.defsubr(
        "backward-prefix-chars",
        super::syntax::builtin_backward_prefix_chars,
        0,
        Some(0),
    );
    ctx.defsubr(
        "forward-word",
        super::syntax::builtin_forward_word,
        0,
        Some(1),
    );
    ctx.defsubr("scan-lists", super::syntax::builtin_scan_lists, 3, Some(3));
    ctx.defsubr("scan-sexps", super::syntax::builtin_scan_sexps, 2, Some(2));
    ctx.defsubr(
        "parse-partial-sexp",
        super::syntax::builtin_parse_partial_sexp,
        2,
        Some(6),
    );
    ctx.defsubr(
        "skip-syntax-forward",
        super::syntax::builtin_skip_syntax_forward,
        1,
        Some(2),
    );
    ctx.defsubr(
        "skip-syntax-backward",
        super::syntax::builtin_skip_syntax_backward,
        1,
        Some(2),
    );
    ctx.defsubr(
        "start-kbd-macro",
        super::kmacro::builtin_start_kbd_macro,
        1,
        Some(2),
    );
    ctx.defsubr(
        "end-kbd-macro",
        super::kmacro::builtin_end_kbd_macro,
        0,
        Some(2),
    );
    ctx.defsubr(
        "call-last-kbd-macro",
        super::kmacro::builtin_call_last_kbd_macro,
        0,
        Some(2),
    );
    ctx.defsubr(
        "execute-kbd-macro",
        super::kmacro::builtin_execute_kbd_macro,
        1,
        Some(3),
    );
    ctx.defsubr(
        "store-kbd-macro-event",
        super::kmacro::builtin_store_kbd_macro_event,
        1,
        Some(1),
    );
    ctx.defsubr(
        "defining-kbd-macro",
        super::kmacro::builtin_defining_kbd_macro,
        1,
        Some(2),
    );
    ctx.defsubr(
        "defining-kbd-macro-p",
        super::kmacro::builtin_defining_kbd_macro_p,
        0,
        Some(0),
    );
    ctx.defsubr(
        "executing-kbd-macro-p",
        super::kmacro::builtin_executing_kbd_macro_p,
        0,
        Some(0),
    );
    ctx.defsubr(
        "kmacro-set-counter",
        super::kmacro::builtin_kmacro_set_counter,
        1,
        Some(1),
    );
    ctx.defsubr(
        "kmacro-add-counter",
        super::kmacro::builtin_kmacro_add_counter,
        1,
        Some(1),
    );
    ctx.defsubr(
        "kmacro-set-format",
        super::kmacro::builtin_kmacro_set_format,
        1,
        Some(1),
    );
    ctx.defsubr(
        "put-text-property",
        super::textprop::builtin_put_text_property,
        0,
        None,
    );
    ctx.defsubr(
        "get-text-property",
        super::textprop::builtin_get_text_property,
        2,
        Some(3),
    );
    ctx.defsubr(
        "get-char-property",
        super::textprop::builtin_get_char_property,
        2,
        Some(3),
    );
    ctx.defsubr("get-pos-property", builtin_get_pos_property, 2, Some(3));
    ctx.defsubr(
        "add-face-text-property",
        super::textprop::builtin_add_face_text_property,
        3,
        Some(5),
    );
    ctx.defsubr(
        "add-text-properties",
        super::textprop::builtin_add_text_properties,
        3,
        Some(4),
    );
    ctx.defsubr(
        "set-text-properties",
        super::textprop::builtin_set_text_properties,
        3,
        Some(4),
    );
    ctx.defsubr(
        "remove-text-properties",
        super::textprop::builtin_remove_text_properties,
        0,
        None,
    );
    ctx.defsubr(
        "text-properties-at",
        super::textprop::builtin_text_properties_at,
        1,
        Some(2),
    );
    ctx.defsubr(
        "get-display-property",
        super::textprop::builtin_get_display_property,
        2,
        Some(4),
    );
    ctx.defsubr(
        "next-single-char-property-change",
        builtin_next_single_char_property_change,
        2,
        Some(4),
    );
    ctx.defsubr(
        "previous-single-char-property-change",
        builtin_previous_single_char_property_change,
        0,
        None,
    );
    ctx.defsubr(
        "next-property-change",
        super::textprop::builtin_next_property_change,
        1,
        Some(3),
    );
    ctx.defsubr(
        "next-char-property-change",
        builtin_next_char_property_change,
        1,
        Some(2),
    );
    ctx.defsubr(
        "previous-property-change",
        builtin_previous_property_change,
        1,
        Some(3),
    );
    ctx.defsubr(
        "previous-char-property-change",
        builtin_previous_char_property_change,
        1,
        Some(2),
    );
    ctx.defsubr(
        "text-property-any",
        super::textprop::builtin_text_property_any,
        0,
        None,
    );
    ctx.defsubr(
        "text-property-not-all",
        super::textprop::builtin_text_property_not_all,
        0,
        None,
    );
    ctx.defsubr(
        "next-overlay-change",
        super::textprop::builtin_next_overlay_change,
        1,
        Some(1),
    );
    ctx.defsubr(
        "previous-overlay-change",
        super::textprop::builtin_previous_overlay_change,
        1,
        Some(1),
    );
    ctx.defsubr(
        "make-overlay",
        super::textprop::builtin_make_overlay,
        2,
        Some(5),
    );
    ctx.defsubr(
        "delete-overlay",
        super::textprop::builtin_delete_overlay,
        1,
        Some(1),
    );
    ctx.defsubr(
        "overlay-put",
        super::textprop::builtin_overlay_put,
        3,
        Some(3),
    );
    ctx.defsubr(
        "overlay-get",
        super::textprop::builtin_overlay_get,
        2,
        Some(2),
    );
    ctx.defsubr(
        "overlays-at",
        super::textprop::builtin_overlays_at,
        1,
        Some(2),
    );
    ctx.defsubr(
        "overlays-in",
        super::textprop::builtin_overlays_in,
        2,
        Some(2),
    );
    ctx.defsubr(
        "move-overlay",
        super::textprop::builtin_move_overlay,
        3,
        Some(4),
    );
    ctx.defsubr(
        "overlay-start",
        super::textprop::builtin_overlay_start,
        1,
        Some(1),
    );
    ctx.defsubr(
        "overlay-end",
        super::textprop::builtin_overlay_end,
        1,
        Some(1),
    );
    ctx.defsubr(
        "overlay-buffer",
        super::textprop::builtin_overlay_buffer,
        1,
        Some(1),
    );
    ctx.defsubr(
        "overlay-properties",
        super::textprop::builtin_overlay_properties,
        1,
        Some(1),
    );
    ctx.defsubr("overlayp", super::textprop::builtin_overlayp, 1, Some(1));
    ctx.defsubr("bobp", super::navigation::builtin_bobp, 0, Some(0));
    ctx.defsubr("eobp", super::navigation::builtin_eobp, 0, Some(0));
    ctx.defsubr("bolp", super::navigation::builtin_bolp, 0, Some(0));
    ctx.defsubr("eolp", super::navigation::builtin_eolp, 0, Some(0));
    ctx.defsubr("pos-bol", builtin_pos_bol, 0, Some(1));
    ctx.defsubr(
        "line-end-position",
        super::navigation::builtin_line_end_position,
        0,
        Some(1),
    );
    ctx.defsubr("pos-eol", builtin_pos_eol, 0, Some(1));
    ctx.defsubr(
        "line-number-at-pos",
        super::navigation::builtin_line_number_at_pos,
        0,
        Some(2),
    );
    ctx.defsubr(
        "forward-line",
        super::navigation::builtin_forward_line,
        0,
        Some(1),
    );
    ctx.defsubr(
        "beginning-of-line",
        super::navigation::builtin_beginning_of_line,
        0,
        Some(1),
    );
    ctx.defsubr(
        "end-of-line",
        super::navigation::builtin_end_of_line,
        0,
        Some(1),
    );
    ctx.defsubr(
        "forward-char",
        super::navigation::builtin_forward_char,
        0,
        Some(1),
    );
    ctx.defsubr(
        "backward-char",
        super::navigation::builtin_backward_char,
        0,
        Some(1),
    );
    ctx.defsubr(
        "skip-chars-forward",
        super::navigation::builtin_skip_chars_forward,
        1,
        Some(2),
    );
    ctx.defsubr(
        "skip-chars-backward",
        super::navigation::builtin_skip_chars_backward,
        1,
        Some(2),
    );
    ctx.defsubr(
        "mark-marker",
        super::marker::builtin_mark_marker,
        0,
        Some(0),
    );
    ctx.defsubr(
        "region-beginning",
        super::navigation::builtin_region_beginning,
        0,
        Some(0),
    );
    ctx.defsubr(
        "region-end",
        super::navigation::builtin_region_end,
        0,
        Some(0),
    );
    ctx.defsubr(
        "transient-mark-mode",
        super::navigation::builtin_transient_mark_mode,
        0,
        None,
    );
    ctx.defsubr(
        "make-local-variable",
        super::custom::builtin_make_local_variable,
        1,
        Some(1),
    );
    ctx.defsubr(
        "local-variable-p",
        super::custom::builtin_local_variable_p,
        1,
        Some(2),
    );
    ctx.defsubr(
        "buffer-local-variables",
        super::custom::builtin_buffer_local_variables,
        0,
        None,
    );
    ctx.defsubr(
        "kill-local-variable",
        super::custom::builtin_kill_local_variable,
        0,
        None,
    );
    ctx.defsubr(
        "default-value",
        super::custom::builtin_default_value,
        1,
        Some(1),
    );
    ctx.defsubr(
        "set-default",
        super::custom::builtin_set_default,
        2,
        Some(2),
    );
    ctx.defsubr(
        "set-default-toplevel-value",
        builtin_set_default_toplevel_value,
        2,
        Some(2),
    );
    ctx.defsubr("autoload", super::autoload::builtin_autoload, 2, Some(5));
    ctx.defsubr(
        "autoload-do-load",
        super::autoload::builtin_autoload_do_load,
        1,
        Some(3),
    );
    ctx.defsubr("symbol-file", super::autoload::builtin_symbol_file, 0, None);
    ctx.defsubr(
        "downcase-region",
        super::casefiddle::builtin_downcase_region,
        2,
        Some(3),
    );
    ctx.defsubr(
        "upcase-region",
        super::casefiddle::builtin_upcase_region,
        2,
        Some(3),
    );
    ctx.defsubr(
        "capitalize-region",
        super::casefiddle::builtin_capitalize_region,
        2,
        Some(3),
    );
    ctx.defsubr(
        "downcase-word",
        super::casefiddle::builtin_downcase_word,
        1,
        Some(1),
    );
    ctx.defsubr(
        "upcase-word",
        super::casefiddle::builtin_upcase_word,
        1,
        Some(1),
    );
    ctx.defsubr(
        "capitalize-word",
        super::casefiddle::builtin_capitalize_word,
        1,
        Some(1),
    );
    ctx.defsubr("indent-to", super::indent::builtin_indent_to, 1, Some(2));
    ctx.defsubr(
        "selected-window",
        super::window_cmds::builtin_selected_window,
        0,
        Some(0),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "old-selected-window",
            super::window_cmds::builtin_old_selected_window,
            0,
            Some(0),
        ),
    );
    ctx.defsubr(
        "minibuffer-window",
        super::window_cmds::builtin_minibuffer_window,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-parameter",
        super::window_cmds::builtin_window_parameter,
        2,
        Some(2),
    );
    ctx.defsubr(
        "set-window-parameter",
        super::window_cmds::builtin_set_window_parameter,
        3,
        Some(3),
    );
    ctx.defsubr(
        "window-parameters",
        super::window_cmds::builtin_window_parameters,
        0,
        None,
    );
    ctx.defsubr(
        "window-parent",
        super::window_cmds::builtin_window_parent,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-top-child",
        super::window_cmds::builtin_window_top_child,
        0,
        Some(1),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "window-left-child",
            super::window_cmds::builtin_window_left_child,
            0,
            Some(1),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "window-next-sibling",
            super::window_cmds::builtin_window_next_sibling,
            0,
            Some(1),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "window-prev-sibling",
            super::window_cmds::builtin_window_prev_sibling,
            0,
            Some(1),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "window-normal-size",
            super::window_cmds::builtin_window_normal_size,
            0,
            Some(2),
        ),
    );
    ctx.defsubr(
        "window-display-table",
        super::window_cmds::builtin_window_display_table,
        0,
        None,
    );
    ctx.defsubr(
        "window-cursor-type",
        super::window_cmds::builtin_window_cursor_type,
        0,
        None,
    );
    ctx.defsubr(
        "window-buffer",
        super::window_cmds::builtin_window_buffer,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-start",
        super::window_cmds::builtin_window_start,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-end",
        super::window_cmds::builtin_window_end,
        0,
        Some(2),
    );
    ctx.defsubr(
        "window-point",
        super::window_cmds::builtin_window_point,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-use-time",
        super::window_cmds::builtin_window_use_time,
        0,
        Some(1),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "window-bump-use-time",
            super::window_cmds::builtin_window_bump_use_time,
            0,
            Some(1),
        ),
    );
    ctx.defsubr(
        "window-old-point",
        super::window_cmds::builtin_window_old_point,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-old-buffer",
        super::window_cmds::builtin_window_old_buffer,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-prev-buffers",
        super::window_cmds::builtin_window_prev_buffers,
        0,
        None,
    );
    ctx.defsubr(
        "window-next-buffers",
        super::window_cmds::builtin_window_next_buffers,
        0,
        None,
    );
    ctx.defsubr(
        "window-left-column",
        super::window_cmds::builtin_window_left_column,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-top-line",
        super::window_cmds::builtin_window_top_line,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-pixel-left",
        super::window_cmds::builtin_window_pixel_left,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-pixel-top",
        super::window_cmds::builtin_window_pixel_top,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-hscroll",
        super::window_cmds::builtin_window_hscroll,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-vscroll",
        super::window_cmds::builtin_window_vscroll,
        0,
        Some(2),
    );
    ctx.defsubr(
        "window-margins",
        super::window_cmds::builtin_window_margins,
        0,
        None,
    );
    ctx.defsubr(
        "window-fringes",
        super::window_cmds::builtin_window_fringes,
        0,
        None,
    );
    ctx.defsubr(
        "window-scroll-bars",
        super::window_cmds::builtin_window_scroll_bars,
        0,
        None,
    );
    ctx.defsubr(
        "window-pixel-height",
        super::window_cmds::builtin_window_pixel_height,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-pixel-width",
        super::window_cmds::builtin_window_pixel_width,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-edges",
        super::window_cmds::builtin_window_edges,
        0,
        Some(4),
    );
    ctx.defsubr(
        "window-pixel-edges",
        super::window_cmds::builtin_window_pixel_edges,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-absolute-pixel-edges",
        super::window_cmds::builtin_window_absolute_pixel_edges,
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-body-height",
        super::window_cmds::builtin_window_body_height,
        0,
        Some(2),
    );
    ctx.defsubr(
        "window-body-width",
        super::window_cmds::builtin_window_body_width,
        0,
        Some(2),
    );
    ctx.defsubr(
        "window-text-height",
        super::window_cmds::builtin_window_text_height,
        0,
        None,
    );
    ctx.defsubr(
        "window-text-width",
        super::window_cmds::builtin_window_text_width,
        0,
        None,
    );
    ctx.defsubr(
        "window-total-height",
        super::window_cmds::builtin_window_total_height,
        0,
        Some(2),
    );
    ctx.defsubr(
        "window-total-width",
        super::window_cmds::builtin_window_total_width,
        0,
        Some(2),
    );
    ctx.defsubr(
        "window-list",
        super::window_cmds::builtin_window_list,
        0,
        Some(3),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "window-list-1",
            super::window_cmds::builtin_window_list_1,
            0,
            Some(3),
        ),
    );
    ctx.defsubr(
        "get-buffer-window",
        super::window_cmds::builtin_get_buffer_window,
        0,
        Some(2),
    );
    ctx.defsubr(
        "window-dedicated-p",
        super::window_cmds::builtin_window_dedicated_p,
        0,
        None,
    );
    ctx.defsubr(
        "window-minibuffer-p",
        super::window_cmds::builtin_window_minibuffer_p,
        0,
        None,
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "window-at",
            super::window_cmds::builtin_window_at,
            2,
            Some(3),
        ),
    );
    ctx.defsubr(
        "window-live-p",
        super::window_cmds::builtin_window_live_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "set-window-start",
        super::window_cmds::builtin_set_window_start,
        2,
        Some(3),
    );
    ctx.defsubr(
        "set-window-hscroll",
        super::window_cmds::builtin_set_window_hscroll,
        2,
        Some(2),
    );
    ctx.defsubr(
        "set-window-margins",
        super::window_cmds::builtin_set_window_margins,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-fringes",
        super::window_cmds::builtin_set_window_fringes,
        2,
        Some(5),
    );
    ctx.defsubr(
        "set-window-vscroll",
        super::window_cmds::builtin_set_window_vscroll,
        2,
        Some(4),
    );
    ctx.defsubr(
        "set-window-point",
        super::window_cmds::builtin_set_window_point,
        2,
        Some(2),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "split-window-internal",
            super::window_cmds::builtin_split_window_internal,
            4,
            Some(5),
        ),
    );
    ctx.defsubr(
        "delete-window",
        super::window_cmds::builtin_delete_window,
        0,
        None,
    );
    ctx.defsubr(
        "delete-other-windows",
        super::window_cmds::builtin_delete_other_windows,
        0,
        None,
    );
    ctx.defsubr(
        "fit-window-to-buffer",
        super::window_cmds::builtin_fit_window_to_buffer,
        0,
        Some(6),
    );
    ctx.defsubr(
        "select-window",
        super::window_cmds::builtin_select_window,
        1,
        Some(2),
    );
    ctx.defsubr(
        "scroll-up",
        super::window_cmds::builtin_scroll_up,
        0,
        Some(1),
    );
    ctx.defsubr(
        "scroll-down",
        super::window_cmds::builtin_scroll_down,
        0,
        Some(1),
    );
    ctx.defsubr(
        "scroll-left",
        super::window_cmds::builtin_scroll_left,
        0,
        Some(2),
    );
    ctx.defsubr(
        "scroll-right",
        super::window_cmds::builtin_scroll_right,
        0,
        Some(2),
    );
    ctx.defsubr(
        "window-resize-apply",
        super::window_cmds::builtin_window_resize_apply,
        0,
        Some(2),
    );
    ctx.defsubr("recenter", super::window_cmds::builtin_recenter, 0, Some(2));
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "vertical-motion",
            builtin_vertical_motion,
            1,
            Some(3),
        ),
    );
    ctx.defsubr(
        "next-window",
        super::window_cmds::builtin_next_window,
        0,
        Some(3),
    );
    ctx.defsubr(
        "previous-window",
        super::window_cmds::builtin_previous_window,
        0,
        Some(3),
    );
    ctx.defsubr(
        "set-window-buffer",
        super::window_cmds::builtin_set_window_buffer,
        2,
        Some(3),
    );
    ctx.defsubr(
        "current-window-configuration",
        super::window_cmds::builtin_current_window_configuration,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-configuration",
        super::window_cmds::builtin_set_window_configuration,
        1,
        Some(3),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "old-selected-frame",
            builtin_old_selected_frame,
            0,
            Some(0),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    ctx.defsubr(
        "selected-frame",
        super::window_cmds::builtin_selected_frame,
        0,
        Some(0),
    );
    ctx.defsubr(
        "mouse-pixel-position",
        builtin_mouse_pixel_position,
        0,
        Some(0),
    );
    ctx.defsubr("mouse-position", builtin_mouse_position, 0, Some(0));
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "next-frame",
            builtin_next_frame,
            0,
            Some(2),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "previous-frame",
            builtin_previous_frame,
            0,
            Some(2),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    ctx.defsubr(
        "select-frame",
        super::window_cmds::builtin_select_frame,
        1,
        Some(2),
    );
    ctx.defsubr(
        "last-nonminibuffer-frame",
        super::window_cmds::builtin_selected_frame,
        0,
        None,
    );
    ctx.defsubr(
        "visible-frame-list",
        super::window_cmds::builtin_visible_frame_list,
        0,
        None,
    );
    ctx.defsubr(
        "frame-list",
        super::window_cmds::builtin_frame_list,
        0,
        None,
    );
    ctx.defsubr(
        "x-create-frame",
        super::window_cmds::builtin_x_create_frame,
        1,
        Some(1),
    );
    ctx.defsubr(
        "make-frame-visible",
        super::window_cmds::builtin_make_frame_visible,
        0,
        Some(1),
    );
    ctx.defsubr(
        "make-frame",
        super::window_cmds::builtin_make_frame,
        0,
        None,
    );
    ctx.defsubr(
        "iconify-frame",
        super::window_cmds::builtin_iconify_frame,
        0,
        Some(1),
    );
    ctx.defsubr(
        "delete-frame",
        super::window_cmds::builtin_delete_frame,
        0,
        Some(2),
    );
    ctx.defsubr(
        "frame-char-height",
        super::window_cmds::builtin_frame_char_height,
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-char-width",
        super::window_cmds::builtin_frame_char_width,
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-native-height",
        super::window_cmds::builtin_frame_native_height,
        0,
        None,
    );
    ctx.defsubr(
        "frame-native-width",
        super::window_cmds::builtin_frame_native_width,
        0,
        None,
    );
    ctx.defsubr(
        "frame-text-cols",
        super::window_cmds::builtin_frame_text_cols,
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-text-height",
        super::window_cmds::builtin_frame_text_height,
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-text-lines",
        super::window_cmds::builtin_frame_text_lines,
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-text-width",
        super::window_cmds::builtin_frame_text_width,
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-total-cols",
        super::window_cmds::builtin_frame_total_cols,
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-total-lines",
        super::window_cmds::builtin_frame_total_lines,
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-position",
        super::window_cmds::builtin_frame_position,
        0,
        None,
    );
    ctx.defsubr(
        "frame-parameters",
        super::window_cmds::builtin_frame_parameters,
        0,
        Some(1),
    );
    ctx.defsubr(
        "set-frame-height",
        super::window_cmds::builtin_set_frame_height,
        2,
        Some(4),
    );
    ctx.defsubr(
        "set-frame-width",
        super::window_cmds::builtin_set_frame_width,
        2,
        Some(4),
    );
    ctx.defsubr(
        "set-frame-size",
        super::window_cmds::builtin_set_frame_size,
        3,
        Some(4),
    );
    ctx.defsubr(
        "set-frame-position",
        super::window_cmds::builtin_set_frame_position,
        3,
        Some(3),
    );
    ctx.defsubr(
        "frame-visible-p",
        super::window_cmds::builtin_frame_visible_p,
        0,
        None,
    );
    ctx.defsubr(
        "frame-live-p",
        super::window_cmds::builtin_frame_live_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "frame-first-window",
        super::window_cmds::builtin_frame_first_window,
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-root-window",
        super::window_cmds::builtin_frame_root_window,
        0,
        Some(1),
    );
    ctx.defsubr("windowp", super::window_cmds::builtin_windowp, 1, Some(1));
    ctx.defsubr(
        "window-valid-p",
        super::window_cmds::builtin_window_valid_p,
        1,
        Some(1),
    );
    ctx.defsubr("framep", super::window_cmds::builtin_framep, 1, Some(1));
    ctx.defsubr(
        "window-frame",
        super::window_cmds::builtin_window_frame,
        0,
        Some(1),
    );
    ctx.defsubr("frame-id", builtin_frame_id, 0, Some(1));
    ctx.defsubr("frame-root-frame", builtin_frame_root_frame, 0, None);
    ctx.defsubr(
        "x-open-connection",
        super::display::builtin_x_open_connection,
        1,
        Some(3),
    );
    ctx.defsubr(
        "x-get-resource",
        super::display::builtin_x_get_resource,
        2,
        Some(4),
    );
    ctx.defsubr(
        "x-list-fonts",
        super::display::builtin_x_list_fonts,
        1,
        Some(5),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "window-system",
            super::display::builtin_window_system,
            0,
            Some(1),
        ),
    );
    ctx.defsubr("current-idle-time", builtin_current_idle_time, 0, Some(0));
    ctx.defsubr(
        "x-server-version",
        super::display::builtin_x_server_version,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-server-input-extension-version",
        super::display::builtin_x_server_input_extension_version,
        0,
        None,
    );
    ctx.defsubr(
        "x-server-vendor",
        super::display::builtin_x_server_vendor,
        0,
        Some(1),
    );
    ctx.defsubr(
        "display-color-cells",
        super::display::builtin_display_color_cells,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-mm-height",
        super::display::builtin_x_display_mm_height,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-display-mm-width",
        super::display::builtin_x_display_mm_width,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-display-planes",
        super::display::builtin_x_display_planes,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-display-screens",
        super::display::builtin_x_display_screens,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-close-connection",
        super::display::builtin_x_close_connection,
        1,
        Some(1),
    );
    ctx.defsubr(
        "call-interactively",
        super::interactive::builtin_call_interactively,
        1,
        Some(3),
    );
    ctx.defsubr(
        "commandp",
        super::interactive::builtin_commandp_interactive,
        1,
        Some(2),
    );
    ctx.defsubr(
        "command-remapping",
        super::interactive::builtin_command_remapping,
        1,
        Some(3),
    );
    ctx.defsubr(
        "self-insert-command",
        super::interactive::builtin_self_insert_command,
        1,
        Some(2),
    );
    ctx.defsubr(
        "key-binding",
        super::interactive::builtin_key_binding,
        1,
        Some(4),
    );
    ctx.defsubr(
        "where-is-internal",
        super::interactive::builtin_where_is_internal,
        1,
        Some(5),
    );
    ctx.defsubr(
        "this-command-keys",
        super::interactive::builtin_this_command_keys,
        0,
        Some(0),
    );
    ctx.defsubr("format", builtin_format, 1, None);
    ctx.defsubr("format-message", builtin_format_message, 1, None);
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("message-box", builtin_message_box, 1, None),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("message-or-box", builtin_message_or_box, 1, None),
    );
    ctx.defsubr("current-message", builtin_current_message, 0, Some(0));
    ctx.defsubr(
        "read-from-string",
        super::reader::builtin_read_from_string,
        1,
        Some(3),
    );
    ctx.defsubr("read", super::reader::builtin_read, 0, Some(1));
    ctx.defsubr(
        "read-from-minibuffer",
        super::reader::builtin_read_from_minibuffer,
        1,
        Some(7),
    );
    ctx.defsubr(
        "read-string",
        super::reader::builtin_read_string,
        1,
        Some(5),
    );
    ctx.defsubr(
        "completing-read",
        super::reader::builtin_completing_read,
        2,
        Some(8),
    );
    ctx.defsubr(
        "read-number",
        super::reader::builtin_read_number,
        1,
        Some(2),
    );
    ctx.defsubr(
        "read-buffer",
        super::minibuffer::builtin_read_buffer,
        1,
        Some(4),
    );
    ctx.defsubr(
        "read-command",
        super::minibuffer::builtin_read_command,
        1,
        Some(2),
    );
    ctx.defsubr(
        "read-variable",
        super::minibuffer::builtin_read_variable,
        1,
        Some(2),
    );
    ctx.defsubr(
        "try-completion",
        super::minibuffer::builtin_try_completion,
        2,
        Some(3),
    );
    ctx.defsubr(
        "all-completions",
        super::minibuffer::builtin_all_completions,
        2,
        Some(3),
    );
    ctx.defsubr(
        "test-completion",
        super::minibuffer::builtin_test_completion,
        2,
        Some(3),
    );
    ctx.defsubr(
        "input-pending-p",
        super::reader::builtin_input_pending_p,
        0,
        Some(1),
    );
    ctx.defsubr(
        "discard-input",
        super::reader::builtin_discard_input,
        0,
        Some(0),
    );
    ctx.defsubr(
        "current-input-mode",
        super::reader::builtin_current_input_mode,
        0,
        Some(0),
    );
    ctx.defsubr(
        "set-input-mode",
        super::reader::builtin_set_input_mode,
        3,
        Some(4),
    );
    ctx.defsubr(
        "set-input-interrupt-mode",
        super::reader::builtin_set_input_interrupt_mode,
        1,
        Some(1),
    );
    ctx.defsubr(
        "read-key-sequence",
        super::reader::builtin_read_key_sequence,
        1,
        Some(6),
    );
    ctx.defsubr(
        "read-key-sequence-vector",
        super::reader::builtin_read_key_sequence_vector,
        1,
        Some(6),
    );
    ctx.defsubr("recent-keys", builtin_recent_keys, 0, Some(1));
    ctx.defsubr(
        "minibufferp",
        super::minibuffer::builtin_minibufferp_ctx,
        0,
        Some(2),
    );
    ctx.defsubr(
        "minibuffer-contents",
        super::minibuffer::builtin_minibuffer_contents_ctx,
        0,
        None,
    );
    ctx.defsubr(
        "minibuffer-contents-no-properties",
        super::minibuffer::builtin_minibuffer_contents_no_properties_ctx,
        0,
        None,
    );
    ctx.defsubr(
        "minibuffer-depth",
        super::minibuffer::builtin_minibuffer_depth_ctx,
        0,
        Some(0),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("princ", builtin_princ, 1, Some(2)),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("prin1", builtin_prin1, 1, Some(3)),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "prin1-to-string",
            builtin_prin1_to_string,
            1,
            Some(3),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("print", builtin_print, 1, Some(2)),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("terpri", builtin_terpri, 0, Some(2)),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("write-char", builtin_write_char, 1, Some(2)),
    );
    ctx.defsubr(
        "backtrace--locals",
        super::misc::builtin_backtrace_locals,
        1,
        Some(2),
    );
    ctx.defsubr(
        "backtrace-debug",
        super::misc::builtin_backtrace_debug,
        2,
        Some(3),
    );
    ctx.defsubr(
        "backtrace-eval",
        super::misc::builtin_backtrace_eval,
        2,
        Some(3),
    );
    ctx.defsubr(
        "backtrace-frame--internal",
        super::misc::builtin_backtrace_frame_internal,
        3,
        Some(3),
    );
    ctx.defsubr(
        "recursion-depth",
        super::misc::builtin_recursion_depth,
        0,
        Some(0),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("kill-emacs", builtin_kill_emacs, 0, Some(2)),
    );
    ctx.defsubr(
        "exit-recursive-edit",
        super::minibuffer::builtin_exit_recursive_edit,
        0,
        Some(0),
    );
    ctx.defsubr(
        "abort-recursive-edit",
        super::minibuffer::builtin_abort_recursive_edit,
        0,
        Some(0),
    );
    ctx.defsubr(
        "make-thread",
        super::threads::builtin_make_thread,
        1,
        Some(3),
    );
    ctx.defsubr(
        "thread-join",
        super::threads::builtin_thread_join,
        1,
        Some(1),
    );
    ctx.defsubr(
        "thread-yield",
        super::threads::builtin_thread_yield,
        0,
        Some(0),
    );
    ctx.defsubr(
        "thread-name",
        super::threads::builtin_thread_name,
        1,
        Some(1),
    );
    ctx.defsubr(
        "thread-live-p",
        super::threads::builtin_thread_live_p,
        1,
        Some(1),
    );
    ctx.defsubr("threadp", super::threads::builtin_threadp, 1, Some(1));
    ctx.defsubr(
        "thread-signal",
        super::threads::builtin_thread_signal,
        3,
        Some(3),
    );
    ctx.defsubr(
        "current-thread",
        super::threads::builtin_current_thread,
        0,
        Some(0),
    );
    ctx.defsubr(
        "all-threads",
        super::threads::builtin_all_threads,
        0,
        Some(0),
    );
    ctx.defsubr(
        "thread-last-error",
        super::threads::builtin_thread_last_error,
        0,
        Some(1),
    );
    ctx.defsubr("make-mutex", super::threads::builtin_make_mutex, 0, Some(1));
    ctx.defsubr("mutex-name", super::threads::builtin_mutex_name, 1, Some(1));
    ctx.defsubr("mutex-lock", super::threads::builtin_mutex_lock, 1, Some(1));
    ctx.defsubr(
        "mutex-unlock",
        super::threads::builtin_mutex_unlock,
        1,
        Some(1),
    );
    ctx.defsubr("mutexp", super::threads::builtin_mutexp, 1, Some(1));
    ctx.defsubr(
        "make-condition-variable",
        super::threads::builtin_make_condition_variable,
        1,
        Some(2),
    );
    ctx.defsubr(
        "condition-variable-p",
        super::threads::builtin_condition_variable_p,
        0,
        None,
    );
    ctx.defsubr(
        "condition-name",
        super::threads::builtin_condition_name,
        1,
        Some(1),
    );
    ctx.defsubr(
        "condition-mutex",
        super::threads::builtin_condition_mutex,
        1,
        Some(1),
    );
    ctx.defsubr(
        "condition-wait",
        super::threads::builtin_condition_wait,
        1,
        Some(1),
    );
    ctx.defsubr(
        "condition-notify",
        super::threads::builtin_condition_notify,
        1,
        Some(2),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "undo-boundary",
            super::undo::builtin_undo_boundary,
            0,
            Some(0),
        ),
    );
    ctx.defsubr(
        "primitive-undo",
        super::undo::builtin_primitive_undo,
        2,
        Some(2),
    );
    ctx.defsubr("undo", super::undo::builtin_undo, 0, Some(1));
    ctx.defsubr(
        "buffer-disable-undo",
        builtin_buffer_disable_undo,
        0,
        Some(1),
    );
    ctx.defsubr("maphash", super::hashtab::builtin_maphash, 2, Some(2));
    ctx.defsubr("mapatoms", super::hashtab::builtin_mapatoms, 1, Some(2));
    ctx.defsubr("unintern", super::hashtab::builtin_unintern, 2, Some(2));
    ctx.defsubr("set-marker", super::marker::builtin_set_marker, 2, Some(3));
    ctx.defsubr(
        "move-marker",
        super::marker::builtin_move_marker,
        2,
        Some(3),
    );
    ctx.defsubr(
        "marker-position",
        super::marker::builtin_marker_position,
        1,
        Some(1),
    );
    ctx.defsubr(
        "marker-buffer",
        super::marker::builtin_marker_buffer,
        1,
        Some(1),
    );
    ctx.defsubr(
        "copy-marker",
        super::marker::builtin_copy_marker,
        0,
        Some(2),
    );
    ctx.defsubr(
        "point-marker",
        super::marker::builtin_point_marker,
        0,
        Some(0),
    );
    ctx.defsubr(
        "point-min-marker",
        super::marker::builtin_point_min_marker,
        0,
        Some(0),
    );
    ctx.defsubr(
        "point-max-marker",
        super::marker::builtin_point_max_marker,
        0,
        Some(0),
    );
    ctx.defsubr(
        "current-case-table",
        super::casetab::builtin_current_case_table,
        0,
        Some(0),
    );
    ctx.defsubr(
        "standard-case-table",
        super::casetab::builtin_standard_case_table,
        0,
        Some(0),
    );
    ctx.defsubr(
        "set-case-table",
        super::casetab::builtin_set_case_table,
        1,
        Some(1),
    );
    ctx.defsubr(
        "define-category",
        super::category::builtin_define_category,
        2,
        Some(3),
    );
    ctx.defsubr(
        "category-docstring",
        super::category::builtin_category_docstring,
        1,
        Some(2),
    );
    ctx.defsubr(
        "modify-category-entry",
        super::category::builtin_modify_category_entry,
        2,
        Some(4),
    );
    ctx.defsubr(
        "char-category-set",
        super::category::builtin_char_category_set,
        1,
        Some(1),
    );
    ctx.defsubr(
        "category-table",
        super::category::builtin_category_table,
        0,
        Some(0),
    );
    ctx.defsubr(
        "set-category-table",
        super::category::builtin_set_category_table,
        1,
        Some(1),
    );
    ctx.defsubr(
        "map-char-table",
        super::chartable::builtin_map_char_table,
        2,
        Some(2),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("assoc", builtin_assoc, 2, Some(3)),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("plist-member", builtin_plist_member, 2, Some(3)),
    );
    ctx.defsubr(
        "json-parse-buffer",
        super::json::builtin_json_parse_buffer,
        0,
        None,
    );
    ctx.defsubr("json-insert", super::json::builtin_json_insert, 1, None);
    ctx.defsubr(
        "documentation",
        super::doc::builtin_documentation,
        1,
        Some(2),
    );
    ctx.defsubr(
        "documentation-property",
        super::doc::builtin_documentation_property,
        2,
        Some(3),
    );
    ctx.defsubr(
        "current-indentation",
        super::indent::builtin_current_indentation,
        0,
        None,
    );
    ctx.defsubr(
        "current-column",
        super::indent::builtin_current_column,
        0,
        Some(0),
    );
    ctx.defsubr(
        "move-to-column",
        super::indent::builtin_move_to_column,
        1,
        Some(2),
    );
    ctx.defsubr("eval-buffer", super::lread::builtin_eval_buffer, 0, Some(5));
    ctx.defsubr("eval-region", super::lread::builtin_eval_region, 2, Some(4));
    ctx.defsubr(
        "read-char-exclusive",
        super::lread::builtin_read_char_exclusive,
        0,
        Some(3),
    );
    ctx.defsubr(
        "insert-before-markers",
        super::editfns::builtin_insert_before_markers,
        0,
        None,
    );
    ctx.defsubr(
        "delete-char",
        super::editfns::builtin_delete_char,
        1,
        Some(2),
    );
    ctx.defsubr(
        "following-char",
        |eval, args| super::editfns::builtin_following_char(eval, args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "preceding-char",
        |eval, args| super::editfns::builtin_preceding_char(eval, args),
        0,
        Some(0),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "font-at",
            super::font::builtin_font_at,
            1,
            Some(3),
        ),
    );
    ctx.defsubr("face-font", super::font::builtin_face_font, 1, Some(3));
    ctx.defsubr(
        "access-file",
        super::fileio::builtin_access_file,
        2,
        Some(2),
    );
    ctx.defsubr(
        "expand-file-name",
        super::fileio::builtin_expand_file_name,
        1,
        Some(2),
    );
    ctx.defsubr(
        "delete-file-internal",
        super::fileio::builtin_delete_file_internal,
        1,
        Some(1),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "rename-file",
            super::fileio::builtin_rename_file,
            2,
            Some(3),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "copy-file",
            super::fileio::builtin_copy_file,
            2,
            Some(6),
        ),
    );
    ctx.defsubr(
        "add-name-to-file",
        super::fileio::builtin_add_name_to_file,
        2,
        Some(3),
    );
    ctx.defsubr(
        "make-symbolic-link",
        super::fileio::builtin_make_symbolic_link,
        2,
        Some(3),
    );
    ctx.defsubr(
        "directory-files",
        super::fileio::builtin_directory_files,
        1,
        Some(5),
    );
    ctx.defsubr(
        "file-attributes",
        super::dired::builtin_file_attributes,
        1,
        Some(2),
    );
    ctx.defsubr(
        "file-exists-p",
        super::fileio::builtin_file_exists_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "file-readable-p",
        super::fileio::builtin_file_readable_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "file-writable-p",
        super::fileio::builtin_file_writable_p,
        1,
        Some(1),
    );
    ctx.defsubr("file-acl", super::fileio::builtin_file_acl, 1, Some(1));
    ctx.defsubr(
        "file-executable-p",
        super::fileio::builtin_file_executable_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "file-locked-p",
        super::filelock::builtin_file_locked_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "file-selinux-context",
        super::fileio::builtin_file_selinux_context,
        0,
        None,
    );
    ctx.defsubr(
        "file-system-info",
        super::fileio::builtin_file_system_info,
        1,
        Some(1),
    );
    ctx.defsubr(
        "file-directory-p",
        super::fileio::builtin_file_directory_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "file-regular-p",
        super::fileio::builtin_file_regular_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "file-symlink-p",
        super::fileio::builtin_file_symlink_p,
        1,
        Some(1),
    );
    ctx.defsubr("file-modes", super::fileio::builtin_file_modes, 1, Some(2));
    ctx.defsubr(
        "set-file-modes",
        super::fileio::builtin_set_file_modes,
        2,
        Some(3),
    );
    ctx.defsubr(
        "set-file-times",
        super::fileio::builtin_set_file_times,
        1,
        Some(3),
    );
    ctx.defsubr(
        "error-message-string",
        super::errors::builtin_error_message_string,
        1,
        Some(1),
    );
    ctx.defsubr("char-equal", builtin_char_equal, 2, Some(2));
    ctx.defsubr("macrop", super::builtins::symbols::builtin_macrop, 0, None);
    ctx.defsubr(
        "set-process-inherit-coding-system-flag",
        super::process::builtin_set_process_inherit_coding_system_flag,
        0,
        None,
    );
    ctx.defsubr(
        "compute-motion",
        super::builtins::buffers::builtin_compute_motion,
        7,
        Some(7),
    );
    ctx.defsubr(
        "frame-parameter",
        super::window_cmds::builtin_frame_parameter,
        2,
        Some(2),
    );
    ctx.defsubr(
        "send-string-to-terminal",
        super::dispnew::pure::builtin_send_string_to_terminal,
        1,
        Some(2),
    );
    ctx.defsubr(
        "internal-show-cursor",
        super::dispnew::pure::builtin_internal_show_cursor,
        2,
        Some(2),
    );
    ctx.defsubr(
        "internal-show-cursor-p",
        super::dispnew::pure::builtin_internal_show_cursor_p,
        0,
        None,
    );
    ctx.defsubr(
        "redraw-frame",
        super::dispnew::pure::builtin_redraw_frame,
        0,
        Some(1),
    );
    ctx.defsubr(
        "display-supports-face-attributes-p",
        super::display::builtin_display_supports_face_attributes_p,
        1,
        Some(2),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "terminal-name",
            super::terminal::pure::builtin_terminal_name,
            0,
            Some(1),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "terminal-live-p",
            super::terminal::pure::builtin_terminal_live_p,
            1,
            Some(1),
        ),
    );
    ctx.defsubr(
        "terminal-parameter",
        super::terminal::pure::builtin_terminal_parameter,
        2,
        Some(2),
    );
    ctx.defsubr(
        "terminal-parameters",
        super::terminal::pure::builtin_terminal_parameters,
        0,
        Some(1),
    );
    ctx.defsubr(
        "set-terminal-parameter",
        super::terminal::pure::builtin_set_terminal_parameter,
        3,
        Some(3),
    );
    ctx.defsubr(
        "tty-type",
        super::terminal::pure::builtin_tty_type,
        0,
        Some(1),
    );
    ctx.defsubr(
        "tty-top-frame",
        super::terminal::pure::builtin_tty_top_frame,
        0,
        Some(1),
    );
    ctx.defsubr(
        "tty-display-color-p",
        super::terminal::pure::builtin_tty_display_color_p,
        0,
        None,
    );
    ctx.defsubr(
        "tty-display-color-cells",
        super::terminal::pure::builtin_tty_display_color_cells,
        0,
        None,
    );
    ctx.defsubr(
        "tty-no-underline",
        super::terminal::pure::builtin_tty_no_underline,
        0,
        Some(1),
    );
    ctx.defsubr(
        "controlling-tty-p",
        super::terminal::pure::builtin_controlling_tty_p,
        0,
        Some(1),
    );
    ctx.defsubr(
        "suspend-tty",
        super::terminal::pure::builtin_suspend_tty,
        0,
        Some(1),
    );
    ctx.defsubr(
        "resume-tty",
        super::terminal::pure::builtin_resume_tty,
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-terminal",
        super::terminal::pure::builtin_frame_terminal,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-display-monitor-attributes-list",
        super::display::builtin_x_display_monitor_attributes_list,
        0,
        None,
    );
    ctx.defsubr("read-char", super::reader::builtin_read_char, 0, Some(3));
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "minibuffer-innermost-command-loop-p",
            super::minibuffer::builtin_minibuffer_innermost_command_loop_p_ctx,
            0,
            Some(1),
        ),
    );
    ctx.defsubr(
        "recursive-edit",
        super::minibuffer::builtin_recursive_edit,
        0,
        Some(0),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "find-coding-systems-region-internal",
            super::coding::builtin_find_coding_systems_region_internal,
            2,
            Some(3),
        ),
    );
    ctx.defsubr(
        "posix-search-forward",
        super::builtins::search::builtin_posix_search_forward,
        1,
        Some(4),
    );
    ctx.defsubr(
        "posix-search-backward",
        super::builtins::search::builtin_posix_search_backward,
        1,
        Some(4),
    );
    ctx.defsubr("read-event", super::lread::builtin_read_event, 0, Some(3));
    ctx.defsubr("run-hooks", defsubr_run_hooks, 0, None);
    ctx.defsubr("load", defsubr_load, 1, Some(5));
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("message", defsubr_message, 1, None),
    );
    ctx.defsubr(
        "coding-system-aliases",
        defsubr_coding_system_aliases,
        1,
        Some(1),
    );
    ctx.defsubr(
        "coding-system-plist",
        defsubr_coding_system_plist,
        1,
        Some(1),
    );
    ctx.defsubr("coding-system-put", defsubr_coding_system_put, 3, Some(3));
    ctx.defsubr("coding-system-base", defsubr_coding_system_base, 1, Some(1));
    ctx.defsubr(
        "coding-system-eol-type",
        defsubr_coding_system_eol_type,
        0,
        None,
    );
    ctx.defsubr(
        "detect-coding-string",
        defsubr_detect_coding_string,
        1,
        Some(2),
    );
    ctx.defsubr(
        "detect-coding-region",
        defsubr_detect_coding_region,
        2,
        Some(3),
    );
    ctx.defsubr(
        "keyboard-coding-system",
        defsubr_keyboard_coding_system,
        0,
        Some(1),
    );
    ctx.defsubr(
        "terminal-coding-system",
        defsubr_terminal_coding_system,
        0,
        None,
    );
    ctx.defsubr(
        "coding-system-priority-list",
        defsubr_coding_system_priority_list,
        0,
        Some(1),
    );
    ctx.defsubr(
        "integer-or-marker-p",
        |_ctx, args| builtin_integer_or_marker_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "number-or-marker-p",
        |_ctx, args| builtin_number_or_marker_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "vector-or-char-table-p",
        |_ctx, args| builtin_vector_or_char_table_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "markerp",
        |_ctx, args| super::marker::builtin_markerp(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "marker-insertion-type",
        |_ctx, args| super::marker::builtin_marker_insertion_type(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-marker",
        |_ctx, args| super::marker::builtin_make_marker(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "bool-vector-p",
        |_ctx, args| super::chartable::builtin_bool_vector_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "make-category-set",
        |_ctx, args| super::category::builtin_make_category_set(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "function-equal",
        |_ctx, args| builtin_function_equal(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "module-function-p",
        |_ctx, args| builtin_module_function_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "user-ptrp",
        |_ctx, args| builtin_user_ptrp(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "symbol-with-pos-p",
        |_ctx, args| builtin_symbol_with_pos_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "symbol-with-pos-pos",
        |_ctx, args| builtin_symbol_with_pos_pos(args),
        1,
        Some(1),
    );
    ctx.defsubr("length<", |_ctx, args| builtin_length_lt(args), 2, Some(2));
    ctx.defsubr("length=", |_ctx, args| builtin_length_eq(args), 2, Some(2));
    ctx.defsubr("length>", |_ctx, args| builtin_length_gt(args), 2, Some(2));
    ctx.defsubr(
        "substring-no-properties",
        |_ctx, args| builtin_substring_no_properties(args),
        1,
        Some(3),
    );
    ctx.defsubr("sqrt", |_ctx, args| builtin_sqrt(args), 1, Some(1));
    ctx.defsubr("sin", |_ctx, args| builtin_sin(args), 1, Some(1));
    ctx.defsubr("cos", |_ctx, args| builtin_cos(args), 1, Some(1));
    ctx.defsubr("tan", |_ctx, args| builtin_tan(args), 1, Some(1));
    ctx.defsubr("asin", |_ctx, args| builtin_asin(args), 1, Some(1));
    ctx.defsubr("acos", |_ctx, args| builtin_acos(args), 1, Some(1));
    ctx.defsubr("atan", |_ctx, args| builtin_atan(args), 1, Some(2));
    ctx.defsubr("exp", |_ctx, args| builtin_exp(args), 1, Some(1));
    ctx.defsubr("log", |_ctx, args| builtin_log(args), 1, Some(2));
    ctx.defsubr("expt", |_ctx, args| builtin_expt(args), 2, Some(2));
    ctx.defsubr("random", |_ctx, args| builtin_random(args), 0, Some(1));
    ctx.defsubr("isnan", |_ctx, args| builtin_isnan(args), 1, Some(1));
    ctx.defsubr(
        "make-string",
        |_ctx, args| builtin_make_string(args),
        2,
        Some(3),
    );
    ctx.defsubr("string", |_ctx, args| builtin_string(args), 0, None);
    ctx.defsubr(
        "string-width",
        |_ctx, args| builtin_string_width(args),
        1,
        Some(3),
    );
    ctx.defsubr("delete", |_ctx, args| builtin_delete(args), 2, Some(2));
    ctx.defsubr("delq", |_ctx, args| builtin_delq(args), 2, Some(2));
    ctx.defsubr("elt", |_ctx, args| builtin_elt(args), 2, Some(2));
    ctx.defsubr("memql", |_ctx, args| builtin_memql(args), 2, Some(2));
    ctx.defsubr("nconc", |_ctx, args| builtin_nconc(args), 0, None);
    ctx.defsubr("identity", |_ctx, args| builtin_identity(args), 1, Some(1));
    ctx.defsubr("ngettext", |_ctx, args| builtin_ngettext(args), 3, Some(3));
    ctx.defsubr(
        "secure-hash-algorithms",
        |_ctx, args| builtin_secure_hash_algorithms(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "prefix-numeric-value",
        |_ctx, args| builtin_prefix_numeric_value(args),
        0,
        None,
    );
    ctx.defsubr("propertize", |_ctx, args| builtin_propertize(args), 1, None);
    ctx.defsubr(
        "bare-symbol",
        |_ctx, args| super::builtins_extra::builtin_bare_symbol(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "capitalize",
        |_ctx, args| super::casefiddle::builtin_capitalize(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "charsetp",
        |_ctx, args| super::charset::builtin_charsetp(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "charset-plist",
        |_ctx, args| super::charset::builtin_charset_plist(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "define-charset-internal",
        |_ctx, args| super::charset::builtin_define_charset_internal(args),
        17,
        None,
    );
    ctx.defsubr(
        "define-charset-alias",
        |_ctx, args| super::charset::builtin_define_charset_alias(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-lisp-face-p",
        |_ctx, args| super::font::builtin_internal_lisp_face_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-make-lisp-face",
        super::font::builtin_internal_make_lisp_face,
        0,
        None,
    );
    ctx.defsubr(
        "internal-set-lisp-face-attribute",
        super::font::builtin_internal_set_lisp_face_attribute,
        3,
        Some(4),
    );
    ctx.defsubr(
        "string-to-syntax",
        |_ctx, args| super::syntax::builtin_string_to_syntax(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "syntax-class-to-char",
        |_ctx, args| super::syntax::builtin_syntax_class_to_char(args),
        0,
        None,
    );
    ctx.defsubr(
        "copy-syntax-table",
        |_ctx, args| super::syntax::builtin_copy_syntax_table(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "syntax-table-p",
        |_ctx, args| super::syntax::builtin_syntax_table_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "standard-syntax-table",
        |_ctx, args| super::syntax::builtin_standard_syntax_table(args),
        0,
        None,
    );
    ctx.defsubr(
        "current-time",
        |_ctx, args| super::timefns::builtin_current_time(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "current-cpu-time",
        |_ctx, args| builtin_current_cpu_time(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "get-internal-run-time",
        |_ctx, args| builtin_get_internal_run_time(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "float-time",
        |_ctx, args| super::timefns::builtin_float_time(args),
        0,
        Some(1),
    );
    ctx.defsubr("daemonp", |_ctx, args| builtin_daemonp(args), 0, Some(0));
    ctx.defsubr(
        "daemon-initialized",
        |_ctx, args| builtin_daemon_initialized(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "flush-standard-output",
        |_ctx, args| builtin_flush_standard_output(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "force-mode-line-update",
        |_ctx, args| builtin_force_mode_line_update(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "invocation-directory",
        |_ctx, args| builtin_invocation_directory(args),
        0,
        None,
    );
    ctx.defsubr(
        "invocation-name",
        |_ctx, args| builtin_invocation_name(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "file-name-directory",
        |ctx, args| super::fileio::builtin_file_name_directory(ctx, args),
        0,
        None,
    );
    ctx.defsubr(
        "file-name-nondirectory",
        |ctx, args| super::fileio::builtin_file_name_nondirectory(ctx, args),
        0,
        None,
    );
    ctx.defsubr(
        "file-name-as-directory",
        |ctx, args| super::fileio::builtin_file_name_as_directory(ctx, args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "directory-file-name",
        |ctx, args| super::fileio::builtin_directory_file_name(ctx, args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "file-name-concat",
        |_ctx, args| super::fileio::builtin_file_name_concat(args),
        1,
        None,
    );
    ctx.defsubr(
        "file-name-absolute-p",
        |_ctx, args| super::fileio::builtin_file_name_absolute_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "directory-name-p",
        |_ctx, args| super::fileio::builtin_directory_name_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "substitute-in-file-name",
        |ctx, args| super::fileio::builtin_substitute_in_file_name(ctx, args),
        0,
        None,
    );
    ctx.defsubr(
        "set-file-acl",
        |_ctx, args| super::fileio::builtin_set_file_acl(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "set-file-selinux-context",
        |_ctx, args| super::fileio::builtin_set_file_selinux_context(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "visited-file-modtime",
        |_ctx, args| super::fileio::builtin_visited_file_modtime(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "make-temp-name",
        |_ctx, args| super::fileio::builtin_make_temp_name(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "next-read-file-uses-dialog-p",
        |_ctx, args| super::fileio::builtin_next_read_file_uses_dialog_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "unhandled-file-name-directory",
        |_ctx, args| super::fileio::builtin_unhandled_file_name_directory(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "get-truename-buffer",
        |_ctx, args| super::fileio::builtin_get_truename_buffer(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "single-key-description",
        |_ctx, args| builtin_single_key_description(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "key-description",
        |_ctx, args| builtin_key_description(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "event-convert-list",
        |_ctx, args| builtin_event_convert_list(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "text-char-description",
        |_ctx, args| builtin_text_char_description(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "set-binary-mode",
        |_ctx, args| super::process::builtin_set_binary_mode(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "group-name",
        |_ctx, args| super::editfns::builtin_group_name(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "group-gid",
        |_ctx, args| super::editfns::builtin_group_gid(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "group-real-gid",
        |_ctx, args| super::editfns::builtin_group_real_gid(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "load-average",
        |_ctx, args| super::editfns::builtin_load_average(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "logcount",
        |_ctx, args| super::editfns::builtin_logcount(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "set-frame-size-and-position-pixelwise",
        |_ctx, args| builtin_set_frame_size_and_position_pixelwise(args),
        0,
        None,
    );
    ctx.defsubr(
        "mouse-position-in-root-frame",
        |_ctx, args| builtin_mouse_position_in_root_frame(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-load-color-file",
        |_ctx, args| super::font::builtin_x_load_color_file(args),
        0,
        None,
    );
    ctx.defsubr(
        "define-fringe-bitmap",
        |_ctx, args| builtin_define_fringe_bitmap(args),
        2,
        Some(5),
    );
    ctx.defsubr(
        "destroy-fringe-bitmap",
        |_ctx, args| builtin_destroy_fringe_bitmap(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "display--line-is-continued-p",
        |_ctx, args| builtin_display_line_is_continued_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "display--update-for-mouse-movement",
        |ctx, args| builtin_display_update_for_mouse_movement(ctx, args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "do-auto-save",
        super::fileio::builtin_do_auto_save,
        0,
        Some(2),
    );
    ctx.defsubr(
        "make-auto-save-file-name",
        super::fileio::builtin_make_auto_save_file_name,
        0,
        Some(0),
    );
    ctx.defsubr(
        "external-debugging-output",
        |_ctx, args| builtin_external_debugging_output(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "describe-buffer-bindings",
        |_ctx, args| builtin_describe_buffer_bindings(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "describe-vector",
        |_ctx, args| builtin_describe_vector(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "face-attributes-as-vector",
        |_ctx, args| super::xfaces::builtin_face_attributes_as_vector(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "font-face-attributes",
        |_ctx, args| builtin_font_face_attributes(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "font-get-glyphs",
        |_ctx, args| builtin_font_get_glyphs(args),
        3,
        Some(4),
    );
    ctx.defsubr(
        "font-get-system-font",
        |_ctx, args| builtin_font_get_system_font(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-get-system-normal-font",
        |_ctx, args| builtin_font_get_system_normal_font(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-has-char-p",
        |_ctx, args| builtin_font_has_char_p(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "font-match-p",
        |_ctx, args| builtin_font_match_p(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "font-shape-gstring",
        |_ctx, args| builtin_font_shape_gstring(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "font-variation-glyphs",
        |_ctx, args| builtin_font_variation_glyphs(args),
        0,
        None,
    );
    ctx.defsubr(
        "fontset-font",
        |_ctx, args| builtin_fontset_font(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "fontset-info",
        |_ctx, args| builtin_fontset_info(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "fontset-list",
        |_ctx, args| builtin_fontset_list(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "fontset-list-all",
        |_ctx, args| builtin_fontset_list_all(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "frame--set-was-invisible",
        |_ctx, args| builtin_frame_set_was_invisible(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-after-make-frame",
        |_ctx, args| builtin_frame_after_make_frame(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-ancestor-p",
        |_ctx, args| builtin_frame_ancestor_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-bottom-divider-width",
        |_ctx, args| builtin_frame_bottom_divider_width(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-child-frame-border-width",
        |_ctx, args| builtin_frame_child_frame_border_width(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-focus",
        |_ctx, args| builtin_frame_focus(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-font-cache",
        |_ctx, args| builtin_frame_font_cache(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-fringe-width",
        |_ctx, args| builtin_frame_fringe_width(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-internal-border-width",
        |_ctx, args| builtin_frame_internal_border_width(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-or-buffer-changed-p",
        |_ctx, args| builtin_frame_or_buffer_changed_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-parent",
        |_ctx, args| builtin_frame_parent(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-pointer-visible-p",
        |_ctx, args| builtin_frame_pointer_visible_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-right-divider-width",
        |_ctx, args| builtin_frame_right_divider_width(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-scale-factor",
        |_ctx, args| builtin_frame_scale_factor(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-scroll-bar-height",
        |_ctx, args| builtin_frame_scroll_bar_height(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-scroll-bar-width",
        |_ctx, args| builtin_frame_scroll_bar_width(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame-window-state-change",
        super::window_cmds::builtin_frame_window_state_change,
        0,
        None,
    );
    ctx.defsubr(
        "fringe-bitmaps-at-pos",
        |_ctx, args| builtin_fringe_bitmaps_at_pos(args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "gap-position",
        |_ctx, args| builtin_gap_position(args),
        0,
        Some(0),
    );
    ctx.defsubr("gap-size", |_ctx, args| builtin_gap_size(args), 0, Some(0));
    ctx.defsubr(
        "garbage-collect-heapsize",
        |_ctx, args| builtin_garbage_collect_heapsize(args),
        0,
        None,
    );
    ctx.defsubr(
        "garbage-collect-maybe",
        |_ctx, args| builtin_garbage_collect_maybe(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "get-unicode-property-internal",
        |_ctx, args| builtin_get_unicode_property_internal(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "gnutls-available-p",
        |_ctx, args| builtin_gnutls_available_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "gnutls-asynchronous-parameters",
        |_ctx, args| builtin_gnutls_asynchronous_parameters(args),
        0,
        None,
    );
    ctx.defsubr("gnutls-bye", |_ctx, args| builtin_gnutls_bye(args), 0, None);
    ctx.defsubr(
        "gnutls-ciphers",
        |_ctx, args| builtin_gnutls_ciphers(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "gnutls-deinit",
        |_ctx, args| builtin_gnutls_deinit(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "gnutls-digests",
        |_ctx, args| builtin_gnutls_digests(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "gnutls-error-fatalp",
        |_ctx, args| builtin_gnutls_error_fatalp(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "gnutls-error-string",
        |_ctx, args| builtin_gnutls_error_string(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "gnutls-errorp",
        |_ctx, args| builtin_gnutls_errorp(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "gnutls-format-certificate",
        |_ctx, args| builtin_gnutls_format_certificate(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-get-initstage",
        |_ctx, args| builtin_gnutls_get_initstage(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "gnutls-hash-digest",
        |_ctx, args| builtin_gnutls_hash_digest(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "gnutls-hash-mac",
        |_ctx, args| builtin_gnutls_hash_mac(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "gnutls-macs",
        |_ctx, args| builtin_gnutls_macs(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "gnutls-peer-status",
        |_ctx, args| builtin_gnutls_peer_status(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "gnutls-peer-status-warning-describe",
        |_ctx, args| builtin_gnutls_peer_status_warning_describe(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "gnutls-symmetric-decrypt",
        |_ctx, args| builtin_gnutls_symmetric_decrypt(args),
        4,
        Some(5),
    );
    ctx.defsubr(
        "gnutls-symmetric-encrypt",
        |_ctx, args| builtin_gnutls_symmetric_encrypt(args),
        4,
        Some(5),
    );
    ctx.defsubr(
        "gpm-mouse-start",
        |_ctx, args| builtin_gpm_mouse_start(args),
        0,
        None,
    );
    ctx.defsubr(
        "gpm-mouse-stop",
        |_ctx, args| builtin_gpm_mouse_stop(args),
        0,
        None,
    );
    ctx.defsubr(
        "handle-save-session",
        |_ctx, args| builtin_handle_save_session(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "handle-switch-frame",
        |_ctx, args| builtin_handle_switch_frame(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "help--describe-vector",
        |_ctx, args| builtin_help_describe_vector(args),
        7,
        Some(7),
    );
    ctx.defsubr(
        "init-image-library",
        |_ctx, args| builtin_init_image_library(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "internal--obarray-buckets",
        |_ctx, args| builtin_internal_obarray_buckets(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "internal--set-buffer-modified-tick",
        |ctx, args| builtin_internal_set_buffer_modified_tick(ctx, args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "internal--track-mouse",
        |ctx, args| builtin_internal_track_mouse(ctx, args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "internal-char-font",
        |_ctx, args| builtin_internal_char_font(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "internal-complete-buffer",
        |_ctx, args| builtin_internal_complete_buffer(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "internal-describe-syntax-value",
        |_ctx, args| builtin_internal_describe_syntax_value(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "internal-event-symbol-parse-modifiers",
        |_ctx, args| builtin_internal_event_symbol_parse_modifiers(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "internal-handle-focus-in",
        |ctx, args| builtin_internal_handle_focus_in(ctx, args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "internal-set-lisp-face-attribute-from-resource",
        |_ctx, args| builtin_internal_set_lisp_face_attribute_from_resource(args),
        3,
        Some(4),
    );
    ctx.defsubr(
        "internal-stack-stats",
        |_ctx, args| builtin_internal_stack_stats(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "internal-subr-documentation",
        |_ctx, args| builtin_internal_subr_documentation(args),
        1,
        Some(1),
    );
    // byte-code: mirrors GNU Emacs Fbyte_code (src/bytecode.c).
    // Receives pre-evaluated args (bytestr, vector, maxdepth), decodes
    // the GNU bytecodes, and executes them via the bytecode VM.
    ctx.defsubr(
        "byte-code",
        |ctx, args| {
            crate::emacs_core::builtins::expect_args("byte-code", &args, 3)?;
            let bytestr = args[0];
            let constants_vec = args[1];
            let maxdepth = args[2];

            use crate::emacs_core::bytecode::ByteCodeFunction;
            use crate::emacs_core::bytecode::decode::{
                decode_gnu_bytecode_with_offset_map, string_value_to_bytes,
            };
            use crate::emacs_core::value::LambdaParams;

            let raw_bytes = if let Some(s) = bytestr.as_str() {
                string_value_to_bytes(s)
            } else {
                return Err(super::error::signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), bytestr],
                ));
            };

            let mut constants: Vec<Value> = match constants_vec.kind() {
                ValueKind::Veclike(VecLikeType::Vector) => {
                    constants_vec.as_vector_data().unwrap().clone()
                }
                _ => {
                    return Err(super::error::signal(
                        "wrong-type-argument",
                        vec![Value::symbol("vectorp"), constants_vec],
                    ));
                }
            };

            for i in 0..constants.len() {
                constants[i] = super::builtins::try_convert_nested_compiled_literal(constants[i]);
            }

            let (ops, gnu_byte_offset_map) =
                decode_gnu_bytecode_with_offset_map(&raw_bytes, &mut constants).map_err(|e| {
                    super::error::signal(
                        "error",
                        vec![Value::string(format!("bytecode decode error: {}", e))],
                    )
                })?;

            let max_stack = match maxdepth.kind() {
                ValueKind::Fixnum(n) => n as u16,
                _ => 16,
            };

            let bc = ByteCodeFunction {
                ops,
                constants,
                max_stack,
                params: LambdaParams::simple(vec![]),
                lexical: false,
                env: None,
                gnu_byte_offset_map: Some(gnu_byte_offset_map),
                gnu_bytecode_bytes: None,
                docstring: None,
                doc_form: None,
                interactive: None,
            };

            ctx.refresh_features_from_variable();
            let mut vm = super::bytecode::Vm::from_context(ctx);
            let result = vm.execute(&bc, vec![]);
            ctx.sync_features_variable();
            result
        },
        0,
        None,
    );
    ctx.defsubr(
        "decode-coding-region",
        |_ctx, args| builtin_decode_coding_region(args),
        3,
        Some(4),
    );
    ctx.defsubr(
        "dump-emacs-portable",
        builtin_dump_emacs_portable,
        1,
        Some(2),
    );
    ctx.defsubr(
        "dump-emacs-portable--sort-predicate",
        |_ctx, args| builtin_dump_emacs_portable_sort_predicate(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "dump-emacs-portable--sort-predicate-copied",
        |_ctx, args| builtin_dump_emacs_portable_sort_predicate_copied(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "emacs-repository-get-version",
        |_ctx, args| builtin_emacs_repository_get_version(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "emacs-repository-get-branch",
        |_ctx, args| builtin_emacs_repository_get_branch(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "emacs-repository-get-dirty",
        |_ctx, args| builtin_emacs_repository_get_dirty(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "encode-coding-region",
        |_ctx, args| builtin_encode_coding_region(args),
        3,
        Some(4),
    );
    ctx.defsubr(
        "find-operation-coding-system",
        |_ctx, args| builtin_find_operation_coding_system(args),
        1,
        None,
    );
    ctx.defsubr(
        "iso-charset",
        |_ctx, args| builtin_iso_charset(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "keymap--get-keyelt",
        |_ctx, args| builtin_keymap_get_keyelt(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "keymap-prompt",
        |_ctx, args| builtin_keymap_prompt(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "lower-frame",
        |_ctx, args| builtin_lower_frame(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "lread--substitute-object-in-subtree",
        |_ctx, args| builtin_lread_substitute_object_in_subtree(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "malloc-info",
        |_ctx, args| builtin_malloc_info(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "malloc-trim",
        |_ctx, args| builtin_malloc_trim(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "make-byte-code",
        |_ctx, args| builtin_make_byte_code(args),
        4,
        None,
    );
    ctx.defsubr(
        "make-char",
        |_ctx, args| builtin_make_char(args),
        1,
        Some(5),
    );
    ctx.defsubr(
        "make-closure",
        |_ctx, args| builtin_make_closure(args),
        1,
        None,
    );
    ctx.defsubr(
        "make-finalizer",
        |_ctx, args| builtin_make_finalizer(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "marker-last-position",
        |_ctx, args| builtin_marker_last_position(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "make-interpreted-closure",
        |_ctx, args| builtin_make_interpreted_closure(args),
        3,
        Some(5),
    );
    ctx.defsubr(
        "make-record",
        |_ctx, args| builtin_make_record(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "make-temp-file-internal",
        builtin_make_temp_file_internal,
        4,
        Some(4),
    );
    ctx.defsubr(
        "map-charset-chars",
        |_ctx, args| builtin_map_charset_chars(args),
        2,
        Some(5),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "mapbacktrace",
            super::misc::builtin_mapbacktrace,
            1,
            Some(2),
        ),
    );
    ctx.defsubr(
        "memory-info",
        |_ctx, args| builtin_memory_info(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "make-frame-invisible",
        |_ctx, args| builtin_make_frame_invisible(args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "menu-bar-menu-at-x-y",
        |_ctx, args| builtin_menu_bar_menu_at_x_y(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "menu-or-popup-active-p",
        |_ctx, args| builtin_menu_or_popup_active_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "module-load",
        |_ctx, args| builtin_module_load(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "newline-cache-check",
        |_ctx, args| builtin_newline_cache_check(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "native-comp-available-p",
        |_ctx, args| builtin_native_comp_available_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "native-comp-unit-file",
        |_ctx, args| builtin_native_comp_unit_file(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "native-comp-unit-set-file",
        |_ctx, args| builtin_native_comp_unit_set_file(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "native-elisp-load",
        |_ctx, args| builtin_native_elisp_load(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "obarray-clear",
        |_ctx, args| builtin_obarray_clear(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "obarray-make",
        |_ctx, args| builtin_obarray_make(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "object-intervals",
        |_ctx, args| builtin_object_intervals(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "open-dribble-file",
        |_ctx, args| builtin_open_dribble_file(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "open-font",
        |_ctx, args| builtin_open_font(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "optimize-char-table",
        |_ctx, args| builtin_optimize_char_table(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "overlay-lists",
        |_ctx, args| builtin_overlay_lists(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "overlay-recenter",
        |_ctx, args| builtin_overlay_recenter(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "pdumper-stats",
        |_ctx, args| builtin_pdumper_stats(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "play-sound-internal",
        |_ctx, args| builtin_play_sound_internal(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "position-symbol",
        |_ctx, args| builtin_position_symbol(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "profiler-cpu-log",
        |_ctx, args| builtin_profiler_cpu_log(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "profiler-cpu-running-p",
        |_ctx, args| builtin_profiler_cpu_running_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "profiler-cpu-start",
        |_ctx, args| builtin_profiler_cpu_start(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "profiler-cpu-stop",
        |_ctx, args| builtin_profiler_cpu_stop(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "profiler-memory-log",
        |_ctx, args| builtin_profiler_memory_log(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "profiler-memory-running-p",
        |_ctx, args| builtin_profiler_memory_running_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "profiler-memory-start",
        |_ctx, args| builtin_profiler_memory_start(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "profiler-memory-stop",
        |_ctx, args| builtin_profiler_memory_stop(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "put-unicode-property-internal",
        |_ctx, args| builtin_put_unicode_property_internal(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "query-font",
        |_ctx, args| builtin_query_font(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "query-fontset",
        |_ctx, args| builtin_query_fontset(args),
        1,
        Some(2),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "raise-frame",
            |_ctx, args| builtin_raise_frame(args),
            0,
            Some(1),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    ctx.defsubr(
        "read-positioning-symbols",
        |_ctx, args| builtin_read_positioning_symbols(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "re--describe-compiled",
        |_ctx, args| builtin_re_describe_compiled(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "recent-auto-save-p",
        buffers::builtin_recent_auto_save_p,
        0,
        Some(0),
    );
    ctx.defsubr("redisplay", builtin_redisplay, 0, Some(1));
    ctx.defsubr("record", |_ctx, args| builtin_record(args), 1, None);
    ctx.defsubr("recordp", |_ctx, args| builtin_recordp(args), 1, Some(1));
    ctx.defsubr(
        "reconsider-frame-fonts",
        |_ctx, args| builtin_reconsider_frame_fonts(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "redirect-debugging-output",
        |_ctx, args| builtin_redirect_debugging_output(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "redirect-frame-focus",
        |_ctx, args| builtin_redirect_frame_focus(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "remove-pos-from-symbol",
        |_ctx, args| builtin_remove_pos_from_symbol(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "resize-mini-window-internal",
        |_ctx, args| super::window_cmds::builtin_resize_mini_window_internal(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "restore-buffer-modified-p",
        buffers::builtin_restore_buffer_modified_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "set--this-command-keys",
        builtin_set_this_command_keys,
        1,
        Some(1),
    );
    ctx.defsubr(
        "set-buffer-auto-saved",
        buffers::builtin_set_buffer_auto_saved,
        0,
        Some(0),
    );
    ctx.defsubr(
        "set-buffer-major-mode",
        |_ctx, args| builtin_set_buffer_major_mode(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "set-buffer-redisplay",
        |_ctx, args| builtin_set_buffer_redisplay(args),
        4,
        Some(4),
    );
    ctx.defsubr(
        "set-charset-plist",
        |_ctx, args| builtin_set_charset_plist(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "set-frame-window-state-change",
        super::window_cmds::builtin_set_frame_window_state_change,
        0,
        Some(2),
    );
    ctx.defsubr(
        "set-fringe-bitmap-face",
        |_ctx, args| builtin_set_fringe_bitmap_face(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "set-minibuffer-window",
        |_ctx, args| builtin_set_minibuffer_window(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "set-mouse-pixel-position",
        |ctx, args| builtin_set_mouse_pixel_position(ctx, args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "set-mouse-position",
        |ctx, args| builtin_set_mouse_position(ctx, args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "set-window-new-normal",
        |_ctx, args| super::window_cmds::builtin_set_window_new_normal(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "set-window-new-pixel",
        |_ctx, args| super::window_cmds::builtin_set_window_new_pixel(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "set-window-new-total",
        |_ctx, args| super::window_cmds::builtin_set_window_new_total(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "sort-charsets",
        |_ctx, args| builtin_sort_charsets(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "split-char",
        |_ctx, args| builtin_split_char(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "string-distance",
        |_ctx, args| builtin_string_distance(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "subr-native-comp-unit",
        |_ctx, args| builtin_subr_native_comp_unit(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "subr-native-lambda-list",
        |_ctx, args| builtin_subr_native_lambda_list(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "subr-type",
        |_ctx, args| builtin_subr_type(args),
        1,
        Some(1),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "suspend-emacs",
            |_ctx, args| builtin_suspend_emacs(args),
            0,
            Some(1),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "thread--blocker",
            super::threads::builtin_thread_blocker,
            1,
            Some(1),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    ctx.defsubr(
        "tool-bar-get-system-style",
        |_ctx, args| builtin_tool_bar_get_system_style(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "tool-bar-pixel-width",
        |_ctx, args| builtin_tool_bar_pixel_width(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "translate-region-internal",
        |_ctx, args| builtin_translate_region_internal(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "transpose-regions",
        |_ctx, args| builtin_transpose_regions(args),
        4,
        Some(5),
    );
    ctx.defsubr(
        "tty--output-buffer-size",
        |_ctx, args| builtin_tty_output_buffer_size(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "tty--set-output-buffer-size",
        |_ctx, args| builtin_tty_set_output_buffer_size(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "tty-display-pixel-height",
        |_ctx, args| builtin_tty_display_pixel_height(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-display-pixel-width",
        |_ctx, args| builtin_tty_display_pixel_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-frame-at",
        |_ctx, args| builtin_tty_frame_at(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "tty-frame-edges",
        |_ctx, args| builtin_tty_frame_edges(args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "tty-frame-geometry",
        |_ctx, args| builtin_tty_frame_geometry(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "tty-frame-list-z-order",
        |_ctx, args| builtin_tty_frame_list_z_order(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-frame-restack",
        |_ctx, args| builtin_tty_frame_restack(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-suppress-bold-inverse-default-colors",
        |_ctx, args| builtin_tty_suppress_bold_inverse_default_colors(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "unencodable-char-position",
        |_ctx, args| builtin_unencodable_char_position(args),
        3,
        Some(5),
    );
    ctx.defsubr(
        "unicode-property-table-internal",
        |_ctx, args| builtin_unicode_property_table_internal(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "unify-charset",
        |_ctx, args| builtin_unify_charset(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "unix-sync",
        |_ctx, args| builtin_unix_sync(args),
        0,
        Some(0),
    );
    ctx.defsubr("value<", |_ctx, args| builtin_value_lt(args), 2, Some(2));
    ctx.defsubr(
        "x-begin-drag",
        |_ctx, args| builtin_x_begin_drag(args),
        1,
        Some(6),
    );
    ctx.defsubr(
        "x-double-buffered-p",
        |_ctx, args| builtin_x_double_buffered_p(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-menu-bar-open-internal",
        |_ctx, args| builtin_x_menu_bar_open_internal(args),
        0,
        Some(1),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "xw-color-defined-p",
            |ctx, args| super::font::builtin_xw_color_defined_p_ctx(ctx, args),
            1,
            Some(2),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    ctx.defsubr(
        "color-defined-p",
        |ctx, args| super::font::builtin_xw_color_defined_p_ctx(ctx, args),
        0,
        None,
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "xw-color-values",
            |ctx, args| super::font::builtin_xw_color_values_ctx(ctx, args),
            1,
            Some(2),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    ctx.defsubr(
        "color-values",
        |ctx, args| super::font::builtin_xw_color_values_ctx(ctx, args),
        0,
        None,
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "xw-display-color-p",
            |ctx, args| builtin_xw_display_color_p_ctx(ctx, args),
            0,
            Some(1),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    ctx.defsubr(
        "inotify-add-watch",
        |_ctx, args| builtin_inotify_add_watch(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "inotify-allocated-p",
        |_ctx, args| builtin_inotify_allocated_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "inotify-rm-watch",
        |_ctx, args| builtin_inotify_rm_watch(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "inotify-valid-p",
        |_ctx, args| builtin_inotify_valid_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "inotify-watch-list",
        |_ctx, args| builtin_inotify_watch_list(args),
        0,
        Some(0),
    );
    ctx.defsubr("lock-buffer", super::filelock::builtin_lock_buffer, 0, None);
    ctx.defsubr("lock-file", super::filelock::builtin_lock_file, 1, Some(1));
    ctx.defsubr(
        "lossage-size",
        |_ctx, args| builtin_lossage_size(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "unlock-buffer",
        super::filelock::builtin_unlock_buffer,
        0,
        Some(0),
    );
    ctx.defsubr(
        "unlock-file",
        super::filelock::builtin_unlock_file,
        1,
        Some(1),
    );
    ctx.defsubr(
        "window-bottom-divider-width",
        |_ctx, args| super::window_cmds::builtin_window_bottom_divider_width(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-lines-pixel-dimensions",
        |_ctx, args| super::window_cmds::builtin_window_lines_pixel_dimensions(args),
        0,
        Some(6),
    );
    ctx.defsubr(
        "window-new-normal",
        |_ctx, args| super::window_cmds::builtin_window_new_normal(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-new-pixel",
        |_ctx, args| super::window_cmds::builtin_window_new_pixel(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "window-new-total",
        |_ctx, args| super::window_cmds::builtin_window_new_total(args),
        0,
        Some(1),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "window-old-body-pixel-height",
            |_ctx, args| super::window_cmds::builtin_window_old_body_pixel_height(args),
            0,
            None,
            BuiltinNoEvalPlaceholder::FixnumZero,
        ),
    );
    ctx.defsubr(
        "window-old-body-pixel-width",
        |_ctx, args| super::window_cmds::builtin_window_old_body_pixel_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-old-pixel-height",
        |_ctx, args| super::window_cmds::builtin_window_old_pixel_height(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-old-pixel-width",
        |_ctx, args| super::window_cmds::builtin_window_old_pixel_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-right-divider-width",
        |_ctx, args| super::window_cmds::builtin_window_right_divider_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-scroll-bar-height",
        super::window_cmds::builtin_window_scroll_bar_height,
        0,
        None,
    );
    ctx.defsubr(
        "window-scroll-bar-width",
        super::window_cmds::builtin_window_scroll_bar_width,
        0,
        None,
    );
    ctx.defsubr(
        "treesit-available-p",
        |_ctx, args| builtin_treesit_available_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "treesit-compiled-query-p",
        |_ctx, args| builtin_treesit_compiled_query_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-induce-sparse-tree",
        |_ctx, args| builtin_treesit_induce_sparse_tree(args),
        2,
        Some(4),
    );
    ctx.defsubr(
        "treesit-language-abi-version",
        |_ctx, args| builtin_treesit_language_abi_version(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "treesit-language-available-p",
        |_ctx, args| builtin_treesit_language_available_p(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "treesit-library-abi-version",
        |_ctx, args| builtin_treesit_library_abi_version(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "treesit-node-check",
        |_ctx, args| builtin_treesit_node_check(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "treesit-node-child",
        |_ctx, args| builtin_treesit_node_child(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "treesit-node-child-by-field-name",
        |_ctx, args| builtin_treesit_node_child_by_field_name(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "treesit-node-child-count",
        |_ctx, args| builtin_treesit_node_child_count(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "treesit-node-descendant-for-range",
        |_ctx, args| builtin_treesit_node_descendant_for_range(args),
        3,
        Some(4),
    );
    ctx.defsubr(
        "treesit-node-end",
        |_ctx, args| builtin_treesit_node_end(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-node-eq",
        |_ctx, args| builtin_treesit_node_eq(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "treesit-node-field-name-for-child",
        |_ctx, args| builtin_treesit_node_field_name_for_child(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "treesit-node-first-child-for-pos",
        |_ctx, args| builtin_treesit_node_first_child_for_pos(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "treesit-node-match-p",
        |_ctx, args| builtin_treesit_node_match_p(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "treesit-node-next-sibling",
        |_ctx, args| builtin_treesit_node_next_sibling(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "treesit-node-p",
        |_ctx, args| builtin_treesit_node_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-node-parent",
        |_ctx, args| builtin_treesit_node_parent(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-node-parser",
        |_ctx, args| builtin_treesit_node_parser(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-node-prev-sibling",
        |_ctx, args| builtin_treesit_node_prev_sibling(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "treesit-node-start",
        |_ctx, args| builtin_treesit_node_start(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-node-string",
        |_ctx, args| builtin_treesit_node_string(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-node-type",
        |_ctx, args| builtin_treesit_node_type(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-parser-add-notifier",
        |_ctx, args| builtin_treesit_parser_add_notifier(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "treesit-parser-buffer",
        |_ctx, args| builtin_treesit_parser_buffer(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-parser-create",
        |_ctx, args| builtin_treesit_parser_create(args),
        1,
        Some(4),
    );
    ctx.defsubr(
        "treesit-parser-delete",
        |_ctx, args| builtin_treesit_parser_delete(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-parser-included-ranges",
        |_ctx, args| builtin_treesit_parser_included_ranges(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-parser-language",
        |_ctx, args| builtin_treesit_parser_language(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-parser-list",
        |_ctx, args| builtin_treesit_parser_list(args),
        0,
        Some(3),
    );
    ctx.defsubr(
        "treesit-parser-notifiers",
        |_ctx, args| builtin_treesit_parser_notifiers(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-parser-p",
        |_ctx, args| builtin_treesit_parser_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-parser-remove-notifier",
        |_ctx, args| builtin_treesit_parser_remove_notifier(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "treesit-parser-root-node",
        |_ctx, args| builtin_treesit_parser_root_node(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-parser-set-included-ranges",
        |_ctx, args| builtin_treesit_parser_set_included_ranges(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "treesit-parser-tag",
        |_ctx, args| builtin_treesit_parser_tag(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-pattern-expand",
        |_ctx, args| builtin_treesit_pattern_expand(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-query-capture",
        |_ctx, args| builtin_treesit_query_capture(args),
        2,
        Some(5),
    );
    ctx.defsubr(
        "treesit-query-compile",
        |_ctx, args| builtin_treesit_query_compile(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "treesit-query-expand",
        |_ctx, args| builtin_treesit_query_expand(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-query-language",
        |_ctx, args| builtin_treesit_query_language(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-query-p",
        |_ctx, args| builtin_treesit_query_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-search-forward",
        |_ctx, args| builtin_treesit_search_forward(args),
        2,
        Some(4),
    );
    ctx.defsubr(
        "treesit-search-subtree",
        |_ctx, args| builtin_treesit_search_subtree(args),
        2,
        Some(5),
    );
    ctx.defsubr(
        "treesit-subtree-stat",
        |_ctx, args| builtin_treesit_subtree_stat(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "treesit-grammar-location",
        |_ctx, args| builtin_treesit_grammar_location(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-tracking-line-column-p",
        |_ctx, args| builtin_treesit_tracking_line_column_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-tracking-line-column-p",
        |_ctx, args| builtin_treesit_parser_tracking_line_column_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-query-eagerly-compiled-p",
        |_ctx, args| builtin_treesit_query_eagerly_compiled_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-query-source",
        |_ctx, args| builtin_treesit_query_source(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-embed-level",
        |_ctx, args| builtin_treesit_parser_embed_level(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-set-embed-level",
        |_ctx, args| builtin_treesit_parser_set_embed_level(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parse-string",
        |_ctx, args| builtin_treesit_parse_string(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-changed-regions",
        |_ctx, args| builtin_treesit_parser_changed_regions(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit--linecol-at",
        |_ctx, args| builtin_treesit_linecol_at(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit--linecol-cache-set",
        |_ctx, args| builtin_treesit_linecol_cache_set(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit--linecol-cache",
        |_ctx, args| builtin_treesit_linecol_cache(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-available-p",
        |_ctx, args| builtin_sqlite_available_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "sqlite-close",
        |_ctx, args| builtin_sqlite_close(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sqlite-columns",
        |_ctx, args| builtin_sqlite_columns(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sqlite-commit",
        |_ctx, args| builtin_sqlite_commit(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sqlite-execute",
        |_ctx, args| builtin_sqlite_execute(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "sqlite-execute-batch",
        builtin_sqlite_execute_batch,
        2,
        Some(2),
    );
    ctx.defsubr(
        "sqlite-finalize",
        |_ctx, args| builtin_sqlite_finalize(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sqlite-load-extension",
        |_ctx, args| builtin_sqlite_load_extension(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-more-p",
        |_ctx, args| builtin_sqlite_more_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sqlite-next",
        |_ctx, args| builtin_sqlite_next(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sqlite-open",
        |_ctx, args| builtin_sqlite_open(args),
        0,
        Some(3),
    );
    ctx.defsubr(
        "sqlite-pragma",
        |_ctx, args| builtin_sqlite_pragma(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "sqlite-rollback",
        |_ctx, args| builtin_sqlite_rollback(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sqlite-select",
        |_ctx, args| builtin_sqlite_select(args),
        2,
        Some(4),
    );
    ctx.defsubr(
        "sqlite-transaction",
        |_ctx, args| builtin_sqlite_transaction(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sqlite-version",
        |_ctx, args| builtin_sqlite_version(args),
        0,
        Some(0),
    );
    ctx.defsubr("sqlitep", |_ctx, args| builtin_sqlitep(args), 1, Some(1));
    ctx.defsubr(
        "fillarray",
        |_ctx, args| builtin_fillarray(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "define-hash-table-test",
        |_ctx, args| builtin_define_hash_table_test(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "hash-table-test",
        |_ctx, args| super::hashtab::builtin_hash_table_test(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "hash-table-size",
        |_ctx, args| super::hashtab::builtin_hash_table_size(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "hash-table-rehash-size",
        |_ctx, args| super::hashtab::builtin_hash_table_rehash_size(args),
        0,
        None,
    );
    ctx.defsubr(
        "hash-table-rehash-threshold",
        |_ctx, args| super::hashtab::builtin_hash_table_rehash_threshold(args),
        0,
        None,
    );
    ctx.defsubr(
        "hash-table-weakness",
        |_ctx, args| super::hashtab::builtin_hash_table_weakness(args),
        0,
        None,
    );
    ctx.defsubr(
        "copy-hash-table",
        |_ctx, args| super::hashtab::builtin_copy_hash_table(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sxhash-eq",
        |_ctx, args| super::hashtab::builtin_sxhash_eq(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sxhash-eql",
        |_ctx, args| super::hashtab::builtin_sxhash_eql(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sxhash-equal",
        |_ctx, args| super::hashtab::builtin_sxhash_equal(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "sxhash-equal-including-properties",
        |_ctx, args| super::hashtab::builtin_sxhash_equal_including_properties(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal--hash-table-buckets",
        |_ctx, args| super::hashtab::builtin_internal_hash_table_buckets(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal--hash-table-histogram",
        |_ctx, args| super::hashtab::builtin_internal_hash_table_histogram(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal--hash-table-index-size",
        |_ctx, args| super::hashtab::builtin_internal_hash_table_index_size(args),
        0,
        None,
    );
    ctx.defsubr(
        "debug-timer-check",
        |_ctx, args| builtin_debug_timer_check(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "dbus-close-inhibitor-lock",
        |_ctx, args| builtin_dbus_close_inhibitor_lock(args),
        0,
        None,
    );
    ctx.defsubr(
        "dbus-make-inhibitor-lock",
        |_ctx, args| builtin_dbus_make_inhibitor_lock(args),
        0,
        None,
    );
    ctx.defsubr(
        "dbus-registered-inhibitor-locks",
        |_ctx, args| builtin_dbus_registered_inhibitor_locks(args),
        0,
        None,
    );
    ctx.defsubr(
        "lcms2-available-p",
        |_ctx, args| builtin_lcms2_available_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "lcms-cie-de2000",
        |_ctx, args| builtin_lcms_cie_de2000(args),
        2,
        Some(5),
    );
    ctx.defsubr(
        "lcms-xyz->jch",
        |_ctx, args| builtin_lcms_xyz_to_jch(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "lcms-jch->xyz",
        |_ctx, args| builtin_lcms_jch_to_xyz(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "lcms-jch->jab",
        |_ctx, args| builtin_lcms_jch_to_jab(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "lcms-jab->jch",
        |_ctx, args| builtin_lcms_jab_to_jch(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "lcms-cam02-ucs",
        |_ctx, args| builtin_lcms_cam02_ucs(args),
        2,
        Some(4),
    );
    ctx.defsubr(
        "lcms-temp->white-point",
        |_ctx, args| builtin_lcms_temp_to_white_point(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "neomacs-frame-geometry",
        |_ctx, args| builtin_neomacs_frame_geometry(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-frame-edges",
        |_ctx, args| builtin_neomacs_frame_edges(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-mouse-absolute-pixel-position",
        |_ctx, args| builtin_neomacs_mouse_absolute_pixel_position(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-set-mouse-absolute-pixel-position",
        |_ctx, args| builtin_neomacs_set_mouse_absolute_pixel_position(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-display-monitor-attributes-list",
        builtin_neomacs_display_monitor_attributes_list,
        0,
        None,
    );
    ctx.defsubr(
        "x-scroll-bar-foreground",
        |_ctx, args| builtin_x_scroll_bar_foreground(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-scroll-bar-background",
        |_ctx, args| builtin_x_scroll_bar_background(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-clipboard-set",
        |_ctx, args| builtin_neomacs_clipboard_set(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-clipboard-get",
        |_ctx, args| builtin_neomacs_clipboard_get(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-primary-selection-set",
        |_ctx, args| builtin_neomacs_primary_selection_set(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-primary-selection-get",
        |_ctx, args| builtin_neomacs_primary_selection_get(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-core-backend",
        |_ctx, args| builtin_neomacs_core_backend(args),
        0,
        None,
    );
    ctx.defsubr(
        "buffer-local-toplevel-value",
        |_ctx, args| builtin_buffer_local_toplevel_value(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-buffer-local-toplevel-value",
        |_ctx, args| builtin_set_buffer_local_toplevel_value(args),
        0,
        None,
    );
    ctx.defsubr(
        "debugger-trap",
        |_ctx, args| builtin_debugger_trap(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "internal-delete-indirect-variable",
        |_ctx, args| builtin_internal_delete_indirect_variable(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-decode-string-utf-8",
        |_ctx, args| builtin_internal_decode_string_utf_8(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-encode-string-utf-8",
        |_ctx, args| builtin_internal_encode_string_utf_8(args),
        0,
        None,
    );
    ctx.defsubr(
        "overlay-tree",
        |_ctx, args| builtin_overlay_tree(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "thread-buffer-disposition",
        super::threads::builtin_thread_buffer_disposition,
        1,
        Some(1),
    );
    ctx.defsubr(
        "thread-set-buffer-disposition",
        super::threads::builtin_thread_set_buffer_disposition,
        2,
        Some(2),
    );
    ctx.defsubr(
        "window-discard-buffer-from-window",
        super::window_cmds::builtin_window_discard_buffer_from_window,
        2,
        Some(3),
    );
    ctx.defsubr(
        "window-cursor-info",
        |_ctx, args| super::window_cmds::builtin_window_cursor_info(args),
        0,
        None,
    );
    ctx.defsubr(
        "combine-windows",
        |_ctx, args| super::window_cmds::builtin_combine_windows(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "uncombine-window",
        |_ctx, args| super::window_cmds::builtin_uncombine_window(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "frame-windows-min-size",
        |_ctx, args| builtin_frame_windows_min_size(args),
        0,
        None,
    );
    ctx.defsubr(
        "remember-mouse-glyph",
        |_ctx, args| builtin_remember_mouse_glyph(args),
        0,
        None,
    );
    ctx.defsubr(
        "lookup-image",
        |_ctx, args| builtin_lookup_image(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "imagemagick-types",
        |_ctx, args| builtin_imagemagick_types(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "font-drive-otf",
        |_ctx, args| builtin_font_drive_otf(args),
        6,
        Some(6),
    );
    ctx.defsubr(
        "font-otf-alternates",
        |_ctx, args| builtin_font_otf_alternates(args),
        0,
        None,
    );
    ctx.defsubr("obarrayp", |_ctx, args| builtin_obarrayp(args), 1, Some(1));
    ctx.defsubr("ntake", |_ctx, args| builtin_ntake(args), 2, Some(2));
    ctx.defsubr(
        "default-file-modes",
        |_ctx, args| super::fileio::builtin_default_file_modes(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "set-default-file-modes",
        |_ctx, args| super::fileio::builtin_set_default_file_modes(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "cancel-kbd-macro-events",
        |ctx, args| builtin_cancel_kbd_macro_events(ctx, args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "window-configuration-p",
        |_ctx, args| super::window_cmds::builtin_window_configuration_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "window-configuration-frame",
        |_ctx, args| super::window_cmds::builtin_window_configuration_frame(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "window-configuration-equal-p",
        |_ctx, args| super::window_cmds::builtin_window_configuration_equal_p(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "set-input-meta-mode",
        |_ctx, args| super::reader::builtin_set_input_meta_mode(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "set-output-flow-control",
        |_ctx, args| super::reader::builtin_set_output_flow_control(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "set-quit-char",
        super::reader::builtin_set_quit_char,
        1,
        Some(1),
    );
    ctx.defsubr(
        "top-level",
        |_ctx, args| super::minibuffer::builtin_top_level(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "documentation-stringp",
        |_ctx, args| builtin_documentation_stringp(args),
        1,
        Some(1),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "internal--define-uninitialized-variable",
            symbols::builtin_internal_define_uninitialized_variable,
            1,
            Some(2),
        ),
    );
    ctx.defsubr(
        "compose-region-internal",
        super::composite::builtin_compose_region_internal,
        2,
        Some(4),
    );
    ctx.defsubr(
        "window-text-pixel-size",
        super::xdisp::builtin_window_text_pixel_size_ctx,
        0,
        Some(7),
    );
    ctx.defsubr(
        "pos-visible-in-window-p",
        super::xdisp::builtin_pos_visible_in_window_p_ctx,
        0,
        None,
    );
    ctx.defsubr(
        "frame--face-hash-table",
        super::xfaces::builtin_frame_face_hash_table,
        0,
        Some(1),
    );
    ctx.defsubr(
        "delete-directory-internal",
        super::fileio::builtin_delete_directory_internal,
        1,
        Some(1),
    );
    ctx.defsubr(
        "make-directory-internal",
        super::fileio::builtin_make_directory_internal,
        1,
        Some(1),
    );
    ctx.defsubr(
        "directory-files-and-attributes",
        super::dired::builtin_directory_files_and_attributes,
        1,
        Some(6),
    );
    ctx.defsubr(
        "find-file-name-handler",
        super::fileio::builtin_find_file_name_handler,
        2,
        Some(2),
    );
    ctx.defsubr(
        "file-name-all-completions",
        super::dired::builtin_file_name_all_completions,
        2,
        Some(2),
    );
    ctx.defsubr(
        "file-accessible-directory-p",
        super::fileio::builtin_file_accessible_directory_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "file-name-case-insensitive-p",
        super::fileio::builtin_file_name_case_insensitive_p,
        0,
        None,
    );
    ctx.defsubr(
        "file-newer-than-file-p",
        super::fileio::builtin_file_newer_than_file_p,
        2,
        Some(2),
    );
    ctx.defsubr(
        "verify-visited-file-modtime",
        super::fileio::builtin_verify_visited_file_modtime,
        0,
        Some(1),
    );
    ctx.defsubr(
        "internal-default-interrupt-process",
        super::process::builtin_internal_default_interrupt_process,
        0,
        Some(2),
    );
    ctx.defsubr(
        "internal-default-process-filter",
        super::process::builtin_internal_default_process_filter,
        0,
        None,
    );
    ctx.defsubr(
        "internal-default-process-sentinel",
        super::process::builtin_internal_default_process_sentinel,
        0,
        None,
    );
    ctx.defsubr(
        "internal-default-signal-process",
        super::process::builtin_internal_default_signal_process,
        0,
        None,
    );
    ctx.defsubr(
        "network-lookup-address-info",
        super::process::builtin_network_lookup_address_info,
        1,
        Some(3),
    );
    ctx.defsubr(
        "set-network-process-option",
        super::process::builtin_set_network_process_option,
        3,
        Some(4),
    );
    ctx.defsubr(
        "process-query-on-exit-flag",
        super::process::builtin_process_query_on_exit_flag,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-query-on-exit-flag",
        super::process::builtin_set_process_query_on_exit_flag,
        0,
        None,
    );
    ctx.defsubr(
        "process-inherit-coding-system-flag",
        super::process::builtin_process_inherit_coding_system_flag,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-coding-system",
        super::process::builtin_set_process_coding_system,
        1,
        Some(3),
    );
    ctx.defsubr(
        "set-process-datagram-address",
        super::process::builtin_set_process_datagram_address,
        0,
        None,
    );
    ctx.defsubr(
        "remove-list-of-text-properties",
        super::textprop::builtin_remove_list_of_text_properties,
        0,
        None,
    );
    ctx.defsubr(
        "get-char-property-and-overlay",
        super::textprop::builtin_get_char_property_and_overlay,
        2,
        Some(3),
    );
    ctx.defsubr(
        "next-single-property-change",
        super::textprop::builtin_next_single_property_change,
        2,
        Some(4),
    );
    ctx.defsubr(
        "previous-single-property-change",
        super::textprop::builtin_previous_single_property_change,
        0,
        None,
    );
    ctx.defsubr(
        "line-beginning-position",
        super::navigation::builtin_line_beginning_position,
        0,
        Some(1),
    );
    ctx.defsubr(
        "make-variable-buffer-local",
        super::custom::builtin_make_variable_buffer_local,
        1,
        Some(1),
    );
    ctx.defsubr(
        "active-minibuffer-window",
        super::window_cmds::builtin_active_minibuffer_window,
        0,
        None,
    );
    ctx.defsubr(
        "minibuffer-selected-window",
        super::window_cmds::builtin_minibuffer_selected_window,
        0,
        Some(0),
    );
    ctx.defsubr(
        "window-mode-line-height",
        super::window_cmds::builtin_window_mode_line_height,
        0,
        None,
    );
    ctx.defsubr(
        "window-header-line-height",
        super::window_cmds::builtin_window_header_line_height,
        0,
        None,
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "window-tab-line-height",
            super::window_cmds::builtin_window_tab_line_height,
            0,
            None,
            BuiltinNoEvalPlaceholder::FixnumZero,
        ),
    );
    ctx.defsubr(
        "set-window-display-table",
        super::window_cmds::builtin_set_window_display_table,
        2,
        Some(2),
    );
    ctx.defsubr(
        "set-window-cursor-type",
        super::window_cmds::builtin_set_window_cursor_type,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-scroll-bars",
        super::window_cmds::builtin_set_window_scroll_bars,
        1,
        Some(6),
    );
    ctx.defsubr(
        "set-window-next-buffers",
        super::window_cmds::builtin_set_window_next_buffers,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-prev-buffers",
        super::window_cmds::builtin_set_window_prev_buffers,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-dedicated-p",
        super::window_cmds::builtin_set_window_dedicated_p,
        0,
        None,
    );
    ctx.defsubr(
        "delete-window-internal",
        super::window_cmds::builtin_delete_window_internal,
        1,
        Some(1),
    );
    ctx.defsubr(
        "delete-other-windows-internal",
        super::window_cmds::builtin_delete_other_windows_internal,
        0,
        Some(2),
    );
    ctx.defsubr(
        "window-combination-limit",
        super::window_cmds::builtin_window_combination_limit,
        1,
        Some(1),
    );
    ctx.defsubr(
        "set-window-combination-limit",
        super::window_cmds::builtin_set_window_combination_limit,
        2,
        Some(2),
    );
    ctx.defsubr(
        "window-resize-apply-total",
        super::window_cmds::builtin_window_resize_apply_total,
        0,
        Some(2),
    );
    ctx.defsubr(
        "other-window-for-scrolling",
        super::window_cmds::builtin_other_window_for_scrolling,
        0,
        Some(0),
    );
    ctx.defsubr(
        "select-frame-set-input-focus",
        super::window_cmds::builtin_select_frame_set_input_focus,
        0,
        None,
    );
    ctx.defsubr(
        "modify-frame-parameters",
        super::window_cmds::builtin_modify_frame_parameters,
        2,
        Some(2),
    );
    ctx.defsubr(
        "frame-selected-window",
        super::window_cmds::builtin_frame_selected_window,
        0,
        None,
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "frame-old-selected-window",
            super::window_cmds::builtin_frame_old_selected_window,
            0,
            None,
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "set-frame-selected-window",
            super::window_cmds::builtin_set_frame_selected_window,
            2,
            Some(3),
        ),
    );
    ctx.defsubr(
        "x-display-pixel-width",
        super::display::builtin_x_display_pixel_width,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-display-pixel-height",
        super::display::builtin_x_display_pixel_height,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-server-max-request-size",
        super::display::builtin_x_server_max_request_size,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-display-grayscale-p",
        super::display::builtin_x_display_grayscale_p,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-display-backing-store",
        super::display::builtin_x_display_backing_store,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-display-color-cells",
        super::display::builtin_x_display_color_cells,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-display-save-under",
        super::display::builtin_x_display_save_under,
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-display-set-last-user-time",
        super::display::builtin_x_display_set_last_user_time,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-visual-class",
        super::display::builtin_x_display_visual_class,
        0,
        Some(1),
    );
    ctx.defsubr(
        "minor-mode-key-binding",
        super::interactive::builtin_minor_mode_key_binding,
        1,
        Some(2),
    );
    ctx.defsubr(
        "this-command-keys-vector",
        super::interactive::builtin_this_command_keys_vector,
        0,
        Some(0),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "this-single-command-keys",
            super::interactive::builtin_this_single_command_keys,
            0,
            Some(0),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::placeholder(
            "this-single-command-raw-keys",
            super::interactive::builtin_this_single_command_raw_keys,
            0,
            Some(0),
            BuiltinNoEvalPlaceholder::Nil,
        ),
    );
    ctx.defsubr(
        "clear-this-command-keys",
        super::interactive::builtin_clear_this_command_keys,
        0,
        Some(1),
    );
    ctx.defsubr(
        "waiting-for-user-input-p",
        super::reader::builtin_waiting_for_user_input_p_ctx,
        0,
        Some(0),
    );
    ctx.defsubr(
        "minibuffer-prompt",
        super::minibuffer::builtin_minibuffer_prompt_ctx,
        0,
        Some(0),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "minibuffer-prompt-end",
            super::minibuffer::builtin_minibuffer_prompt_end_ctx,
            0,
            Some(0),
        ),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "innermost-minibuffer-p",
            super::minibuffer::builtin_innermost_minibuffer_p_ctx,
            0,
            None,
        ),
    );
    ctx.defsubr(
        "backtrace--frames-from-thread",
        super::misc::builtin_backtrace_frames_from_thread,
        1,
        Some(1),
    );
    ctx.defsubr(
        "abort-minibuffers",
        super::minibuffer::builtin_abort_minibuffers_ctx,
        0,
        Some(0),
    );
    ctx.defsubr(
        "set-marker-insertion-type",
        super::marker::builtin_set_marker_insertion_type,
        0,
        None,
    );
    ctx.defsubr(
        "set-standard-case-table",
        super::casetab::builtin_set_standard_case_table,
        0,
        None,
    );
    ctx.defsubr(
        "get-unused-category",
        super::category::builtin_get_unused_category,
        0,
        Some(1),
    );
    ctx.defsubr(
        "standard-category-table",
        super::category::builtin_standard_category_table,
        0,
        None,
    );
    ctx.defsubr(
        "upcase-initials-region",
        super::casefiddle::builtin_upcase_initials_region,
        2,
        Some(3),
    );
    ctx.defsubr(
        "buffer-substring-no-properties",
        |eval, args| super::editfns::builtin_buffer_substring_no_properties(eval, args),
        0,
        None,
    );

    // Pure builtins from builtins_extra (previously in old match dispatch).
    // These don't need &mut Context, so we wrap them.
    macro_rules! defsubr_pure {
        ($ctx:expr, $name:expr, $func:expr) => {
            $ctx.defsubr($name, |_eval, args| $func(args), 0, None);
        };
    }
    defsubr_pure!(ctx, "take", super::builtins_extra::builtin_take);
    defsubr_pure!(
        ctx,
        "assoc-string",
        super::builtins_extra::builtin_assoc_string
    );
    defsubr_pure!(
        ctx,
        "string-search",
        super::builtins_extra::builtin_string_search
    );
    defsubr_pure!(
        ctx,
        "bare-symbol",
        super::builtins_extra::builtin_bare_symbol
    );
    defsubr_pure!(
        ctx,
        "bare-symbol-p",
        super::builtins_extra::builtin_bare_symbol_p
    );
    defsubr_pure!(ctx, "byteorder", super::builtins_extra::builtin_byteorder);
    defsubr_pure!(
        ctx,
        "car-less-than-car",
        super::builtins_extra::builtin_car_less_than_car
    );
    defsubr_pure!(
        ctx,
        "proper-list-p",
        super::builtins_extra::builtin_proper_list_p
    );
    defsubr_pure!(ctx, "subrp", super::builtins_extra::builtin_subrp);
    defsubr_pure!(
        ctx,
        "byte-code-function-p",
        super::builtins_extra::builtin_byte_code_function_p
    );
    defsubr_pure!(ctx, "closurep", super::builtins_extra::builtin_closurep);
    defsubr_pure!(ctx, "natnump", super::builtins_extra::builtin_natnump);
    // GNU defines `fixnump` and `bignump` in `lisp/subr.el` (not in C),
    // so they must come from the loaded Lisp source — registering Rust
    // subrs here would shadow the elisp definitions and make
    // `(subrp (symbol-function 'fixnump))` return t instead of nil.
    defsubr_pure!(
        ctx,
        "user-login-name",
        super::builtins_extra::builtin_user_login_name
    );
    defsubr_pure!(
        ctx,
        "user-real-login-name",
        super::builtins_extra::builtin_user_real_login_name
    );
    defsubr_pure!(
        ctx,
        "user-full-name",
        super::builtins_extra::builtin_user_full_name
    );
    defsubr_pure!(
        ctx,
        "system-name",
        super::builtins_extra::builtin_system_name
    );
    defsubr_pure!(ctx, "emacs-pid", super::builtins_extra::builtin_emacs_pid);
    defsubr_pure!(
        ctx,
        "memory-use-counts",
        super::builtins_extra::builtin_memory_use_counts
    );

    // -----------------------------------------------------------------------
    // Additional builtins registered via defsubr.
    // -----------------------------------------------------------------------

    // -- Arithmetic --
    ctx.defsubr("+", super::builtins::arithmetic::builtin_add, 0, None);
    ctx.defsubr("-", super::builtins::arithmetic::builtin_sub, 0, None);
    ctx.defsubr("*", |_ctx, args| builtin_mul(args), 0, None);
    ctx.defsubr("/", |_ctx, args| builtin_div(args), 1, None);
    ctx.defsubr("%", |_ctx, args| builtin_percent(args), 2, Some(2));
    ctx.defsubr("mod", |_ctx, args| builtin_mod(args), 2, Some(2));
    ctx.defsubr("1+", |_ctx, args| builtin_add1(args), 1, Some(1));
    ctx.defsubr("1-", |_ctx, args| builtin_sub1(args), 1, Some(1));
    ctx.defsubr("max", |ctx, args| builtin_max(ctx, args), 1, None);
    ctx.defsubr("min", |ctx, args| builtin_min(ctx, args), 1, None);
    ctx.defsubr("abs", |_ctx, args| builtin_abs(args), 1, Some(1));

    // -- Logical / bitwise --
    ctx.defsubr("logand", |_ctx, args| builtin_logand(args), 0, None);
    ctx.defsubr("logior", |_ctx, args| builtin_logior(args), 0, None);
    ctx.defsubr("logxor", |_ctx, args| builtin_logxor(args), 0, None);
    ctx.defsubr("lognot", |_ctx, args| builtin_lognot(args), 1, Some(1));
    ctx.defsubr("ash", |_ctx, args| builtin_ash(args), 2, Some(2));

    // -- Numeric comparisons --
    ctx.defsubr("=", builtin_num_eq, 1, None);
    ctx.defsubr("<", builtin_num_lt, 1, None);
    ctx.defsubr("<=", builtin_num_le, 1, None);
    ctx.defsubr(">", builtin_num_gt, 1, None);
    ctx.defsubr(">=", builtin_num_ge, 1, None);
    ctx.defsubr("/=", builtin_num_ne, 2, Some(2));

    // -- Type predicates --
    ctx.defsubr_1("null", builtin_null_1, 1);
    ctx.defsubr_1("not", builtin_not_1, 1);
    ctx.defsubr_1("atom", builtin_atom_1, 1);
    ctx.defsubr_1("consp", builtin_consp_1, 1);
    ctx.defsubr_1("listp", builtin_listp_1, 1);
    ctx.defsubr(
        "list-of-strings-p",
        |_ctx, args| builtin_list_of_strings_p(args),
        0,
        None,
    );
    ctx.defsubr_1("nlistp", builtin_nlistp_1, 1);
    ctx.defsubr_1("symbolp", builtin_symbolp_1, 1);
    ctx.defsubr_1("booleanp", builtin_booleanp_1, 1);
    ctx.defsubr_1("numberp", builtin_numberp_1, 1);
    ctx.defsubr_1("integerp", builtin_integerp_1, 1);
    ctx.defsubr(
        "integer-or-null-p",
        |_ctx, args| builtin_integer_or_null_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "string-or-null-p",
        |_ctx, args| builtin_string_or_null_p(args),
        0,
        None,
    );
    ctx.defsubr_1("floatp", builtin_floatp_1, 1);
    ctx.defsubr_1("stringp", builtin_stringp_1, 1);
    ctx.defsubr_1("vectorp", builtin_vectorp_1, 1);
    ctx.defsubr(
        "characterp",
        |_ctx, args| builtin_characterp(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "char-uppercase-p",
        |_ctx, args| builtin_char_uppercase_p(args),
        0,
        None,
    );
    ctx.defsubr_1("keywordp", builtin_keywordp_1, 1);
    ctx.defsubr(
        "hash-table-p",
        |_ctx, args| builtin_hash_table_p(args),
        1,
        Some(1),
    );
    ctx.defsubr("bufferp", |_ctx, args| builtin_bufferp(args), 1, Some(1));
    ctx.defsubr(
        "type-of",
        super::builtins::types::builtin_type_of_with_ctx,
        1,
        Some(1),
    );
    ctx.defsubr(
        "sequencep",
        |_ctx, args| builtin_sequencep(args),
        1,
        Some(1),
    );
    ctx.defsubr("arrayp", |_ctx, args| builtin_arrayp(args), 1, Some(1));
    ctx.defsubr("ignore", |_ctx, args| builtin_ignore(args), 0, None);
    ctx.defsubr(
        "cl-type-of",
        |_ctx, args| builtin_cl_type_of(args),
        1,
        Some(1),
    );

    // -- Equality --
    ctx.defsubr_2("eq", builtin_eq_2, 2);
    ctx.defsubr("eql", |_ctx, args| builtin_eql(args), 2, Some(2));
    ctx.defsubr("equal", |_ctx, args| builtin_equal(args), 2, Some(2));

    // -- Cons / List --
    ctx.defsubr("cons", |_ctx, args| builtin_cons(args), 2, Some(2));
    ctx.defsubr_1("car", builtin_car_1, 1);
    ctx.defsubr_1("cdr", builtin_cdr_1, 1);
    ctx.defsubr_1("car-safe", builtin_car_safe_1, 1);
    ctx.defsubr_1("cdr-safe", builtin_cdr_safe_1, 1);
    ctx.defsubr("setcar", |_ctx, args| builtin_setcar(args), 2, Some(2));
    ctx.defsubr("setcdr", |_ctx, args| builtin_setcdr(args), 2, Some(2));
    ctx.defsubr("list", |_ctx, args| builtin_list(args), 0, None);
    ctx.defsubr("length", |_ctx, args| builtin_length(args), 1, Some(1));
    ctx.defsubr("nth", |_ctx, args| builtin_nth(args), 2, Some(2));
    ctx.defsubr("nthcdr", |_ctx, args| builtin_nthcdr(args), 2, Some(2));
    ctx.defsubr("append", |_ctx, args| builtin_append(args), 0, None);
    ctx.defsubr("reverse", |_ctx, args| builtin_reverse(args), 1, Some(1));
    ctx.defsubr("nreverse", |_ctx, args| builtin_nreverse(args), 1, Some(1));
    ctx.defsubr("member", |_ctx, args| builtin_member(args), 2, Some(2));
    ctx.defsubr("memq", |_ctx, args| builtin_memq(args), 2, Some(2));
    ctx.defsubr("assq", |_ctx, args| builtin_assq(args), 2, Some(2));
    ctx.defsubr(
        "copy-sequence",
        |_ctx, args| builtin_copy_sequence(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "plist-get",
        |_ctx, args| builtin_plist_get(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "plist-put",
        |_ctx, args| builtin_plist_put(args),
        3,
        Some(4),
    );
    ctx.defsubr(
        "copy-alist",
        |_ctx, args| super::misc::builtin_copy_alist(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "rassoc",
        |_ctx, args| super::misc::builtin_rassoc(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "rassq",
        |_ctx, args| super::misc::builtin_rassq(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "make-list",
        |_ctx, args| super::misc::builtin_make_list(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "safe-length",
        |_ctx, args| super::misc::builtin_safe_length(args),
        1,
        Some(1),
    );

    // -- String --
    ctx.defsubr(
        "string-equal",
        |_ctx, args| builtin_string_equal(args),
        2,
        Some(2),
    );
    ctx.defsubr("string=", |_ctx, args| builtin_string_equal(args), 0, None);
    ctx.defsubr(
        "string-lessp",
        |_ctx, args| builtin_string_lessp(args),
        2,
        Some(2),
    );
    ctx.defsubr("string<", |_ctx, args| builtin_string_lessp(args), 0, None);
    ctx.defsubr(
        "string-greaterp",
        |_ctx, args| builtin_string_greaterp(args),
        0,
        None,
    );
    ctx.defsubr(
        "string>",
        |_ctx, args| builtin_string_greaterp(args),
        0,
        None,
    );
    ctx.defsubr(
        "substring",
        |_ctx, args| builtin_substring(args),
        1,
        Some(3),
    );
    ctx.defsubr("concat", |_ctx, args| builtin_concat(args), 0, None);
    ctx.defsubr(
        "unibyte-string",
        |_ctx, args| builtin_unibyte_string(args),
        0,
        None,
    );
    ctx.defsubr(
        "string-to-number",
        |_ctx, args| builtin_string_to_number(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "number-to-string",
        |_ctx, args| builtin_number_to_string(args),
        1,
        Some(1),
    );
    ctx.defsubr("upcase", |_ctx, args| builtin_upcase(args), 1, Some(1));
    ctx.defsubr("downcase", |_ctx, args| builtin_downcase(args), 1, Some(1));
    ctx.defsubr(
        "char-to-string",
        |_ctx, args| builtin_char_to_string(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "string-to-char",
        |_ctx, args| builtin_string_to_char(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "clear-string",
        |_ctx, args| builtin_clear_string(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "compare-strings",
        |_ctx, args| super::fns::builtin_compare_strings(args),
        6,
        Some(7),
    );
    ctx.defsubr(
        "string-version-lessp",
        |_ctx, args| super::fns::builtin_string_version_lessp(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "string-collate-lessp",
        |_ctx, args| super::fns::builtin_string_collate_lessp(args),
        2,
        Some(4),
    );
    ctx.defsubr(
        "string-collate-equalp",
        |_ctx, args| super::fns::builtin_string_collate_equalp(args),
        2,
        Some(4),
    );
    ctx.defsubr(
        "equal-including-properties",
        |_ctx, args| super::fns::builtin_equal_including_properties(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "string-make-multibyte",
        |_ctx, args| super::fns::builtin_string_make_multibyte(args),
        0,
        None,
    );
    ctx.defsubr(
        "string-make-unibyte",
        |_ctx, args| super::fns::builtin_string_make_unibyte(args),
        0,
        None,
    );
    ctx.defsubr(
        "string-to-multibyte",
        |_ctx, args| super::misc::builtin_string_to_multibyte(args),
        0,
        None,
    );
    ctx.defsubr(
        "string-to-unibyte",
        |_ctx, args| super::misc::builtin_string_to_unibyte(args),
        0,
        None,
    );
    ctx.defsubr(
        "string-as-unibyte",
        |_ctx, args| super::misc::builtin_string_as_unibyte(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "string-as-multibyte",
        |_ctx, args| super::misc::builtin_string_as_multibyte(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "unibyte-char-to-multibyte",
        |_ctx, args| super::misc::builtin_unibyte_char_to_multibyte(args),
        0,
        None,
    );
    ctx.defsubr(
        "multibyte-char-to-unibyte",
        |_ctx, args| super::misc::builtin_multibyte_char_to_unibyte(args),
        0,
        None,
    );

    // -- Vector --
    ctx.defsubr(
        "make-vector",
        |_ctx, args| builtin_make_vector(args),
        2,
        Some(2),
    );
    ctx.defsubr("vector", |_ctx, args| builtin_vector(args), 0, None);
    ctx.defsubr("aref", |_ctx, args| builtin_aref(args), 2, Some(2));
    ctx.defsubr("aset", |_ctx, args| builtin_aset(args), 3, Some(3));
    ctx.defsubr("vconcat", |_ctx, args| builtin_vconcat(args), 0, None);

    // -- Hash table --
    ctx.defsubr(
        "make-hash-table",
        |_ctx, args| builtin_make_hash_table(args),
        0,
        None,
    );
    ctx.defsubr("gethash", |_ctx, args| builtin_gethash(args), 2, Some(3));
    ctx.defsubr("puthash", |_ctx, args| builtin_puthash(args), 3, Some(3));
    ctx.defsubr("remhash", |_ctx, args| builtin_remhash(args), 2, Some(2));
    ctx.defsubr("clrhash", |_ctx, args| builtin_clrhash(args), 1, Some(1));
    ctx.defsubr(
        "hash-table-count",
        |_ctx, args| builtin_hash_table_count(args),
        1,
        Some(1),
    );

    // -- Float / math / conversion --
    ctx.defsubr("float", |_ctx, args| builtin_float(args), 1, Some(1));
    ctx.defsubr("truncate", |_ctx, args| builtin_truncate(args), 1, Some(2));
    ctx.defsubr("floor", |_ctx, args| builtin_floor(args), 1, Some(2));
    ctx.defsubr("ceiling", |_ctx, args| builtin_ceiling(args), 1, Some(2));
    ctx.defsubr("round", |_ctx, args| builtin_round(args), 1, Some(2));
    ctx.defsubr(
        "copysign",
        |_ctx, args| super::floatfns::builtin_copysign(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "frexp",
        |_ctx, args| super::floatfns::builtin_frexp(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "ldexp",
        |_ctx, args| super::floatfns::builtin_ldexp(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "logb",
        |_ctx, args| super::floatfns::builtin_logb(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "fceiling",
        |_ctx, args| super::floatfns::builtin_fceiling(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "ffloor",
        |_ctx, args| super::floatfns::builtin_ffloor(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "fround",
        |_ctx, args| super::floatfns::builtin_fround(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "ftruncate",
        |_ctx, args| super::floatfns::builtin_ftruncate(args),
        1,
        Some(1),
    );

    // -- Symbol --
    ctx.defsubr(
        "symbol-name",
        |_ctx, args| builtin_symbol_name(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "make-symbol",
        |_ctx, args| builtin_make_symbol(args),
        1,
        Some(1),
    );

    // -- Misc pure --
    ctx.defsubr(
        "bitmap-spec-p",
        |_ctx, args| builtin_bitmap_spec_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "byte-to-string",
        |_ctx, args| builtin_byte_to_string(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "clear-buffer-auto-save-failure",
        |_ctx, args| builtin_clear_buffer_auto_save_failure(args),
        0,
        None,
    );
    ctx.defsubr(
        "clear-face-cache",
        |_ctx, args| builtin_clear_face_cache(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "combine-after-change-execute",
        |_ctx, args| builtin_combine_after_change_execute(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "command-error-default-function",
        |_ctx, args| builtin_command_error_default_function(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "locale-info",
        |_ctx, args| super::misc::builtin_locale_info(args),
        1,
        Some(1),
    );
    ctx.defsubr("nconc", |_ctx, args| builtin_nconc(args), 0, None);

    // -- Subr introspection --
    ctx.defsubr(
        "subr-name",
        |_ctx, args| super::subr_info::builtin_subr_name(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "subr-arity",
        super::subr_info::builtin_subr_arity,
        1,
        Some(1),
    );
    ctx.defsubr(
        "native-comp-function-p",
        |_ctx, args| super::subr_info::builtin_native_comp_function_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "interpreted-function-p",
        |_ctx, args| super::subr_info::builtin_interpreted_function_p(args),
        0,
        None,
    );
    ctx.defsubr("func-arity", builtin_func_arity, 1, Some(1));

    // -- Character encoding --
    ctx.defsubr(
        "char-width",
        |_ctx, args| crate::encoding::builtin_char_width(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "string-bytes",
        |_ctx, args| crate::encoding::builtin_string_bytes(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "multibyte-string-p",
        |_ctx, args| crate::encoding::builtin_multibyte_string_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "encode-coding-string",
        |_ctx, args| crate::encoding::builtin_encode_coding_string(args),
        2,
        Some(4),
    );
    ctx.defsubr(
        "decode-coding-string",
        |_ctx, args| crate::encoding::builtin_decode_coding_string(args),
        2,
        Some(4),
    );
    ctx.defsubr(
        "char-or-string-p",
        |_ctx, args| crate::encoding::builtin_char_or_string_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "max-char",
        |_ctx, args| crate::encoding::builtin_max_char(args),
        0,
        Some(1),
    );

    // -- Search --
    ctx.defsubr(
        "regexp-quote",
        |_ctx, args| super::search::builtin_regexp_quote(args),
        1,
        Some(1),
    );

    // -- File I/O --
    ctx.defsubr(
        "file-attributes-lessp",
        |_ctx, args| super::dired::builtin_file_attributes_lessp(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "system-users",
        |_ctx, args| super::dired::builtin_system_users(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "system-groups",
        |_ctx, args| super::dired::builtin_system_groups(args),
        0,
        Some(0),
    );

    // -- User / editfns --
    ctx.defsubr(
        "user-uid",
        |_ctx, args| super::editfns::builtin_user_uid(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "user-real-uid",
        |_ctx, args| super::editfns::builtin_user_real_uid(args),
        0,
        Some(0),
    );

    // -- Time/date --
    ctx.defsubr(
        "time-add",
        |_ctx, args| super::timefns::builtin_time_add(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "time-subtract",
        |_ctx, args| super::timefns::builtin_time_subtract(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "time-less-p",
        |_ctx, args| super::timefns::builtin_time_less_p(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "time-equal-p",
        |_ctx, args| super::timefns::builtin_time_equal_p(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "current-time-string",
        |_ctx, args| super::timefns::builtin_current_time_string(args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "current-time-zone",
        |_ctx, args| super::timefns::builtin_current_time_zone(args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "encode-time",
        |_ctx, args| super::timefns::builtin_encode_time(args),
        1,
        None,
    );
    ctx.defsubr(
        "decode-time",
        |_ctx, args| super::timefns::builtin_decode_time(args),
        0,
        Some(3),
    );
    ctx.defsubr(
        "time-convert",
        |_ctx, args| super::timefns::builtin_time_convert(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "set-time-zone-rule",
        |_ctx, args| super::timefns::builtin_set_time_zone_rule(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "format-time-string",
        |_ctx, args| super::format::builtin_format_time_string(args),
        1,
        Some(3),
    );

    // -- Case/char --
    ctx.defsubr(
        "upcase-initials",
        |_ctx, args| super::casefiddle::builtin_upcase_initials(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "char-resolve-modifiers",
        |_ctx, args| super::casefiddle::builtin_char_resolve_modifiers(args),
        0,
        None,
    );

    // -- Font/face --
    ctx.defsubr(
        "fontp",
        |_ctx, args| super::font::builtin_fontp(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "font-spec",
        |_ctx, args| super::font::builtin_font_spec(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-get",
        |_ctx, args| super::font::builtin_font_get(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "font-put",
        |_ctx, args| super::font::builtin_font_put(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "font-xlfd-name",
        |_ctx, args| super::font::builtin_font_xlfd_name(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "close-font",
        |_ctx, args| super::font::builtin_close_font(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "clear-font-cache",
        |_ctx, args| super::font::builtin_clear_font_cache(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "internal-lisp-face-attribute-values",
        |_ctx, args| super::font::builtin_internal_lisp_face_attribute_values(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "internal-lisp-face-equal-p",
        |_ctx, args| super::font::builtin_internal_lisp_face_equal_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-lisp-face-empty-p",
        |_ctx, args| super::font::builtin_internal_lisp_face_empty_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "face-attribute-relative-p",
        |_ctx, args| super::font::builtin_face_attribute_relative_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "merge-face-attribute",
        |_ctx, args| super::font::builtin_merge_face_attribute(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "color-gray-p",
        |_ctx, args| super::font::builtin_color_gray_p(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "color-supported-p",
        |_ctx, args| super::font::builtin_color_supported_p(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "color-distance",
        |_ctx, args| super::font::builtin_color_distance(args),
        2,
        Some(4),
    );
    ctx.defsubr(
        "color-values-from-color-spec",
        |_ctx, args| super::font::builtin_color_values_from_color_spec(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "internal-face-x-get-resource",
        |_ctx, args| super::font::builtin_internal_face_x_get_resource(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "internal-set-font-selection-order",
        |_ctx, args| super::font::builtin_internal_set_font_selection_order(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-set-alternative-font-family-alist",
        |_ctx, args| super::font::builtin_internal_set_alternative_font_family_alist(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "internal-set-alternative-font-registry-alist",
        |_ctx, args| super::font::builtin_internal_set_alternative_font_registry_alist(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-copy-lisp-face",
        super::font::builtin_internal_copy_lisp_face,
        4,
        Some(4),
    );
    ctx.defsubr(
        "internal-get-lisp-face-attribute",
        super::font::builtin_internal_get_lisp_face_attribute,
        2,
        Some(3),
    );
    ctx.defsubr(
        "internal-merge-in-global-face",
        super::font::builtin_internal_merge_in_global_face,
        0,
        None,
    );

    // -- Case table --
    ctx.defsubr(
        "case-table-p",
        |_ctx, args| super::casetab::builtin_case_table_p(args),
        1,
        Some(1),
    );

    // -- Category --
    ctx.defsubr(
        "category-table-p",
        |_ctx, args| super::category::builtin_category_table_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "copy-category-table",
        |_ctx, args| super::category::builtin_copy_category_table(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "make-category-table",
        |_ctx, args| super::category::builtin_make_category_table(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "category-set-mnemonics",
        |_ctx, args| super::category::builtin_category_set_mnemonics(args),
        0,
        None,
    );

    // -- Char-table / bool-vector --
    ctx.defsubr(
        "char-table-p",
        |_ctx, args| super::chartable::builtin_char_table_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "set-char-table-range",
        |_ctx, args| super::chartable::builtin_set_char_table_range(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "char-table-range",
        |_ctx, args| super::chartable::builtin_char_table_range(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "char-table-parent",
        |_ctx, args| super::chartable::builtin_char_table_parent(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-char-table-parent",
        |_ctx, args| super::chartable::builtin_set_char_table_parent(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "char-table-extra-slot",
        |_ctx, args| super::chartable::builtin_char_table_extra_slot(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "set-char-table-extra-slot",
        |_ctx, args| super::chartable::builtin_set_char_table_extra_slot(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "char-table-subtype",
        |_ctx, args| super::chartable::builtin_char_table_subtype(args),
        0,
        None,
    );
    ctx.defsubr(
        "bool-vector",
        |_ctx, args| super::chartable::builtin_bool_vector(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-bool-vector",
        |_ctx, args| super::chartable::builtin_make_bool_vector(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "bool-vector-count-population",
        |_ctx, args| super::chartable::builtin_bool_vector_count_population(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "bool-vector-count-consecutive",
        |_ctx, args| super::chartable::builtin_bool_vector_count_consecutive(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "bool-vector-intersection",
        |_ctx, args| super::chartable::builtin_bool_vector_intersection(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "bool-vector-not",
        |_ctx, args| super::chartable::builtin_bool_vector_not(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "bool-vector-set-difference",
        |_ctx, args| super::chartable::builtin_bool_vector_set_difference(args),
        0,
        None,
    );
    ctx.defsubr(
        "bool-vector-union",
        |_ctx, args| super::chartable::builtin_bool_vector_union(args),
        0,
        None,
    );
    ctx.defsubr(
        "bool-vector-exclusive-or",
        |_ctx, args| super::chartable::builtin_bool_vector_exclusive_or(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "bool-vector-subsetp",
        |_ctx, args| super::chartable::builtin_bool_vector_subsetp(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "make-char-table",
        super::chartable::builtin_make_char_table,
        1,
        Some(2),
    );

    // -- Charset --
    ctx.defsubr(
        "charset-priority-list",
        |_ctx, args| super::charset::builtin_charset_priority_list(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-charset-priority",
        |_ctx, args| super::charset::builtin_set_charset_priority(args),
        1,
        None,
    );
    ctx.defsubr(
        "char-charset",
        |_ctx, args| super::charset::builtin_char_charset(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "charset-id-internal",
        |_ctx, args| super::charset::builtin_charset_id_internal(args),
        0,
        None,
    );
    ctx.defsubr(
        "declare-equiv-charset",
        |_ctx, args| super::charset::builtin_declare_equiv_charset(args),
        4,
        Some(4),
    );
    ctx.defsubr(
        "find-charset-string",
        |_ctx, args| super::charset::builtin_find_charset_string(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "decode-big5-char",
        |_ctx, args| super::charset::builtin_decode_big5_char(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "decode-char",
        |_ctx, args| super::charset::builtin_decode_char(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "decode-sjis-char",
        |_ctx, args| super::charset::builtin_decode_sjis_char(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "encode-big5-char",
        |_ctx, args| super::charset::builtin_encode_big5_char(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "encode-char",
        |_ctx, args| super::charset::builtin_encode_char(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "encode-sjis-char",
        |_ctx, args| super::charset::builtin_encode_sjis_char(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "get-unused-iso-final-char",
        |_ctx, args| super::charset::builtin_get_unused_iso_final_char(args),
        0,
        None,
    );
    ctx.defsubr(
        "clear-charset-maps",
        |_ctx, args| super::charset::builtin_clear_charset_maps(args),
        0,
        None,
    );

    // -- Coding system (eval-dependent via coding_systems field) --
    ctx.defsubr("coding-system-p", defsubr_coding_system_p, 1, Some(1));
    ctx.defsubr("check-coding-system", defsubr_check_coding_system, 0, None);
    ctx.defsubr(
        "check-coding-systems-region",
        defsubr_check_coding_systems_region,
        3,
        Some(3),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state(
            "define-coding-system-internal",
            defsubr_define_coding_system_internal,
            13,
            None,
        ),
    );
    ctx.defsubr(
        "define-coding-system-alias",
        defsubr_define_coding_system_alias,
        2,
        Some(2),
    );
    ctx.defsubr(
        "set-coding-system-priority",
        defsubr_set_coding_system_priority,
        0,
        None,
    );
    ctx.defsubr(
        "set-keyboard-coding-system-internal",
        defsubr_set_keyboard_coding_system_internal,
        1,
        Some(2),
    );
    ctx.defsubr(
        "set-safe-terminal-coding-system-internal",
        defsubr_set_safe_terminal_coding_system_internal,
        1,
        Some(1),
    );
    ctx.defsubr(
        "set-terminal-coding-system-internal",
        defsubr_set_terminal_coding_system_internal,
        1,
        Some(2),
    );
    ctx.defsubr(
        "set-text-conversion-style",
        |_ctx, args| super::coding::builtin_set_text_conversion_style(args),
        0,
        None,
    );
    ctx.defsubr(
        "text-quoting-style",
        |ctx, args| super::coding::builtin_text_quoting_style(ctx, args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "set-buffer-file-coding-system",
        super::coding::builtin_set_buffer_file_coding_system,
        1,
        Some(3),
    );

    // -- CCL (eval-dependent) --
    ctx.defsubr("ccl-program-p", builtin_ccl_program_p, 1, Some(1));
    ctx.defsubr("ccl-execute", builtin_ccl_execute, 2, Some(2));
    ctx.defsubr(
        "ccl-execute-on-string",
        builtin_ccl_execute_on_string,
        3,
        Some(5),
    );
    ctx.defsubr(
        "register-ccl-program",
        builtin_register_ccl_program,
        0,
        None,
    );
    ctx.defsubr(
        "register-code-conversion-map",
        builtin_register_code_conversion_map,
        0,
        None,
    );

    // -- Eval builtins (eval-dependent) --
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("defconst-1", builtin_defconst_1, 2, Some(3)),
    );
    register_builtin(
        ctx,
        BuiltinRegistration::requires_eval_state("defvar-1", builtin_defvar_1, 2, Some(3)),
    );
    ctx.defsubr(
        "yes-or-no-p",
        super::reader::builtin_yes_or_no_p,
        1,
        Some(1),
    );
    ctx.defsubr(
        "locate-file-internal",
        super::lread::builtin_locate_file_internal,
        2,
        Some(4),
    );

    // -- Dispnew --
    ctx.defsubr(
        "redraw-display",
        |_ctx, args| super::dispnew::pure::builtin_redraw_display(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "open-termscript",
        |_ctx, args| super::dispnew::pure::builtin_open_termscript(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "ding",
        |_ctx, args| super::dispnew::pure::builtin_ding(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "frame--z-order-lessp",
        |_ctx, args| super::dispnew::pure::builtin_frame_z_order_lessp(args),
        0,
        None,
    );
    ctx.defsubr(
        "force-window-update",
        |_ctx, args| super::dispnew::pure::builtin_force_window_update(args),
        0,
        Some(1),
    );

    // -- Display/terminal --
    ctx.defsubr(
        "x-export-frames",
        |_ctx, args| super::display::builtin_x_export_frames(args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "x-backspace-delete-keys-p",
        |_ctx, args| super::display::builtin_x_backspace_delete_keys_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-change-window-property",
        |_ctx, args| super::display::builtin_x_change_window_property(args),
        2,
        Some(7),
    );
    ctx.defsubr(
        "x-focus-frame",
        |_ctx, args| super::display::builtin_x_focus_frame(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "x-get-local-selection",
        |_ctx, args| super::display::builtin_x_get_local_selection(args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "x-get-modifier-masks",
        |_ctx, args| super::display::builtin_x_get_modifier_masks(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-get-selection-internal",
        |_ctx, args| super::display::builtin_x_get_selection_internal(args),
        2,
        Some(4),
    );
    ctx.defsubr(
        "x-display-list",
        |_ctx, args| super::display::builtin_x_display_list(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "x-disown-selection-internal",
        |_ctx, args| super::display::builtin_x_disown_selection_internal(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "x-delete-window-property",
        |_ctx, args| super::display::builtin_x_delete_window_property(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "x-frame-edges",
        |_ctx, args| super::display::builtin_x_frame_edges(args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "x-frame-geometry",
        |_ctx, args| super::display::builtin_x_frame_geometry(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "x-frame-list-z-order",
        |_ctx, args| super::display::builtin_x_frame_list_z_order(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-frame-restack",
        |_ctx, args| super::display::builtin_x_frame_restack(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "x-family-fonts",
        |_ctx, args| super::display::builtin_x_family_fonts(args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "x-get-atom-name",
        |_ctx, args| super::display::builtin_x_get_atom_name(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-mouse-absolute-pixel-position",
        |_ctx, args| super::display::builtin_x_mouse_absolute_pixel_position(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-own-selection-internal",
        |_ctx, args| super::display::builtin_x_own_selection_internal(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "x-parse-geometry",
        |_ctx, args| super::display::builtin_x_parse_geometry(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "x-popup-dialog",
        |_ctx, args| super::display::builtin_x_popup_dialog(args),
        2,
        Some(3),
    );
    ctx.defsubr(
        "x-popup-menu",
        |_ctx, args| super::display::builtin_x_popup_menu(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "x-register-dnd-atom",
        |_ctx, args| super::display::builtin_x_register_dnd_atom(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-selection-exists-p",
        |_ctx, args| super::display::builtin_x_selection_exists_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-selection-owner-p",
        |_ctx, args| super::display::builtin_x_selection_owner_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-hide-tip",
        |_ctx, args| super::display::builtin_x_hide_tip(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "x-internal-focus-input-context",
        |_ctx, args| super::display::builtin_x_internal_focus_input_context(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-send-client-message",
        |_ctx, args| super::display::builtin_x_send_client_message(args),
        6,
        Some(6),
    );
    ctx.defsubr(
        "x-show-tip",
        |_ctx, args| super::display::builtin_x_show_tip(args),
        1,
        Some(6),
    );
    ctx.defsubr(
        "x-set-mouse-absolute-pixel-position",
        |_ctx, args| super::display::builtin_x_set_mouse_absolute_pixel_position(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "x-synchronize",
        |_ctx, args| super::display::builtin_x_synchronize(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "x-translate-coordinates",
        |_ctx, args| super::display::builtin_x_translate_coordinates(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-uses-old-gtk-dialog",
        |_ctx, args| super::display::builtin_x_uses_old_gtk_dialog(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-window-property",
        |_ctx, args| super::display::builtin_x_window_property(args),
        1,
        Some(6),
    );
    ctx.defsubr(
        "x-window-property-attributes",
        |_ctx, args| super::display::builtin_x_window_property_attributes(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-wm-set-size-hint",
        |_ctx, args| super::display::builtin_x_wm_set_size_hint(args),
        0,
        None,
    );
    ctx.defsubr(
        "terminal-list",
        |_ctx, args| super::terminal::pure::builtin_terminal_list(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "delete-terminal",
        |ctx, args| super::terminal::pure::builtin_delete_terminal(ctx, args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "make-terminal-frame",
        |_ctx, args| super::terminal::pure::builtin_make_terminal_frame(args),
        1,
        Some(1),
    );

    // -- Image --
    ctx.defsubr(
        "image-size",
        |_ctx, args| super::image::builtin_image_size(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "image-mask-p",
        |_ctx, args| super::image::builtin_image_mask_p(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "image-flush",
        |_ctx, args| super::image::builtin_image_flush(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "clear-image-cache",
        |_ctx, args| super::image::builtin_clear_image_cache(args),
        0,
        Some(2),
    );
    ctx.defsubr(
        "image-cache-size",
        |_ctx, args| super::image::builtin_image_cache_size(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "image-metadata",
        |_ctx, args| super::image::builtin_image_metadata(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "imagep",
        |_ctx, args| super::image::builtin_imagep(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "image-transforms-p",
        |_ctx, args| super::image::builtin_image_transforms_p(args),
        0,
        Some(1),
    );

    // -- Display engine (xdisp) --
    ctx.defsubr(
        "invisible-p",
        |_ctx, args| super::xdisp::builtin_invisible_p(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "line-pixel-height",
        |_ctx, args| super::xdisp::builtin_line_pixel_height(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "move-point-visually",
        |_ctx, args| super::xdisp::builtin_move_point_visually(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "lookup-image-map",
        |_ctx, args| super::xdisp::builtin_lookup_image_map(args),
        3,
        Some(3),
    );
    ctx.defsubr(
        "current-bidi-paragraph-direction",
        |_ctx, args| super::xdisp::builtin_current_bidi_paragraph_direction(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "bidi-resolved-levels",
        |_ctx, args| super::xdisp::builtin_bidi_resolved_levels(args),
        0,
        Some(1),
    );
    ctx.defsubr(
        "bidi-find-overridden-directionality",
        |_ctx, args| super::xdisp::builtin_bidi_find_overridden_directionality(args),
        3,
        Some(4),
    );
    ctx.defsubr(
        "move-to-window-line",
        super::xdisp::builtin_move_to_window_line,
        1,
        Some(1),
    );
    ctx.defsubr(
        "line-number-display-width",
        |_ctx, args| super::xdisp::builtin_line_number_display_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "long-line-optimizations-p",
        |_ctx, args| super::xdisp::builtin_long_line_optimizations_p(args),
        0,
        Some(0),
    );

    // -- XML/decompress --
    ctx.defsubr(
        "libxml-parse-html-region",
        |_ctx, args| super::xml::builtin_libxml_parse_html_region(args),
        0,
        Some(4),
    );
    ctx.defsubr(
        "libxml-parse-xml-region",
        |_ctx, args| super::xml::builtin_libxml_parse_xml_region(args),
        0,
        Some(4),
    );
    ctx.defsubr(
        "libxml-available-p",
        |_ctx, args| super::xml::builtin_libxml_available_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "zlib-available-p",
        |_ctx, args| super::xml::builtin_zlib_available_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "zlib-decompress-region",
        |_ctx, args| super::xml::builtin_zlib_decompress_region(args),
        2,
        Some(3),
    );

    // -- Native compilation compatibility --
    ctx.defsubr(
        "comp--compile-ctxt-to-file0",
        |_ctx, args| super::comp::builtin_comp_compile_ctxt_to_file0(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "comp--init-ctxt",
        |_ctx, args| super::comp::builtin_comp_init_ctxt(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "comp--install-trampoline",
        |_ctx, args| super::comp::builtin_comp_install_trampoline(args),
        2,
        Some(2),
    );
    ctx.defsubr(
        "comp--late-register-subr",
        |_ctx, args| super::comp::builtin_comp_late_register_subr(args),
        7,
        Some(7),
    );
    ctx.defsubr(
        "comp--register-lambda",
        |_ctx, args| super::comp::builtin_comp_register_lambda(args),
        7,
        Some(7),
    );
    ctx.defsubr(
        "comp--register-subr",
        |_ctx, args| super::comp::builtin_comp_register_subr(args),
        7,
        Some(7),
    );
    ctx.defsubr(
        "comp--release-ctxt",
        |_ctx, args| super::comp::builtin_comp_release_ctxt(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "comp--subr-signature",
        |_ctx, args| super::comp::builtin_comp_subr_signature(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "comp-el-to-eln-filename",
        |_ctx, args| super::comp::builtin_comp_el_to_eln_filename(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "comp-el-to-eln-rel-filename",
        |_ctx, args| super::comp::builtin_comp_el_to_eln_rel_filename(args),
        1,
        Some(1),
    );
    ctx.defsubr(
        "comp-libgccjit-version",
        |_ctx, args| super::comp::builtin_comp_libgccjit_version(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "comp-native-compiler-options-effective-p",
        |_ctx, args| super::comp::builtin_comp_native_compiler_options_effective_p(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "comp-native-driver-options-effective-p",
        |_ctx, args| super::comp::builtin_comp_native_driver_options_effective_p(args),
        0,
        Some(0),
    );

    // -- DBus compatibility --
    ctx.defsubr(
        "dbus--init-bus",
        |_ctx, args| super::dbus::builtin_dbus_init_bus(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "dbus-get-unique-name",
        |_ctx, args| super::dbus::builtin_dbus_get_unique_name(args),
        0,
        None,
    );
    ctx.defsubr(
        "dbus-message-internal",
        |_ctx, args| super::dbus::builtin_dbus_message_internal(args),
        0,
        None,
    );

    // -- Documentation/help --
    ctx.defsubr(
        "Snarf-documentation",
        |_ctx, args| super::doc::builtin_snarf_documentation(args),
        1,
        Some(1),
    );

    // -- JSON --
    ctx.defsubr(
        "json-serialize",
        |_ctx, args| super::json::builtin_json_serialize(args),
        1,
        None,
    );
    ctx.defsubr(
        "json-parse-string",
        |_ctx, args| super::json::builtin_json_parse_string(args),
        1,
        None,
    );

    // -- Composite --
    ctx.defsubr(
        "compose-string-internal",
        |_ctx, args| super::composite::builtin_compose_string_internal(args),
        3,
        Some(5),
    );
    ctx.defsubr(
        "find-composition-internal",
        |_ctx, args| super::composite::builtin_find_composition_internal(args),
        4,
        Some(4),
    );
    ctx.defsubr(
        "composition-get-gstring",
        |_ctx, args| super::composite::builtin_composition_get_gstring(args),
        4,
        Some(4),
    );
    ctx.defsubr(
        "clear-composition-cache",
        |_ctx, args| super::composite::builtin_clear_composition_cache(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "composition-sort-rules",
        |_ctx, args| super::composite::builtin_composition_sort_rules(args),
        1,
        Some(1),
    );

    // -- Marker --
    ctx.defsubr(
        "markerp",
        |_ctx, args| super::marker::builtin_markerp(args),
        1,
        Some(1),
    );

    // -- Lread --
    ctx.defsubr(
        "get-load-suffixes",
        |_ctx, args| super::lread::builtin_get_load_suffixes(args),
        0,
        Some(0),
    );
    ctx.defsubr(
        "read-coding-system",
        |_ctx, args| super::lread::builtin_read_coding_system(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "read-non-nil-coding-system",
        |_ctx, args| super::lread::builtin_read_non_nil_coding_system(args),
        1,
        Some(1),
    );

    // -- Base64/hash --
    ctx.defsubr(
        "base64-encode-string",
        |_ctx, args| super::fns::builtin_base64_encode_string(args),
        1,
        Some(2),
    );
    ctx.defsubr(
        "base64-decode-string",
        |_ctx, args| super::fns::builtin_base64_decode_string(args),
        1,
        Some(3),
    );
    ctx.defsubr(
        "base64url-encode-string",
        |_ctx, args| super::fns::builtin_base64url_encode_string(args),
        1,
        Some(2),
    );

    // -- Window builtins: display-buffer, switch-to-buffer, pop-to-buffer --
    ctx.defsubr(
        "switch-to-buffer",
        super::window_cmds::builtin_switch_to_buffer,
        1,
        Some(3),
    );
    ctx.defsubr(
        "display-buffer",
        super::window_cmds::builtin_display_buffer,
        1,
        Some(3),
    );
    ctx.defsubr(
        "pop-to-buffer",
        super::window_cmds::builtin_pop_to_buffer,
        1,
        Some(3),
    );

    // -- Window tree / resize builtins --
    ctx.defsubr(
        "balance-windows",
        super::window_cmds::builtin_balance_windows,
        0,
        Some(1),
    );
    ctx.defsubr(
        "enlarge-window",
        super::window_cmds::builtin_enlarge_window,
        1,
        Some(2),
    );
    ctx.defsubr(
        "shrink-window",
        super::window_cmds::builtin_shrink_window,
        1,
        Some(2),
    );
    ctx.defsubr(
        "window-tree",
        super::window_cmds::builtin_window_tree,
        0,
        Some(1),
    );

    // GNU exposes public evaluator-owned entries like `if` and `throw` as
    // real subrs in the function cell even though they are dispatched by the
    // evaluator rather than the ordinary builtin function table.
    ctx.materialize_public_evaluator_function_cells();
}
