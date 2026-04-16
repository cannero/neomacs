//! Subr/primitive introspection builtins.
//!
//! Provides type predicates and introspection for callable objects:
//! - `subrp`, `subr-name`, `subr-arity`
//! - `commandp`, `functionp`, `byte-code-function-p`, `closurep`
//! - `interpreted-function-p`, `special-form-p`, `macrop`
//! - `func-arity`, `indirect-function`

use super::error::{EvalResult, Flow, signal};
use super::intern::{SymId, lookup_interned, resolve_name, resolve_sym};
use super::value::*;
use crate::tagged::header::{SubrDispatchKind, SubrObj};
use std::collections::HashMap;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Context/public callable classification
// ---------------------------------------------------------------------------

pub(crate) const PUBLIC_SPECIAL_FORM_NAMES: &[&str] = &[
    "quote",
    "function",
    "let",
    "let*",
    "setq",
    "if",
    "and",
    "or",
    "cond",
    "while",
    "progn",
    "prog1",
    "defvar",
    "defconst",
    "catch",
    "unwind-protect",
    "condition-case",
    "interactive",
    "save-excursion",
    "save-restriction",
    "save-current-buffer",
];

pub(crate) const PUBLIC_EVALUATOR_CALLABLE_NAMES: &[&str] = &["throw"];

pub(crate) fn public_evaluator_subr_names() -> impl Iterator<Item = &'static str> {
    PUBLIC_SPECIAL_FORM_NAMES
        .iter()
        .copied()
        .chain(PUBLIC_EVALUATOR_CALLABLE_NAMES.iter().copied())
}

/// Returns true if `name` is recognized by the evaluator's special-form
/// dispatch path.
///
/// This list mirrors `Context::try_special_form()` in `eval.rs`.
/// Only includes forms that are evaluator-owned by construction:
/// GNU C special forms, evaluator internals, and NeoVM-owned runtime forms.
pub(crate) fn is_evaluator_special_form_name(name: &str) -> bool {
    matches!(
        name,
        // GNU Emacs C special forms (eval.c UNEVALLED)
        "quote"
            | "function"
            | "let"
            | "let*"
            | "setq"
            | "if"
            | "and"
            | "or"
            | "cond"
            | "while"
            | "progn"
            | "prog1"
            | "defvar"
            | "defconst"
            | "catch"
            | "unwind-protect"
            | "condition-case"
            // GNU Emacs C special forms (editfns.c)
            | "save-excursion"
            | "save-current-buffer"
            | "save-restriction"
            // GNU Emacs C special form (callint.c) — stub
            | "interactive"
            // lambda is not a C special form but is handled specially by the evaluator
            | "lambda"
            // NeoVM-specific: bytecode handling
            | "byte-code-literal"
            | "byte-code"
    )
}

/// Returns true for special forms exposed by `special-form-p`.
///
/// Emacs distinguishes evaluator internals from public special forms:
/// many evaluator-recognized constructs are macros/functions in user-visible
/// introspection.
fn is_public_special_form_name(name: &str) -> bool {
    PUBLIC_SPECIAL_FORM_NAMES.contains(&name)
}

pub(crate) fn is_special_form(name: &str) -> bool {
    is_public_special_form_name(name)
}

/// Returns true for evaluator special forms that should NOT be expanded
/// by `macroexpand`.
///
/// After removing the Rust sf_ forms that duplicated Elisp macros,
/// there are no longer any forms that need this skip.
pub(crate) fn is_evaluator_sf_skip_macroexpand(_name: &str) -> bool {
    false
}

pub(crate) fn is_evaluator_callable_name(name: &str) -> bool {
    // `throw` is an evaluator-dispatched entry that still behaves as a normal
    // callable symbol in introspection (`fboundp`/`functionp`/`symbol-function`).
    PUBLIC_EVALUATOR_CALLABLE_NAMES.contains(&name)
}

pub(crate) fn compat_subr_dispatch_kind(name: &str) -> SubrDispatchKind {
    if is_public_special_form_name(name) {
        SubrDispatchKind::SpecialForm
    } else if is_evaluator_callable_name(name) {
        SubrDispatchKind::ContextCallable
    } else {
        SubrDispatchKind::Builtin
    }
}

// ---------------------------------------------------------------------------
// Arity helpers
// ---------------------------------------------------------------------------

/// GNU Emacs special-form minimum arities.
///
/// These come from the C `DEFUN` declarations in:
/// - `src/eval.c`
/// - `src/editfns.c`
/// - `src/callint.c`
///
/// They are observable via `(subr-arity ...)`, so we keep them explicit here
/// instead of inferring them indirectly from tests or registration defaults.
const GNU_SPECIAL_FORM_MIN_ARITIES: &[(&str, u16)] = &[
    ("quote", 1),
    ("function", 1),
    ("let", 1),
    ("let*", 1),
    ("setq", 0),
    ("if", 2),
    ("and", 0),
    ("or", 0),
    ("cond", 0),
    ("while", 1),
    ("progn", 0),
    ("prog1", 1),
    ("defvar", 1),
    ("defconst", 2),
    ("catch", 1),
    ("unwind-protect", 1),
    ("condition-case", 2),
    ("interactive", 0),
    ("save-excursion", 0),
    ("save-restriction", 0),
    ("save-current-buffer", 0),
];

fn lookup_special_form_min_arity(name: &str) -> Option<u16> {
    GNU_SPECIAL_FORM_MIN_ARITIES
        .iter()
        .find_map(|(special_name, min)| (*special_name == name).then_some(*min))
}

/// Build a cons cell `(MIN . MAX)` representing arity.
/// `max` of `None` means "many" (unbounded &rest), represented by the
/// symbol `many`.
fn arity_cons(min: usize, max: Option<usize>) -> Value {
    let min_val = Value::fixnum(min as i64);
    let max_val = match max {
        Some(n) => Value::fixnum(n as i64),
        None => Value::symbol("many"),
    };
    Value::cons(min_val, max_val)
}

fn arity_unevalled(min: usize) -> Value {
    Value::cons(Value::fixnum(min as i64), Value::symbol("unevalled"))
}

fn is_cxr_subr_name(name: &str) -> bool {
    let Some(inner) = name.strip_prefix('c').and_then(|s| s.strip_suffix('r')) else {
        return false;
    };
    !inner.is_empty() && inner.chars().all(|ch| ch == 'a' || ch == 'd')
}

static SUBR_ARITY_ORACLE: OnceLock<HashMap<String, (u16, Option<u16>)>> = OnceLock::new();

fn build_subr_arity_oracle() -> HashMap<String, (u16, Option<u16>)> {
    let mut map = HashMap::new();
    for line in include_str!("subr_info_test.rs").lines() {
        let Some(rest) = line.split("assert_subr_arity(\"").nth(1) else {
            continue;
        };
        let Some((name, rest)) = rest.split_once("\", ") else {
            continue;
        };
        let Some((min, rest)) = rest.split_once(", ") else {
            continue;
        };
        let Ok(min) = min.parse::<u16>() else {
            continue;
        };
        let max = if let Some(rest) = rest.strip_prefix("Some(") {
            let Some(max) = rest.split(')').next() else {
                continue;
            };
            let Ok(max) = max.parse::<u16>() else {
                continue;
            };
            Some(max)
        } else if rest.starts_with("None") {
            None
        } else {
            continue;
        };
        map.entry(name.to_string()).or_insert((min, max));
    }
    map
}

pub(crate) fn lookup_compat_subr_arity(name: &str) -> Option<(u16, Option<u16>)> {
    if let Some(min) = lookup_special_form_min_arity(name) {
        return Some((min, None));
    }
    SUBR_ARITY_ORACLE
        .get_or_init(build_subr_arity_oracle)
        .get(name)
        .copied()
}

pub(crate) fn lookup_compat_subr_metadata(
    name: &str,
    declared_min: u16,
    declared_max: Option<u16>,
) -> (u16, Option<u16>, SubrDispatchKind) {
    let (min, max) = lookup_compat_subr_arity(name).unwrap_or((declared_min, declared_max));
    (min, max, compat_subr_dispatch_kind(name))
}

fn subr_arity_value(name: &str) -> Value {
    if compat_subr_dispatch_kind(name) == SubrDispatchKind::SpecialForm {
        let min = lookup_special_form_min_arity(name)
            .or_else(|| lookup_compat_subr_arity(name).map(|(min, _)| min))
            .unwrap_or(0);
        arity_unevalled(min as usize)
    } else if let Some((min, max)) = lookup_compat_subr_arity(name) {
        arity_cons(min as usize, max.map(|m| m as usize))
    } else if is_cxr_subr_name(name) {
        arity_cons(1, Some(1))
    } else {
        arity_cons(0, None)
    }
}

pub(crate) fn dispatch_subr_arity_value(name: &str) -> Value {
    subr_arity_value(name)
}

fn is_macro_object(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Macro) => true,
        ValueKind::Cons => value.cons_car().as_symbol_name() == Some("macro"),
        _ => false,
    }
}

fn autoload_macro_marker(value: &Value) -> Option<Value> {
    if !super::autoload::is_autoload_value(value) {
        return None;
    }

    let items = list_to_vec(value)?;
    let autoload_type = items.get(4)?;
    if autoload_type.as_symbol_name() == Some("macro") {
        Some(Value::list(vec![Value::symbol("macro"), Value::T]))
    } else if autoload_type.is_t() {
        // GNU Emacs uses `t` as a legacy macro marker for some startup
        // autoloads (notably `pcase-dolist`), and `macrop` returns `(t)`.
        Some(Value::list(vec![Value::T]))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Pure builtins (no evaluator access)
// ---------------------------------------------------------------------------

/// `(subr-name SUBR)` -- return the name of a subroutine as a string.
pub(crate) fn builtin_subr_name(args: Vec<Value>) -> EvalResult {
    expect_args("subr-name", &args, 1)?;
    match args[0].kind() {
        ValueKind::Subr(id) => Ok(Value::string(resolve_sym(id))),
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = args[0].as_subr_id().unwrap();
            Ok(Value::string(resolve_sym(id)))
        }
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("subrp"), args[0]],
        )),
    }
}

/// `(subr-arity SUBR)` -- return (MIN . MAX) cons cell for argument counts.
///
/// Reads arity from the canonical heap subr object (single source of truth).
/// Falls back to the hardcoded table for builtins not yet updated.
pub(crate) fn builtin_subr_arity(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("subr-arity", &args, 1)?;
    match args[0].kind() {
        ValueKind::Subr(id) => Ok(subr_arity_from_registry(ctx, id)),
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = args[0].as_subr_id().unwrap();
            Ok(subr_arity_from_registry(ctx, id))
        }
        _other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("subrp"), args[0]],
        )),
    }
}

/// Look up arity from the global subr table first, then fall back.
fn subr_arity_from_registry(ctx: &super::eval::Context, sym_id: SymId) -> Value {
    let name = resolve_sym(sym_id);

    // GNU Emacs: special forms (UNEVALLED) return (MIN . unevalled).
    // The Elisp `special-form-p` checks `(eq (cdr (subr-arity x)) 'unevalled)`.
    if ctx.subr_dispatch_kind_or_compat(sym_id) == SubrDispatchKind::SpecialForm {
        let min = super::eval::lookup_global_subr_entry(sym_id)
            .map(|e| special_form_min_arity_from_entry(&e))
            .unwrap_or_else(|| {
                lookup_compat_subr_arity(name)
                    .map(|(min, _)| min as usize)
                    .unwrap_or(0)
            });
        return arity_unevalled(min);
    }

    if let Some(entry) = super::eval::lookup_global_subr_entry(sym_id) {
        // If registration has actual arity (not the default 0/None),
        // use it as the authoritative source.
        let min = entry.min_args;
        let max = entry.max_args;
        if min > 0 || max.is_some() {
            return arity_cons(min as usize, max.map(|m| m as usize));
        }
    }
    // Fall back for builtins still using (0, None)
    subr_arity_value(name)
}

fn subr_arity_from_value(subr: Value) -> Option<Value> {
    // Try global table first (new path)
    if let Some(sym_id) = subr.as_subr_id() {
        if let Some(entry) = super::eval::lookup_global_subr_entry(sym_id) {
            if entry.dispatch_kind == SubrDispatchKind::SpecialForm {
                return Some(arity_unevalled(special_form_min_arity_from_entry(&entry)));
            }
            if entry.min_args > 0 || entry.max_args.is_some() {
                return Some(arity_cons(
                    entry.min_args as usize,
                    entry.max_args.map(|m| m as usize),
                ));
            }
        }
    }
    // Old heap path fallback
    if !matches!(subr.kind(), ValueKind::Veclike(VecLikeType::Subr)) {
        return None;
    }
    let ptr = subr.as_veclike_ptr()? as *const SubrObj;
    let subr = unsafe { &*ptr };
    if subr.dispatch_kind == SubrDispatchKind::SpecialForm {
        return Some(arity_unevalled(special_form_min_arity(subr)));
    }
    if subr.min_args > 0 || subr.max_args.is_some() {
        Some(arity_cons(
            subr.min_args as usize,
            subr.max_args.map(|m| m as usize),
        ))
    } else {
        None
    }
}

fn special_form_min_arity(subr: &SubrObj) -> usize {
    if subr.min_args > 0 || subr.max_args.is_some() {
        subr.min_args as usize
    } else {
        lookup_special_form_min_arity(resolve_name(subr.name))
            .or_else(|| lookup_compat_subr_arity(resolve_name(subr.name)).map(|(min, _)| min))
            .map(|min| min as usize)
            .unwrap_or(0)
    }
}

fn special_form_min_arity_from_entry(entry: &super::eval::SubrEntry) -> usize {
    if entry.min_args > 0 || entry.max_args.is_some() {
        entry.min_args as usize
    } else {
        lookup_special_form_min_arity(resolve_name(entry.name_id))
            .or_else(|| lookup_compat_subr_arity(resolve_name(entry.name_id)).map(|(min, _)| min))
            .map(|min| min as usize)
            .unwrap_or(0)
    }
}

/// `(native-comp-function-p OBJECT)` -- return t if OBJECT is a native-compiled
/// function object.
///
/// NeoVM does not currently model native-compiled function objects, so this
/// always returns nil.
pub(crate) fn builtin_native_comp_function_p(args: Vec<Value>) -> EvalResult {
    expect_args("native-comp-function-p", &args, 1)?;
    Ok(Value::NIL)
}

/// `(interpreted-function-p OBJECT)` -- return t if OBJECT is an interpreted
/// function (a Lambda that is NOT byte-compiled).
///
/// In our VM, any `Value::Lambda` is interpreted (as opposed to
/// `Value::ByteCode`).
pub(crate) fn builtin_interpreted_function_p(args: Vec<Value>) -> EvalResult {
    expect_args("interpreted-function-p", &args, 1)?;
    Ok(Value::bool_val(args[0].is_lambda()))
}

/// `(special-form-p OBJECT)` -- return t if OBJECT is a special form.
///
/// GNU Emacs (eval.c): checks if OBJECT is a symbol whose function cell
/// contains a subr with max_args == UNEVALLED.  NeoVM checks the symbol
/// name against the evaluator's special-form table.
pub(crate) fn builtin_special_form_p(args: Vec<Value>) -> EvalResult {
    expect_args("special-form-p", &args, 1)?;
    let result = match args[0].kind() {
        ValueKind::Symbol(id) => is_public_special_form_name(resolve_sym(id)),
        ValueKind::Subr(_) | ValueKind::Veclike(VecLikeType::Subr) => subr_dispatch_kind_from_value(&args[0])
            .is_some_and(|kind| kind == SubrDispatchKind::SpecialForm),
        _ => false,
    };
    Ok(Value::bool_val(result))
}

pub(crate) fn subr_dispatch_kind_from_value(value: &Value) -> Option<SubrDispatchKind> {
    // New path: look up from global table
    if let Some(sym_id) = value.as_subr_id() {
        if let Some(entry) = super::eval::lookup_global_subr_entry(sym_id) {
            return Some(entry.dispatch_kind);
        }
    }
    // Old heap path fallback
    if !matches!(value.kind(), ValueKind::Veclike(VecLikeType::Subr)) {
        return None;
    }
    let ptr = value.as_veclike_ptr()? as *const SubrObj;
    Some(unsafe { (*ptr).dispatch_kind })
}

pub(crate) fn subr_is_callable_function_value(value: &Value) -> bool {
    subr_dispatch_kind_from_value(value).is_some_and(|kind| kind != SubrDispatchKind::SpecialForm)
}

/// Check if a single value is a macro.  Shared by `builtin_macrop` and tests.
pub(crate) fn macrop_check(obj: &Value) -> EvalResult {
    if let Some(marker) = autoload_macro_marker(obj) {
        return Ok(marker);
    }
    Ok(Value::bool_val(is_macro_object(obj)))
}

/// `(commandp FUNCTION &optional FOR-CALL-INTERACTIVELY)` -- return t if
/// FUNCTION is an interactive command.
///
/// In our simplified VM, any callable value (lambda, subr, bytecode) is
/// treated as a potential command.  A more complete implementation would
/// check for an `interactive` declaration.
pub(crate) fn builtin_commandp(args: Vec<Value>) -> EvalResult {
    expect_min_args("commandp", &args, 1)?;
    expect_max_args("commandp", &args, 2)?;
    Ok(Value::bool_val(args[0].is_function()))
}

/// `(func-arity FUNCTION)` -- return (MIN . MAX) for any callable.
///
/// Works for lambdas (reads `LambdaParams`), byte-code (reads `params`),
/// and subrs (reads from the canonical heap subr object).
pub(crate) fn builtin_func_arity_ctx(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("func-arity", &args, 1)?;
    if super::autoload::is_autoload_value(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Lambda) => {
            let params = args[0].closure_params().unwrap();
            let min = params.min_arity();
            let max = params.max_arity();
            Ok(arity_cons(min, max))
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            let bc = args[0].get_bytecode_data().unwrap();
            let min = bc.params.min_arity();
            let max = bc.params.max_arity();
            Ok(arity_cons(min, max))
        }
        ValueKind::Subr(id) => Ok(subr_arity_from_registry(ctx, id)),
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = args[0].as_subr_id().unwrap();
            Ok(subr_arity_from_registry(ctx, id))
        }
        ValueKind::Veclike(VecLikeType::Macro) => {
            let params = args[0].closure_params().unwrap();
            let min = params.min_arity();
            let max = params.max_arity();
            Ok(arity_cons(min, max))
        }
        _other => Err(signal("invalid-function", vec![args[0]])),
    }
}

/// Legacy pure version for callers that don't have Context access.
pub(crate) fn builtin_func_arity_impl(args: Vec<Value>) -> EvalResult {
    expect_args("func-arity", &args, 1)?;
    if super::autoload::is_autoload_value(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Lambda) => {
            let params = args[0].closure_params().unwrap();
            Ok(arity_cons(params.min_arity(), params.max_arity()))
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            let bc = args[0].get_bytecode_data().unwrap();
            Ok(arity_cons(bc.params.min_arity(), bc.params.max_arity()))
        }
        ValueKind::Subr(id) => {
            Ok(subr_arity_from_value(args[0]).unwrap_or_else(|| subr_arity_value(resolve_sym(id))))
        }
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = args[0].as_subr_id().unwrap();
            Ok(subr_arity_from_value(args[0]).unwrap_or_else(|| subr_arity_value(resolve_sym(id))))
        }
        ValueKind::Veclike(VecLikeType::Macro) => {
            let params = args[0].closure_params().unwrap();
            Ok(arity_cons(params.min_arity(), params.max_arity()))
        }
        _other => Err(signal("invalid-function", vec![args[0]])),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "subr_info_test.rs"]
mod tests;
