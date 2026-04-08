//! Customization and buffer-local variable system.
//!
//! GNU Lisp owns `defcustom`, `defgroup`, `setq-default`, and `custom-*`.
//! The live Rust-side responsibility here is the buffer-local/default-value
//! machinery that the evaluator still needs directly.

use super::error::{EvalResult, Flow, signal};
use super::intern::{SymId, intern, resolve_sym};
use super::value::*;
use crate::gc_trace::GcTrace;

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
    let name = match args[0].kind() {
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        ValueKind::Nil => "nil".to_string(),
        ValueKind::T => "t".to_string(),
        _other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };
    let resolved_id =
        super::builtins::resolve_variable_alias_id_in_obarray(obarray, intern(&name))?;
    let resolved = resolve_sym(resolved_id).to_string();
    if obarray.is_constant(&resolved) {
        return Err(signal("setting-constant", vec![Value::symbol(name)]));
    }
    if !obarray.boundp(&resolved) {
        obarray.set_symbol_value(&resolved, Value::NIL);
    }
    // Phase 6 of the symbol-redirect refactor: flip the new redirect
    // tag to LOCALIZED and set local_if_set on the BLV. The legacy
    // BufferLocal SymbolValue marker and the CustomManager
    // auto_buffer_local set stay in sync until Phase 10 deletes
    // them. Mirrors GNU Fmake_variable_buffer_local
    // (data.c:2142-2207).
    let default_value = obarray
        .find_symbol_value(resolved_id)
        .unwrap_or(Value::NIL);
    obarray.make_symbol_localized(resolved_id, default_value);
    obarray.set_blv_local_if_set(resolved_id, true);
    obarray.make_buffer_local(&resolved, true);
    custom.make_variable_buffer_local(&resolved);
    Ok(args[0])
}

/// `(make-local-variable VARIABLE)` -- make variable local in current buffer.
///
/// Mirrors GNU `Fmake_local_variable` (`data.c:2209-2312`). Differs
/// from `make-variable-buffer-local` in that it creates a per-buffer
/// binding *only* in the current buffer, without setting
/// `local_if_set` (which would auto-create on every subsequent
/// `setq` in any buffer).
pub(crate) fn builtin_make_local_variable(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("make-local-variable", &args, 1)?;
    let name = match args[0].kind() {
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        ValueKind::Nil => "nil".to_string(),
        ValueKind::T => "t".to_string(),
        _other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };
    let symbol = intern(&name);
    let resolved = super::builtins::resolve_variable_alias_id_in_obarray(&ctx.obarray, symbol)?;
    let resolved_name = resolve_sym(resolved);
    if ctx.obarray.is_constant_id(resolved) {
        return Err(signal("setting-constant", vec![Value::symbol(name)]));
    }

    // Phase 6 of the symbol-redirect refactor: flip the symbol to
    // LOCALIZED (preserving its current value as the default) and
    // seed the current buffer's local_var_alist with `(sym . default)`
    // if it doesn't already have an entry. This is the new GNU-shape
    // path. The legacy BufferLocals storage stays populated below
    // until Phase 10 deletes it.
    let default_value = ctx
        .obarray
        .find_symbol_value(resolved)
        .unwrap_or(Value::NIL);
    ctx.obarray.make_symbol_localized(resolved, default_value);
    if let Some(current_id) = ctx.buffers.current_buffer_id() {
        if let Some(buf) = ctx.buffers.get_mut(current_id) {
            // Only seed when no entry exists yet (idempotent — calling
            // make-local-variable twice doesn't double-prepend).
            let key = Value::from_sym_id(resolved);
            let mut cursor = buf.local_var_alist;
            let mut found = false;
            while cursor.is_cons() {
                let entry = cursor.cons_car();
                if entry.is_cons() && super::value::eq_value(&entry.cons_car(), &key) {
                    found = true;
                    break;
                }
                cursor = cursor.cons_cdr();
            }
            if !found {
                let cell = Value::cons(key, default_value);
                buf.local_var_alist = Value::cons(cell, buf.local_var_alist);
            }
        }
    }

    // Legacy path: keep BufferLocals populated until Phase 10.
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
        return RuntimeBindingValue::Bound(Value::NIL);
    }
    if super::builtins::is_canonical_symbol_id(resolved) && resolved_name == "t" {
        return RuntimeBindingValue::Bound(Value::T);
    }
    if super::builtins::is_canonical_symbol_id(resolved) && resolved_name.starts_with(':') {
        return RuntimeBindingValue::Bound(Value::keyword_id(resolved));
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
    let sym_id = match args[0].kind() {
        ValueKind::Symbol(id) => id,
        ValueKind::Nil => intern("nil"),
        ValueKind::T => intern("t"),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };
    let resolved_id = super::builtins::resolve_variable_alias_id_in_obarray(&ctx.obarray, sym_id)?;
    let resolved_name = resolve_sym(resolved_id);

    let buf = if args.len() > 1 {
        match args[1].kind() {
            ValueKind::Nil => ctx.buffers.current_buffer(),
            ValueKind::Veclike(VecLikeType::Buffer) => {
                ctx.buffers.get(args[1].as_buffer_id().unwrap())
            }
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("bufferp"), args[1]],
                ));
            }
        }
    } else {
        ctx.buffers.current_buffer()
    };

    let Some(b) = buf else {
        return Ok(Value::NIL);
    };

    // Phase 10E: route LOCALIZED checks through the BLV machinery.
    // Mirrors GNU `Flocal_variable_p` SYMBOL_LOCALIZED arm at
    // `data.c:2399-2412`: walk the buffer's local_var_alist (or
    // trust the BLV cache if `where == buf`).
    use crate::emacs_core::symbol::SymbolRedirect;
    if let Some(sym_slot) = ctx.obarray.get_by_id(resolved_id)
        && sym_slot.redirect() == SymbolRedirect::Localized
    {
        let target_buf = Value::make_buffer(b.id);
        return Ok(Value::bool_val(ctx.obarray.has_per_buffer_binding(
            resolved_id,
            target_buf,
            b.local_var_alist,
        )));
    }

    Ok(Value::bool_val(b.has_buffer_local(resolved_name)))
}

/// `(buffer-local-variables &optional BUFFER)` -- list all local variables.
pub(crate) fn builtin_buffer_local_variables(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-local-variables", &args, 1)?;

    let id = match args.first() {
        None => ctx
            .buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?,
        Some(v) if v.is_nil() => ctx
            .buffers
            .current_buffer()
            .map(|b| b.id)
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?,
        Some(v) if v.is_buffer() => v.as_buffer_id().unwrap(),
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
            ctx.run_variable_watchers_by_id_with_where(
                outcome.resolved_id,
                &Value::NIL,
                &Value::NIL,
                "makunbound",
                &Value::make_buffer(buffer_id),
            )?;
        }
    }
    Ok(outcome.result)
}

pub(crate) struct KillLocalVariableOutcome {
    pub result: Value,
    pub removed: bool,
    pub resolved_id: SymId,
    pub buffer_id: Option<crate::buffer::BufferId>,
}

pub(crate) fn builtin_kill_local_variable_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: &[Value],
) -> Result<KillLocalVariableOutcome, Flow> {
    expect_args("kill-local-variable", &args, 1)?;
    let symbol = match args[0].kind() {
        ValueKind::Symbol(id) => id,
        ValueKind::Nil => intern("nil"),
        ValueKind::T => intern("t"),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };

    let resolved = super::builtins::resolve_variable_alias_id_in_obarray(&ctx.obarray, symbol)?;
    let resolved_name = resolve_sym(resolved);
    let mut removed = false;
    let buffer_id = ctx.buffers.current_buffer_id();
    if let Some(buffer_id) = buffer_id {
        removed = ctx
            .buffers
            .remove_buffer_local_property(buffer_id, resolved_name)
            .flatten()
            .is_some();
    }

    Ok(KillLocalVariableOutcome {
        result: args[0],
        removed,
        resolved_id: resolved,
        buffer_id,
    })
}

/// `(default-value SYMBOL)` -- get the default (global) value of a variable.
pub(crate) fn builtin_default_value(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("default-value", &args, 1)?;
    let symbol = match args[0].kind() {
        ValueKind::Nil => intern("nil"),
        ValueKind::T => intern("t"),
        ValueKind::Symbol(id) => id,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };
    let resolved = super::builtins::resolve_variable_alias_id_in_obarray(&eval.obarray, symbol)?;
    let resolved_name = resolve_sym(resolved);

    // Phase 10D: FORWARDED BUFFER_OBJFWD reads consult
    // `BufferManager::buffer_defaults` (the live default), not the
    // legacy `symbol_value_id` reader which returns None for
    // FORWARDED. Mirrors GNU `Fdefault_value` (`data.c:1834-1846`)
    // dispatching through `do_default_value` for SYMBOL_FORWARDED.
    {
        use crate::emacs_core::forward::{LispBufferObjFwd, LispFwdType};
        use crate::emacs_core::symbol::SymbolRedirect;
        if let Some(sym) = eval.obarray().get_by_id(resolved)
            && sym.redirect() == SymbolRedirect::Forwarded
        {
            let fwd = unsafe { &*sym.val.fwd };
            if matches!(fwd.ty, LispFwdType::BufferObj) {
                let buf_fwd = unsafe { &*(fwd as *const _ as *const LispBufferObjFwd) };
                let off = buf_fwd.offset as usize;
                if off < eval.buffers.buffer_defaults.len() {
                    return Ok(eval.buffers.buffer_defaults[off]);
                }
                return Ok(buf_fwd.default);
            }
        }
    }

    // specbind writes directly to obarray, so no dynamic stack lookup needed.
    match eval.obarray.symbol_value_id(resolved) {
        Some(v) => Ok(*v),
        None if super::builtins::is_canonical_symbol_id(resolved)
            && resolved_name.starts_with(':') =>
        {
            Ok(Value::from_kw_id(resolved))
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
    let symbol = match args[0].kind() {
        ValueKind::Nil => intern("nil"),
        ValueKind::T => intern("t"),
        ValueKind::Symbol(id) => id,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };
    let resolved = super::builtins::resolve_variable_alias_id(eval, symbol)?;
    if eval.obarray().is_constant_id(resolved) {
        return Err(signal("setting-constant", vec![args[0]]));
    }
    let value = args[1];

    // Phase 10D: route FORWARDED BUFFER_OBJFWD writes through
    // `BufferManager::set_buffer_default_slot`, which updates
    // `buffer_defaults` AND propagates to every live buffer whose
    // local_flags bit is clear. Mirrors GNU `set_default_internal`
    // SYMBOL_FORWARDED arm (`data.c:2044-2078`).
    let forwarded_slot = {
        use crate::emacs_core::forward::{LispBufferObjFwd, LispFwdType};
        use crate::emacs_core::symbol::SymbolRedirect;
        eval.obarray()
            .get_by_id(resolved)
            .filter(|sym| sym.redirect() == SymbolRedirect::Forwarded)
            .and_then(|sym| {
                let fwd = unsafe { &*sym.val.fwd };
                if matches!(fwd.ty, LispFwdType::BufferObj) {
                    let buf_fwd = unsafe { &*(fwd as *const _ as *const LispBufferObjFwd) };
                    let info_name = crate::emacs_core::intern::resolve_sym(resolved);
                    crate::buffer::buffer::lookup_buffer_slot(info_name)
                        .map(|info| (info, buf_fwd.offset))
                } else {
                    None
                }
            })
    };
    if let Some((info, _off)) = forwarded_slot {
        eval.buffers.set_buffer_default_slot(info, value);
    } else if !crate::emacs_core::eval::set_default_toplevel_value_in_state(
        eval.specpdl.as_mut_slice(),
        resolved,
        value,
    ) {
        eval.obarray_mut().set_symbol_value_id(resolved, value);
    }

    // Fire watchers AFTER the write with operation="set".
    // When the symbol was resolved through an alias, fire watchers twice
    // (matching GNU where both set_default_internal and set_internal notify).
    eval.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "set")?;
    if resolved != symbol {
        eval.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "set")?;
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "custom_test.rs"]
mod tests;
