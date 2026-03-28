//! Customization and buffer-local variable system.
//!
//! GNU Lisp owns `defcustom`, `defgroup`, `setq-default`, and `custom-*`.
//! The live Rust-side responsibility here is the buffer-local/default-value
//! machinery that the evaluator still needs directly.

use super::error::{EvalResult, Flow, signal};
use super::intern::{SymId, intern, resolve_sym};
use super::value::*;
use crate::gc::GcTrace;

/// Rust-side registry for automatic buffer-local declarations.
#[derive(Clone, Debug, Default)]
pub struct CustomManager {
    /// Set of variable names marked as automatically buffer-local.
    pub auto_buffer_local: std::collections::HashSet<String>,
}

impl CustomManager {
    pub fn new() -> Self {
        Self {
            auto_buffer_local: std::collections::HashSet::new(),
        }
    }

    /// Mark a variable as automatically buffer-local.
    pub fn make_variable_buffer_local(&mut self, name: &str) {
        self.auto_buffer_local.insert(name.to_string());
    }

    /// Check if a variable is automatically buffer-local.
    pub fn is_auto_buffer_local(&self, name: &str) -> bool {
        self.auto_buffer_local.contains(name)
    }
}

impl GcTrace for CustomManager {
    fn trace_roots(&self, _roots: &mut Vec<Value>) {}
}

// ---------------------------------------------------------------------------
// Pure builtins (no evaluator needed)
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

/// `(make-variable-buffer-local VARIABLE)` -- mark variable as automatically buffer-local.
pub(crate) fn builtin_make_variable_buffer_local(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let (obarray, custom) = (&mut eval.obarray, &mut eval.custom);
    builtin_make_variable_buffer_local_with_state(obarray, custom, args)
}

pub(crate) fn builtin_make_variable_buffer_local_with_state(
    obarray: &mut crate::emacs_core::symbol::Obarray,
    custom: &mut CustomManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("make-variable-buffer-local", &args, 1)?;
    let name = match &args[0] {
        Value::Symbol(id) | Value::Keyword(id) => resolve_sym(*id).to_owned(),
        Value::Nil => "nil".to_string(),
        Value::True => "t".to_string(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            ));
        }
    };
    let resolved = resolve_sym(super::builtins::resolve_variable_alias_id_in_obarray(
        obarray,
        intern(&name),
    )?)
    .to_string();
    if obarray.is_constant(&resolved) {
        return Err(signal("setting-constant", vec![Value::symbol(name)]));
    }
    if !obarray.boundp(&resolved) {
        obarray.set_symbol_value(&resolved, Value::Nil);
    }
    // Primary mechanism: mark in the obarray's SymbolValue enum.
    obarray.make_buffer_local(&resolved, true);
    // Keep CustomManager in sync during the transition period.
    custom.make_variable_buffer_local(&resolved);
    Ok(args[0])
}

/// `(make-local-variable VARIABLE)` -- make variable local in current buffer.
pub(crate) fn builtin_make_local_variable(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("make-local-variable", &args, 1)?;
    let name = match &args[0] {
        Value::Symbol(id) | Value::Keyword(id) => resolve_sym(*id).to_owned(),
        Value::Nil => "nil".to_string(),
        Value::True => "t".to_string(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            ));
        }
    };
    let symbol = intern(&name);
    let resolved = super::builtins::resolve_variable_alias_id_in_obarray(&ctx.obarray, symbol)?;
    let resolved_name = resolve_sym(resolved);
    if ctx.obarray.is_constant_id(resolved) {
        return Err(signal("setting-constant", vec![Value::symbol(name)]));
    }

    if let Some(current_id) = ctx.buffers.current_buffer_id() {
        if ctx
            .buffers
            .get(current_id)
            .is_some_and(|buf| !buf.has_buffer_local(resolved_name))
        {
            match runtime_binding_for_make_local_variable(&ctx.obarray, &[], symbol, resolved) {
                RuntimeBindingValue::Bound(value) => {
                    let _ = ctx
                        .buffers
                        .set_buffer_local_property(current_id, resolved_name, value);
                }
                RuntimeBindingValue::Void => {
                    let _ = ctx
                        .buffers
                        .set_buffer_local_void_property(current_id, resolved_name);
                }
            }
        }
    }
    Ok(args[0])
}

fn runtime_binding_for_make_local_variable(
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    symbol: SymId,
    resolved: SymId,
) -> RuntimeBindingValue {
    // specbind writes directly to obarray, so no dynamic stack lookup needed.
    if let Some(value) = obarray.symbol_value_id(resolved) {
        return RuntimeBindingValue::Bound(*value);
    }

    let resolved_name = resolve_sym(resolved);
    if super::builtins::is_canonical_symbol_id(resolved) && resolved_name == "nil" {
        return RuntimeBindingValue::Bound(Value::Nil);
    }
    if super::builtins::is_canonical_symbol_id(resolved) && resolved_name == "t" {
        return RuntimeBindingValue::Bound(Value::True);
    }
    if super::builtins::is_canonical_symbol_id(resolved) && resolved_name.starts_with(':') {
        return RuntimeBindingValue::Bound(Value::Keyword(resolved));
    }

    RuntimeBindingValue::Void
}

/// `(local-variable-p VARIABLE &optional BUFFER)` -- test if variable is local.
pub(crate) fn builtin_local_variable_p(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("local-variable-p", &args, 1)?;
    expect_max_args("local-variable-p", &args, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = super::builtins::resolve_variable_alias_name_in_obarray(&ctx.obarray, name)?;

    let buf = if args.len() > 1 {
        match &args[1] {
            Value::Nil => ctx.buffers.current_buffer(),
            Value::Buffer(id) => ctx.buffers.get(*id),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("bufferp"), *other],
                ));
            }
        }
    } else {
        ctx.buffers.current_buffer()
    };

    match buf {
        Some(b) => Ok(Value::bool(b.has_buffer_local(&resolved))),
        None => Ok(Value::Nil),
    }
}

/// `(buffer-local-variables &optional BUFFER)` -- list all local variables.
pub(crate) fn builtin_buffer_local_variables(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-local-variables", &args, 1)?;

    let id = match args.first() {
        None | Some(Value::Nil) => ctx
            .buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?,
        Some(Value::Buffer(id)) => *id,
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("bufferp"), *other],
            ));
        }
    };

    let buf = ctx
        .buffers
        .get(id)
        .ok_or_else(|| signal("error", vec![Value::string("No such live buffer")]))?;

    let locals: Vec<(String, Option<Value>)> = buf
        .ordered_buffer_local_bindings()
        .into_iter()
        .map(|(name, value)| (name, value.as_value()))
        .collect();

    let entries: Vec<Value> = locals
        .into_iter()
        .map(|(name, value)| match value {
            Some(value) => Value::cons(Value::symbol(name), value),
            None => Value::symbol(name),
        })
        .collect();
    Ok(Value::list(entries))
}

/// `(kill-local-variable VARIABLE)` -- remove local binding in current buffer.
pub(crate) fn builtin_kill_local_variable(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let outcome = builtin_kill_local_variable_impl(ctx, &args)?;
    if outcome.removed {
        if let Some(buffer_id) = outcome.buffer_id {
            ctx.run_variable_watchers_with_where(
                &outcome.resolved_name,
                &Value::Nil,
                &Value::Nil,
                "makunbound",
                &Value::Buffer(buffer_id),
            )?;
        }
    }
    Ok(outcome.result)
}

pub(crate) struct KillLocalVariableOutcome {
    pub result: Value,
    pub removed: bool,
    pub resolved_name: String,
    pub buffer_id: Option<crate::buffer::BufferId>,
}

pub(crate) fn builtin_kill_local_variable_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: &[Value],
) -> Result<KillLocalVariableOutcome, Flow> {
    expect_args("kill-local-variable", &args, 1)?;
    let name = match &args[0] {
        Value::Symbol(id) | Value::Keyword(id) => resolve_sym(*id).to_owned(),
        Value::Nil => "nil".to_string(),
        Value::True => "t".to_string(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            ));
        }
    };

    let resolved = super::builtins::resolve_variable_alias_name_in_obarray(&ctx.obarray, &name)?;
    let mut removed = false;
    let buffer_id = ctx.buffers.current_buffer_id();
    if let Some(buffer_id) = buffer_id {
        removed = ctx
            .buffers
            .remove_buffer_local_property(buffer_id, &resolved)
            .flatten()
            .is_some();
    }

    Ok(KillLocalVariableOutcome {
        result: args[0],
        removed,
        resolved_name: resolved,
        buffer_id,
    })
}

/// `(default-value SYMBOL)` -- get the default (global) value of a variable.
pub(crate) fn builtin_default_value(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("default-value", &args, 1)?;
    let symbol = match args[0] {
        Value::Nil => intern("nil"),
        Value::True => intern("t"),
        Value::Symbol(id) | Value::Keyword(id) => id,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };
    let resolved = super::builtins::resolve_variable_alias_id_in_obarray(&eval.obarray, symbol)?;
    let resolved_name = resolve_sym(resolved);
    // specbind writes directly to obarray, so no dynamic stack lookup needed.
    match eval.obarray.symbol_value_id(resolved) {
        Some(v) => Ok(*v),
        None if super::builtins::is_canonical_symbol_id(resolved)
            && resolved_name.starts_with(':') =>
        {
            Ok(Value::Keyword(resolved))
        }
        None => Err(signal("void-variable", vec![args[0]])),
    }
}

/// `(set-default SYMBOL VALUE)` -- set the default (global) value.
///
/// GNU design for PLAINVAL (non-buffer-local) variables: `set-default`
/// delegates to `set_internal`, which writes to the dynamic frame when
/// let-bound, so the let-bound value is updated.  After the let unwinds,
/// the obarray value (saved "old" default) is restored.
///
/// For buffer-local variables, `set-default` writes to the obarray
/// (default cell) directly, not to the dynamic frame or buffer-local slot.
pub(crate) fn builtin_set_default(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("set-default", &args, 2)?;
    let symbol = match args[0] {
        Value::Nil => intern("nil"),
        Value::True => intern("t"),
        Value::Symbol(id) | Value::Keyword(id) => id,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };
    let resolved = super::builtins::resolve_variable_alias_id(eval, symbol)?;
    let resolved_name = resolve_sym(resolved);
    if eval.obarray().is_constant_id(resolved) {
        return Err(signal("setting-constant", vec![args[0]]));
    }
    let value = args[1];

    // GNU PLAINVAL path: for non-buffer-local variables, `set-default`
    // behaves like `set` -- writes to the dynamic frame if let-bound.
    let is_buffer_local = eval.obarray().is_buffer_local(resolved_name)
        || eval.custom.is_auto_buffer_local(resolved_name);
    if !is_buffer_local {
        // PLAINVAL: delegate to set_runtime_binding which writes to the
        // dynamic frame if the variable is let-bound, else to the obarray.
        eval.set_runtime_binding_by_id(resolved, value);
    } else {
        // LOCALIZED: write to the obarray (default cell) directly.
        eval.obarray_mut().set_symbol_value_id(resolved, value);
    }

    // Fire watchers AFTER the write with operation="set".
    // When the symbol was resolved through an alias, fire watchers twice
    // (matching GNU where both set_default_internal and set_internal notify).
    eval.run_variable_watchers(resolved_name, &value, &Value::Nil, "set")?;
    if resolved != symbol {
        eval.run_variable_watchers(resolved_name, &value, &Value::Nil, "set")?;
    }
    Ok(value)
}

/// `(defvar-local NAME VALUE &optional DOCSTRING)`
///
/// Like `defvar`: only sets the variable if it is not already bound, marks it
/// as special (dynamically scoped).  Additionally marks the variable as
/// automatically buffer-local via the [`CustomManager`].  Returns the symbol
/// name.
pub(crate) fn sf_defvar_local(
    eval: &mut super::eval::Context,
    tail: &[super::expr::Expr],
) -> super::error::EvalResult {
    use super::eval::quote_to_value;
    use super::expr::Expr;

    if tail.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("defvar-local"), Value::Int(tail.len() as i64)],
        ));
    }

    // 1. Extract the symbol name (unevaluated).
    let name = match &tail[0] {
        Expr::Symbol(id) => resolve_sym(*id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), quote_to_value(other)],
            ));
        }
    };

    // 2. Evaluate the default value expression.
    let default_value = if tail.len() > 1 {
        eval.eval(&tail[1])?
    } else {
        Value::Nil
    };

    // 3. Optional docstring (ignored for now, but consumed so we don't error).
    // (defvar-local NAME VALUE "docstring") — third element may be a string.

    // 4. Like defvar: only set if not already bound.
    if !eval.obarray().boundp(&name) {
        eval.obarray_mut().set_symbol_value(&name, default_value);
    }

    // 5. Mark as special (dynamically scoped).
    eval.obarray_mut().make_special(&name);

    // 6. Mark as automatically buffer-local (primary: obarray enum).
    eval.obarray_mut().make_buffer_local(&name, true);
    // Keep CustomManager in sync during the transition period.
    eval.custom.make_variable_buffer_local(&name);

    Ok(Value::symbol(name))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "custom_test.rs"]
mod tests;
