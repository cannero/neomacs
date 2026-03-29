//! Subr/primitive introspection builtins.
//!
//! Provides type predicates and introspection for callable objects:
//! - `subrp`, `subr-name`, `subr-arity`
//! - `commandp`, `functionp`, `byte-code-function-p`, `closurep`
//! - `interpreted-function-p`, `special-form-p`, `macrop`
//! - `func-arity`, `indirect-function`

use super::error::{EvalResult, Flow, signal};
use super::intern::{SymId, lookup_interned, resolve_sym};
use super::value::*;

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

// ---------------------------------------------------------------------------
// Arity helpers
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Arity helpers
// ---------------------------------------------------------------------------

/// Build a cons cell `(MIN . MAX)` representing arity.
/// `max` of `None` means "many" (unbounded &rest), represented by the
/// symbol `many`.
fn arity_cons(min: usize, max: Option<usize>) -> Value {
    let min_val = Value::Int(min as i64);
    let max_val = match max {
        Some(n) => Value::Int(n as i64),
        None => Value::symbol("many"),
    };
    Value::cons(min_val, max_val)
}

fn arity_unevalled(min: usize) -> Value {
    Value::cons(Value::Int(min as i64), Value::symbol("unevalled"))
}

fn is_cxr_subr_name(name: &str) -> bool {
    let Some(inner) = name.strip_prefix('c').and_then(|s| s.strip_suffix('r')) else {
        return false;
    };
    !inner.is_empty() && inner.chars().all(|ch| ch == 'a' || ch == 'd')
}

fn subr_arity_value(_name: &str) -> Value {
    // Fallback for builtins whose registration still uses (0, None).
    // All builtins with known arities now have correct registration,
    // so this should only be reached for legitimately variadic builtins.
    arity_cons(0, None)
}

pub(crate) fn dispatch_subr_arity_value(name: &str) -> Value {
    subr_arity_value(name)
}

fn is_macro_object(value: &Value) -> bool {
    match value {
        Value::Macro(_) => true,
        Value::Cons(cell) => read_cons(*cell).car.as_symbol_name() == Some("macro"),
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
        Some(Value::list(vec![Value::symbol("macro"), Value::True]))
    } else if matches!(autoload_type, Value::True) {
        // GNU Emacs uses `t` as a legacy macro marker for some startup
        // autoloads (notably `pcase-dolist`), and `macrop` returns `(t)`.
        Some(Value::list(vec![Value::True]))
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
    match &args[0] {
        Value::Subr(id) => Ok(Value::string(resolve_sym(*id))),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("subrp"), *other],
        )),
    }
}

/// `(subr-arity SUBR)` -- return (MIN . MAX) cons cell for argument counts.
///
/// Reads arity from the SubrObject registration (single source of truth).
/// Falls back to the hardcoded table for builtins not yet updated.
pub(crate) fn builtin_subr_arity(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("subr-arity", &args, 1)?;
    match &args[0] {
        Value::Subr(id) => Ok(subr_arity_from_registry(ctx, *id)),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("subrp"), *other],
        )),
    }
}

/// Look up arity from SubrObject registration first, fall back to hardcoded table.
fn subr_arity_from_registry(ctx: &super::eval::Context, sym_id: SymId) -> Value {
    if let Some(subr) = ctx.subr_slot(sym_id) {
        // If registration has actual arity (not the default 0/None),
        // use it as the authoritative source.
        let min = subr.min_args;
        let max = subr.max_args;
        if min > 0 || max.is_some() {
            return arity_cons(min as usize, max.map(|m| m as usize));
        }
    }
    // Fall back to hardcoded table for builtins still using (0, None)
    subr_arity_value(resolve_sym(sym_id))
}

/// `(native-comp-function-p OBJECT)` -- return t if OBJECT is a native-compiled
/// function object.
///
/// NeoVM does not currently model native-compiled function objects, so this
/// always returns nil.
pub(crate) fn builtin_native_comp_function_p(args: Vec<Value>) -> EvalResult {
    expect_args("native-comp-function-p", &args, 1)?;
    Ok(Value::Nil)
}

/// `(subr-primitive-p OBJECT)` -- return t if OBJECT is a primitive subr.
pub(crate) fn builtin_subr_primitive_p(args: Vec<Value>) -> EvalResult {
    expect_args("subr-primitive-p", &args, 1)?;
    Ok(Value::bool(matches!(&args[0], Value::Subr(_))))
}

/// `(interpreted-function-p OBJECT)` -- return t if OBJECT is an interpreted
/// function (a Lambda that is NOT byte-compiled).
///
/// In our VM, any `Value::Lambda` is interpreted (as opposed to
/// `Value::ByteCode`).
pub(crate) fn builtin_interpreted_function_p(args: Vec<Value>) -> EvalResult {
    expect_args("interpreted-function-p", &args, 1)?;
    Ok(Value::bool(matches!(&args[0], Value::Lambda(_))))
}

/// `(special-form-p OBJECT)` -- return t if OBJECT is a symbol that names a
/// special form.
///
/// Accepts a symbol (including nil/t) and checks it against the evaluator's
/// special-form table.
pub(crate) fn builtin_special_form_p(args: Vec<Value>) -> EvalResult {
    expect_args("special-form-p", &args, 1)?;
    let result = match &args[0] {
        Value::Symbol(id) => {
            let name = resolve_sym(*id);
            lookup_interned(name).is_some_and(|canonical| canonical == *id)
                && is_public_special_form_name(name)
        }
        Value::Subr(id) => is_public_special_form_name(resolve_sym(*id)),
        _ => false,
    };
    Ok(Value::bool(result))
}

/// Check if a single value is a macro.  Shared by `builtin_macrop` and tests.
pub(crate) fn macrop_check(obj: &Value) -> EvalResult {
    if let Some(marker) = autoload_macro_marker(obj) {
        return Ok(marker);
    }
    Ok(Value::bool(is_macro_object(obj)))
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
    Ok(Value::bool(args[0].is_function()))
}

/// `(func-arity FUNCTION)` -- return (MIN . MAX) for any callable.
///
/// Works for lambdas (reads `LambdaParams`), byte-code (reads `params`),
/// and subrs (reads from SubrObject registration).
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
    match &args[0] {
        Value::Lambda(_) => {
            let ld = args[0].get_lambda_data().unwrap();
            let min = ld.params.min_arity();
            let max = ld.params.max_arity();
            Ok(arity_cons(min, max))
        }
        Value::ByteCode(_) => {
            let bc = args[0].get_bytecode_data().unwrap();
            let min = bc.params.min_arity();
            let max = bc.params.max_arity();
            Ok(arity_cons(min, max))
        }
        Value::Subr(id) => Ok(subr_arity_from_registry(ctx, *id)),
        Value::Macro(_) => {
            let ld = args[0].get_lambda_data().unwrap();
            let min = ld.params.min_arity();
            let max = ld.params.max_arity();
            Ok(arity_cons(min, max))
        }
        other => Err(signal("invalid-function", vec![*other])),
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
    match &args[0] {
        Value::Lambda(_) => {
            let ld = args[0].get_lambda_data().unwrap();
            Ok(arity_cons(ld.params.min_arity(), ld.params.max_arity()))
        }
        Value::ByteCode(_) => {
            let bc = args[0].get_bytecode_data().unwrap();
            Ok(arity_cons(bc.params.min_arity(), bc.params.max_arity()))
        }
        Value::Subr(id) => Ok(subr_arity_value(resolve_sym(*id))),
        Value::Macro(_) => {
            let ld = args[0].get_lambda_data().unwrap();
            Ok(arity_cons(ld.params.min_arity(), ld.params.max_arity()))
        }
        other => Err(signal("invalid-function", vec![*other])),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "subr_info_test.rs"]
mod tests;
