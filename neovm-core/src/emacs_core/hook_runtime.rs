use super::builtins;
use super::error::{EvalResult, Flow, signal};
use super::eval::Context;
use super::intern::{SymId, intern};
use super::symbol::Obarray;
use super::value::*;
use crate::emacs_core::value::{ValueKind};

pub(crate) trait HookRuntime {
    fn hook_context(&self) -> &Context;
    fn call_hook_callable(&mut self, function: Value, args: &[Value]) -> EvalResult;
}

impl HookRuntime for Context {
    fn hook_context(&self) -> &Context {
        self
    }

    fn call_hook_callable(&mut self, function: Value, args: &[Value]) -> EvalResult {
        self.apply(function, args.to_vec())
    }
}

pub(crate) fn resolve_hook_symbol(ctx: &Context, hook_symbol: Value) -> Result<SymId, Flow> {
    let symbol = builtins::expect_symbol_id(&hook_symbol)?;
    builtins::resolve_variable_alias_id_in_obarray(&ctx.obarray, symbol)
}

pub(crate) fn hook_symbol_by_name(ctx: &Context, hook_name: &str) -> SymId {
    builtins::resolve_variable_alias_id_in_obarray(&ctx.obarray, intern(hook_name))
        .unwrap_or_else(|_| intern(hook_name))
}

pub(crate) fn hook_value_by_id(ctx: &Context, hook_sym: SymId) -> Option<Value> {
    ctx.visible_runtime_variable_value_by_id_resolved(hook_sym)
}

fn collect_hook_functions_impl(
    obarray: &Obarray,
    hook_sym: SymId,
    hook_value: Value,
    inherit_global: bool,
    out: &mut Vec<Value>,
) {
    match hook_value.kind() {
        ValueKind::Nil => {}
        ValueKind::Cons => {
            let mut cursor = hook_value;
            let mut saw_global_marker = false;
            while cursor.is_cons() {
                let pair_car = hook_value.cons_car();
                let pair_cdr = hook_value.cons_cdr();
                if pair_car.as_symbol_name() == Some("t") {
                    saw_global_marker = true;
                } else {
                    out.push(pair_car);
                }
                cursor = pair_cdr;
            }

            if saw_global_marker && inherit_global {
                let global_value = obarray
                    .default_value_id(hook_sym)
                    .copied()
                    .unwrap_or(Value::NIL);
                collect_hook_functions_impl(obarray, hook_sym, global_value, false, out);
            }
        }
        value => out.push(hook_value),
    }
}

pub(crate) fn collect_hook_functions_in_state(
    ctx: &Context,
    hook_sym: SymId,
    hook_value: Value,
    inherit_global: bool,
) -> Vec<Value> {
    let mut functions = Vec::new();
    collect_hook_functions_impl(
        &ctx.obarray,
        hook_sym,
        hook_value,
        inherit_global,
        &mut functions,
    );
    functions
}

pub(crate) fn run_hook_value<R: HookRuntime>(
    runtime: &mut R,
    hook_sym: SymId,
    hook_value: Value,
    hook_args: &[Value],
    inherit_global: bool,
) -> EvalResult {
    let funcs = collect_hook_functions_in_state(
        runtime.hook_context(),
        hook_sym,
        hook_value,
        inherit_global,
    );
    for func in funcs {
        let _ = runtime.call_hook_callable(func, hook_args)?;
    }
    Ok(Value::NIL)
}

pub(crate) fn safe_run_hook_value<R: HookRuntime>(
    runtime: &mut R,
    hook_sym: SymId,
    hook_value: Value,
    hook_args: &[Value],
    inherit_global: bool,
) -> EvalResult {
    match run_hook_value(runtime, hook_sym, hook_value, hook_args, inherit_global) {
        Ok(value) => Ok(value),
        Err(Flow::Signal(_)) => Ok(Value::NIL),
        Err(flow) => Err(flow),
    }
}

pub(crate) fn run_hook_value_until_success<R: HookRuntime>(
    runtime: &mut R,
    hook_sym: SymId,
    hook_value: Value,
    hook_args: &[Value],
    inherit_global: bool,
) -> EvalResult {
    let funcs = collect_hook_functions_in_state(
        runtime.hook_context(),
        hook_sym,
        hook_value,
        inherit_global,
    );
    for func in funcs {
        let value = runtime.call_hook_callable(func, hook_args)?;
        if value.is_truthy() {
            return Ok(value);
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn run_hook_value_until_failure<R: HookRuntime>(
    runtime: &mut R,
    hook_sym: SymId,
    hook_value: Value,
    hook_args: &[Value],
    inherit_global: bool,
) -> EvalResult {
    let funcs = collect_hook_functions_in_state(
        runtime.hook_context(),
        hook_sym,
        hook_value,
        inherit_global,
    );
    for func in funcs {
        let value = runtime.call_hook_callable(func, hook_args)?;
        if value.is_nil() {
            return Ok(Value::NIL);
        }
    }
    Ok(Value::T)
}

pub(crate) fn run_hook_value_wrapped<R: HookRuntime>(
    runtime: &mut R,
    hook_sym: SymId,
    hook_value: Value,
    wrapper: Value,
    wrapped_args: &[Value],
    inherit_global: bool,
) -> EvalResult {
    let funcs = collect_hook_functions_in_state(
        runtime.hook_context(),
        hook_sym,
        hook_value,
        inherit_global,
    );
    for func in funcs {
        let mut call_args = Vec::with_capacity(wrapped_args.len() + 1);
        call_args.push(func);
        call_args.extend(wrapped_args.iter().copied());
        let value = runtime.call_hook_callable(wrapper, &call_args)?;
        if value.is_truthy() {
            return Ok(value);
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn run_hook_query_error_with_timeout<R: HookRuntime>(
    runtime: &mut R,
    hook_sym: SymId,
    hook_value: Value,
) -> EvalResult {
    match run_hook_value(runtime, hook_sym, hook_value, &[], true) {
        Ok(value) => Ok(value),
        Err(Flow::Signal(_)) => Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        )),
        Err(flow) => Err(flow),
    }
}

pub(crate) fn run_named_hook<R: HookRuntime>(
    runtime: &mut R,
    hook_sym: SymId,
    hook_args: &[Value],
) -> EvalResult {
    let hook_value = hook_value_by_id(runtime.hook_context(), hook_sym).unwrap_or(Value::NIL);
    run_hook_value(runtime, hook_sym, hook_value, hook_args, true)
}

pub(crate) fn safe_run_named_hook<R: HookRuntime>(
    runtime: &mut R,
    hook_sym: SymId,
    hook_args: &[Value],
) -> EvalResult {
    let hook_value = hook_value_by_id(runtime.hook_context(), hook_sym).unwrap_or(Value::NIL);
    safe_run_hook_value(runtime, hook_sym, hook_value, hook_args, true)
}

pub(crate) fn run_named_hooks<R: HookRuntime>(
    runtime: &mut R,
    hook_symbols: &[Value],
) -> EvalResult {
    for hook_symbol in hook_symbols {
        let hook_sym = resolve_hook_symbol(runtime.hook_context(), *hook_symbol)?;
        let _ = run_named_hook(runtime, hook_sym, &[])?;
    }
    Ok(Value::NIL)
}

pub(crate) fn run_named_hook_with_args<R: HookRuntime>(
    runtime: &mut R,
    args: &[Value],
) -> EvalResult {
    let hook_sym = resolve_hook_symbol(runtime.hook_context(), args[0])?;
    let hook_value = hook_value_by_id(runtime.hook_context(), hook_sym).unwrap_or(Value::NIL);
    run_hook_value(runtime, hook_sym, hook_value, &args[1..], true)
}

pub(crate) fn run_named_hook_with_args_until_success<R: HookRuntime>(
    runtime: &mut R,
    args: &[Value],
) -> EvalResult {
    let hook_sym = resolve_hook_symbol(runtime.hook_context(), args[0])?;
    let hook_value = hook_value_by_id(runtime.hook_context(), hook_sym).unwrap_or(Value::NIL);
    run_hook_value_until_success(runtime, hook_sym, hook_value, &args[1..], true)
}

pub(crate) fn run_named_hook_with_args_until_failure<R: HookRuntime>(
    runtime: &mut R,
    args: &[Value],
) -> EvalResult {
    let hook_sym = resolve_hook_symbol(runtime.hook_context(), args[0])?;
    let hook_value = hook_value_by_id(runtime.hook_context(), hook_sym).unwrap_or(Value::NIL);
    run_hook_value_until_failure(runtime, hook_sym, hook_value, &args[1..], true)
}

pub(crate) fn run_named_hook_wrapped<R: HookRuntime>(
    runtime: &mut R,
    args: &[Value],
) -> EvalResult {
    let hook_sym = resolve_hook_symbol(runtime.hook_context(), args[0])?;
    let wrapper = args[1];
    let hook_value = hook_value_by_id(runtime.hook_context(), hook_sym).unwrap_or(Value::NIL);
    run_hook_value_wrapped(runtime, hook_sym, hook_value, wrapper, &args[2..], true)
}
