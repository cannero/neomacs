use super::*;
use crate::emacs_core::fontset;
use crate::emacs_core::intern::{NIL_SYM_ID, T_SYM_ID, is_canonical_id};
use crate::emacs_core::symbol::Obarray;

// ===========================================================================
// Symbol operations (need evaluator for obarray access)
// ===========================================================================

pub(crate) const RAW_SYMBOL_PLIST_PROPERTY: &str = "neovm--raw-symbol-plist";

pub(crate) fn is_internal_symbol_plist_property(property: &str) -> bool {
    property == RAW_SYMBOL_PLIST_PROPERTY
}

pub(crate) fn symbol_id(value: &Value) -> Option<SymId> {
    match value.kind() {
        ValueKind::Nil => Some(NIL_SYM_ID),
        ValueKind::T => Some(T_SYM_ID),
        ValueKind::Symbol(id) => Some(id),
        _ => None,
    }
}

fn value_from_symbol_id(id: SymId) -> Value {
    if is_canonical_id(id) {
        if id == NIL_SYM_ID {
            return Value::NIL;
        }
        if id == T_SYM_ID {
            return Value::T;
        }
        let name = resolve_sym(id);
        if name.starts_with(':') {
            return Value::from_kw_id(id);
        }
    }
    Value::from_sym_id(id)
}

pub(crate) trait MacroexpandRuntime {
    fn symbol_function_by_id(&self, symbol: SymId) -> Option<Value>;
    fn autoload_do_load_macro(&mut self, autoload: Value, head: Value) -> Result<(), Flow>;
    fn apply_macro_function(
        &mut self,
        form: Value,
        definition: Value,
        args: Vec<Value>,
        environment: Option<Value>,
    ) -> Result<Value, Flow>;
}

impl MacroexpandRuntime for super::eval::Context {
    fn symbol_function_by_id(&self, symbol: SymId) -> Option<Value> {
        symbol_function_cell_in_obarray(self.obarray(), symbol)
    }

    fn autoload_do_load_macro(&mut self, autoload: Value, head: Value) -> Result<(), Flow> {
        let _ = super::autoload::builtin_autoload_do_load(
            self,
            vec![autoload, head, Value::symbol("macro")],
        )?;
        Ok(())
    }

    fn apply_macro_function(
        &mut self,
        form: Value,
        definition: Value,
        args: Vec<Value>,
        environment: Option<Value>,
    ) -> Result<Value, Flow> {
        self.expand_macro_for_macroexpand(form, definition, args, environment)
    }
}

pub(crate) fn constant_set_outcome_in_obarray(
    obarray: &Obarray,
    symbol: SymId,
    symbol_arg: Value,
    new_value: Value,
) -> Option<EvalResult> {
    if !obarray.is_constant_id(symbol) {
        return None;
    }

    let name = resolve_sym(symbol);
    if name.starts_with(':') && eq_value(&Value::from_kw_id(symbol), &new_value) {
        return Some(Ok(new_value));
    }

    Some(Err(signal("setting-constant", vec![symbol_arg])))
}

pub(crate) fn expect_symbol_id(value: &Value) -> Result<SymId, Flow> {
    symbol_id(value).ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *value],
        )
    })
}

pub(crate) fn is_canonical_symbol_id(id: SymId) -> bool {
    is_canonical_id(id)
}

pub(crate) fn resolve_variable_alias_id_in_obarray(
    obarray: &Obarray,
    symbol: SymId,
) -> Result<SymId, Flow> {
    // Phase 3 of the symbol-redirect refactor: walk via the new
    // `flags.redirect() == Varalias` + `val.alias` path through
    // `Obarray::indirect_variable_id`. Mirrors GNU's
    // `indirect_variable` (`src/data.c:1284-1301`) and the `goto
    // start` loop in `find_symbol_value` (`src/data.c:1593-1595`).
    //
    // Returns the chain terminus on success, or
    // `cyclic-variable-indirection` if a cycle is detected via Floyd's
    // tortoise/hare.
    obarray.indirect_variable_id(symbol).ok_or_else(|| {
        signal(
            "cyclic-variable-indirection",
            vec![Value::from_sym_id(symbol)],
        )
    })
}

pub(crate) fn resolve_variable_alias_id(
    eval: &super::eval::Context,
    symbol: SymId,
) -> Result<SymId, Flow> {
    resolve_variable_alias_id_in_obarray(&eval.obarray, symbol)
}

pub(crate) fn resolve_variable_alias_name(
    eval: &super::eval::Context,
    name: &str,
) -> Result<String, Flow> {
    resolve_variable_alias_name_in_obarray(&eval.obarray, name)
}

pub(crate) fn resolve_variable_alias_name_in_obarray(
    obarray: &Obarray,
    name: &str,
) -> Result<String, Flow> {
    Ok(resolve_sym(resolve_variable_alias_id_in_obarray(obarray, intern(name))?).to_string())
}

fn would_create_variable_alias_cycle(eval: &super::eval::Context, new: &str, old: &str) -> bool {
    would_create_variable_alias_cycle_in_obarray(eval.obarray(), intern(new), intern(old))
}

pub(crate) fn would_create_variable_alias_cycle_in_obarray(
    obarray: &Obarray,
    new_symbol: SymId,
    old_symbol: SymId,
) -> bool {
    use crate::emacs_core::symbol::SymbolRedirect;

    // Phase 3: walk via the new redirect tag instead of the legacy
    // SymbolValue enum. Mirrors GNU `Fdefvaralias`'s base-chain walk
    // (`src/eval.c:631-726`).
    let mut current = old_symbol;
    loop {
        if current == new_symbol {
            return true;
        }
        match obarray.get_by_id(current) {
            Some(sym) if sym.flags.redirect() == SymbolRedirect::Varalias => {
                current = unsafe { sym.val.alias };
            }
            _ => return false,
        }
    }
}

pub(crate) fn symbol_raw_plist_value_in_obarray(obarray: &Obarray, symbol: SymId) -> Option<Value> {
    obarray
        .get_property_id(symbol, intern(RAW_SYMBOL_PLIST_PROPERTY))
        .cloned()
}

fn symbol_raw_plist_value(eval: &super::eval::Context, symbol: SymId) -> Option<Value> {
    symbol_raw_plist_value_in_obarray(eval.obarray(), symbol)
}

pub(crate) fn visible_symbol_plist_snapshot_in_obarray(obarray: &Obarray, symbol: SymId) -> Value {
    let Some(sym) = obarray.get_by_id(symbol) else {
        return Value::NIL;
    };

    let mut items = Vec::new();
    for (key, value) in &sym.plist {
        if is_internal_symbol_plist_property(resolve_sym(*key)) {
            continue;
        }
        items.push(value_from_symbol_id(*key));
        items.push(*value);
    }

    if items.is_empty() {
        Value::NIL
    } else {
        Value::list(items)
    }
}

fn visible_symbol_plist_entries(plist: Value) -> Vec<(SymId, Value)> {
    let mut entries = Vec::new();
    let mut cursor = plist;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return entries,
            ValueKind::Cons => {
                let key = cursor.cons_car();
                let rest = cursor.cons_cdr();

                let Some(key_id) = symbol_id(&key) else {
                    return entries;
                };
                if !rest.is_cons() {
                    return entries;
                };

                let value = rest.cons_car();
                cursor = rest.cons_cdr();

                if is_internal_symbol_plist_property(resolve_sym(key_id)) {
                    continue;
                }
                entries.push((key_id, value));
            }
            _ => return entries,
        }
    }
}

pub(crate) fn set_symbol_raw_plist_in_obarray(obarray: &mut Obarray, symbol: SymId, plist: Value) {
    let preserved_internal = obarray
        .get_by_id(symbol)
        .map(|sym| {
            sym.plist
                .iter()
                .filter(|(key, _)| is_internal_symbol_plist_property(resolve_sym(**key)))
                .map(|(key, value)| (*key, *value))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut entries = preserved_internal;
    entries.push((intern(RAW_SYMBOL_PLIST_PROPERTY), plist));
    entries.extend(visible_symbol_plist_entries(plist));
    obarray.replace_symbol_plist_id(symbol, entries);
}

fn set_symbol_raw_plist(eval: &mut super::eval::Context, symbol: SymId, plist: Value) {
    set_symbol_raw_plist_in_obarray(eval.obarray_mut(), symbol, plist);
}

pub(crate) fn plist_lookup_value(plist: &Value, prop: &Value) -> Option<Value> {
    let mut cursor = *plist;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return None,
            ValueKind::Cons => {
                let key = cursor.cons_car();
                let rest = cursor.cons_cdr();
                if !rest.is_cons() {
                    return None;
                };
                let value = rest.cons_car();
                let next = rest.cons_cdr();
                if eq_value(&key, prop) {
                    return Some(value);
                }
                cursor = next;
            }
            _ => return None,
        }
    }
}

pub(crate) fn builtin_boundp(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let obarray = eval.obarray();
    expect_args("boundp", &args, 1)?;
    let resolved = resolve_variable_alias_id_in_obarray(obarray, expect_symbol_id(&args[0])?)?;
    // specbind writes directly to obarray, so no dynamic stack lookup needed.
    let resolved_name = resolve_sym(resolved);
    if let Some(buf) = eval.buffers.current_buffer() {
        if let Some(binding) = buf.get_buffer_local_binding(resolved_name) {
            return Ok(Value::bool_val(binding.as_value().is_some()));
        }
    }
    Ok(Value::bool_val(
        obarray.boundp_id(resolved) || obarray.is_constant_id(resolved),
    ))
}

pub(crate) fn builtin_obarrayp(args: Vec<Value>) -> EvalResult {
    expect_args("obarrayp", &args, 1)?;
    Ok(Value::bool_val(expect_obarray_vector_id(&args[0]).is_ok()))
}

pub(crate) fn builtin_special_variable_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("special-variable-p", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    // Match GNU eval.c Fspecial_variable_p: this is a direct declared-special
    // bit test on the symbol itself, not an alias walk and not a constant
    // check.  Canonical keywords become special when materialized in the
    // initial obarray, mirroring lread.c intern_sym.
    eval.obarray_mut().ensure_interned_global_id(symbol);
    Ok(Value::bool_val(eval.obarray().is_special_id(symbol)))
}

pub(crate) fn builtin_default_boundp(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("default-boundp", &args, 1)?;
    let obarray = eval.obarray();
    let resolved = resolve_variable_alias_id_in_obarray(obarray, expect_symbol_id(&args[0])?)?;
    // boundp_id already returns true for BUFFER_OBJFWD slots
    // (Phase 10D), so default-boundp picks that up automatically.
    Ok(Value::bool_val(
        obarray.boundp_id(resolved) || obarray.is_constant_id(resolved),
    ))
}

pub(crate) fn builtin_default_toplevel_value(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("default-toplevel-value", &args, 1)?;
    let obarray = eval.obarray();
    let symbol = expect_symbol_id(&args[0])?;
    let resolved = resolve_variable_alias_id_in_obarray(obarray, symbol)?;
    let resolved_name = resolve_sym(resolved);
    match crate::emacs_core::eval::default_toplevel_value_in_state(
        obarray,
        eval.specpdl.as_slice(),
        resolved,
    ) {
        Some(value) => Ok(value),
        None if is_canonical_symbol_id(resolved) && resolved_name.starts_with(':') => {
            Ok(Value::from_kw_id(resolved))
        }
        None => Err(signal("void-variable", vec![args[0]])),
    }
}

pub(crate) fn builtin_internal_define_uninitialized_variable(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("internal--define-uninitialized-variable", &args, 1, 2)?;
    let symbol = expect_symbol_id(&args[0])?;
    let documentation = args.get(1).copied().unwrap_or(Value::NIL);

    eval.note_macro_expansion_mutation();
    eval.obarray_mut().make_special_id(symbol);

    if !documentation.is_nil() {
        eval.obarray_mut()
            .put_property_id(symbol, intern("variable-documentation"), documentation);
        preflight_symbol_plist_put(eval, &args[0], "variable-documentation")?;
    }

    Ok(Value::NIL)
}

pub(crate) fn builtin_set_default_toplevel_value(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    set_default_toplevel_value_impl(eval, args.clone())?;
    let symbol = expect_symbol_id(&args[0])?;
    let resolved = resolve_variable_alias_id(eval, symbol)?;
    let value = args[1];
    eval.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "set")?;
    if resolved != symbol {
        eval.run_variable_watchers_by_id(resolved, &value, &Value::NIL, "set")?;
    }
    Ok(Value::NIL)
}

pub(crate) fn set_default_toplevel_value_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-default-toplevel-value", &args, 2)?;
    let symbol = expect_symbol_id(&args[0])?;
    let resolved = resolve_variable_alias_id_in_obarray(&ctx.obarray, symbol)?;
    if ctx.obarray.is_constant_id(resolved) {
        return Err(signal("setting-constant", vec![args[0]]));
    }
    let value = args[1];
    ctx.note_macro_expansion_mutation();
    if !crate::emacs_core::eval::set_default_toplevel_value_in_state(
        ctx.specpdl.as_mut_slice(),
        resolved,
        value,
    ) {
        ctx.obarray.set_symbol_value_id(resolved, value);
    }
    ctx.mark_gc_runtime_settings_dirty_by_id(resolved);
    Ok(Value::NIL)
}

pub(crate) fn builtin_defvaralias(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let state_change = defvaralias_impl(eval, args.clone())?;
    eval.run_variable_watchers_by_id(
        state_change.previous_target_id,
        &state_change.base_variable,
        &Value::NIL,
        "defvaralias",
    )?;
    eval.watchers.clear_watchers(state_change.alias_id);
    // GNU Emacs updates `variable-documentation` through plist machinery after
    // installing alias state, so malformed raw plists still raise
    // `(wrong-type-argument plistp ...)` with the alias edge retained.
    builtin_put(
        eval,
        vec![
            args[0],
            Value::symbol("variable-documentation"),
            state_change.docstring,
        ],
    )?;
    Ok(state_change.result)
}

pub(crate) struct DefvaraliasStateChange {
    pub(crate) alias_id: SymId,
    pub(crate) previous_target_id: SymId,
    pub(crate) base_variable: Value,
    pub(crate) docstring: Value,
    pub(crate) result: Value,
}

pub(crate) fn defvaralias_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> Result<DefvaraliasStateChange, Flow> {
    expect_range_args("defvaralias", &args, 2, 3)?;
    let new_symbol = expect_symbol_id(&args[0])?;
    let old_symbol = expect_symbol_id(&args[1])?;
    let new_name = resolve_sym(new_symbol).to_string();
    if ctx.obarray.is_constant_id(new_symbol) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Cannot make a constant an alias: {new_name}"
            ))],
        ));
    }
    if would_create_variable_alias_cycle_in_obarray(&ctx.obarray, new_symbol, old_symbol) {
        return Err(signal("cyclic-variable-indirection", vec![args[1]]));
    }
    let previous_target_id = resolve_variable_alias_id_in_obarray(&ctx.obarray, new_symbol)?;
    ctx.note_macro_expansion_mutation();
    ctx.obarray.make_special_id(new_symbol);
    ctx.obarray.make_alias(new_symbol, old_symbol);
    ctx.obarray.make_special_id(old_symbol);
    ctx.mark_gc_runtime_settings_dirty_by_id(new_symbol);
    preflight_symbol_plist_put_in_obarray(&mut ctx.obarray, new_symbol, "variable-documentation")?;
    let docstring = args.get(2).cloned().unwrap_or(Value::NIL);
    Ok(DefvaraliasStateChange {
        alias_id: new_symbol,
        previous_target_id,
        base_variable: args[1],
        docstring,
        result: args[1],
    })
}

pub(crate) fn builtin_indirect_variable(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("indirect-variable", &args, 1)?;
    let obarray = eval.obarray();
    let Some(symbol) = symbol_id(&args[0]) else {
        return Ok(args[0]);
    };
    let resolved = resolve_variable_alias_id_in_obarray(obarray, symbol)?;
    Ok(value_from_symbol_id(resolved))
}

pub(crate) fn builtin_fboundp(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("fboundp", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    Ok(Value::bool_val(
        symbol_function_cell_in_obarray(eval.obarray(), symbol)
            .is_some_and(|function| !function.is_nil()),
    ))
}

pub(crate) fn builtin_symbol_value(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("symbol-value", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    match eval.visible_runtime_variable_value_by_id(symbol)? {
        Some(value) => Ok(value),
        None => Err(signal("void-variable", vec![args[0]])),
    }
}

pub(super) fn startup_virtual_autoload_function_cell(
    _eval: &super::eval::Context,
    _name: &str,
) -> Option<Value> {
    None
}

pub(crate) fn builtin_symbol_function(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    symbol_function_impl(eval.obarray(), args)
}

/// Obarray-only implementation shared by `builtin_symbol_function` and doc.rs.
pub(crate) fn symbol_function_impl(obarray: &Obarray, args: Vec<Value>) -> EvalResult {
    expect_args("symbol-function", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    let name = resolve_sym(symbol);
    if obarray.is_function_unbound_id(symbol) {
        return Ok(Value::NIL);
    }

    if let Some(function) = obarray.symbol_function_id(symbol) {
        return Ok(*function);
    }

    if !is_canonical_symbol_id(symbol) {
        return Ok(Value::NIL);
    }

    Ok(symbol_function_cell_in_obarray(obarray, symbol).unwrap_or(Value::NIL))
}

pub(crate) fn builtin_func_arity(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let obarray = eval.obarray();
    expect_args("func-arity", &args, 1)?;

    if let Some(name) = args[0].as_symbol_name() {
        if let Some(function) =
            resolve_indirect_symbol_by_id_in_obarray(obarray, intern(name)).map(|(_, value)| value)
        {
            if function.is_nil() {
                return Err(signal("void-function", vec![Value::symbol(name)]));
            }
            if super::subr_info::subr_dispatch_kind_from_value(&function)
                .is_some_and(|kind| kind == crate::tagged::header::SubrDispatchKind::SpecialForm)
            {
                return super::subr_info::builtin_func_arity_ctx(eval, vec![function]);
            }
            if let Some(arity) =
                dispatch_symbol_func_arity_override_in_obarray(obarray, name, &function)
            {
                return Ok(arity);
            }
            return super::subr_info::builtin_func_arity_ctx(eval, vec![function]);
        }
        return Err(signal("void-function", vec![Value::symbol(name)]));
    }

    super::subr_info::builtin_func_arity_ctx(eval, vec![args[0]])
}

fn dispatch_symbol_func_arity_override_in_obarray(
    obarray: &Obarray,
    name: &str,
    function: &Value,
) -> Option<Value> {
    // Only applies to builtin functions (those with Subr function cells).
    if !obarray.symbol_function(name).is_some_and(|v| v.is_subr()) {
        return None;
    }

    if super::autoload::is_autoload_value(function) {
        return Some(super::subr_info::dispatch_subr_arity_value(name));
    }

    None
}

pub(crate) fn builtin_set(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("set", &args, 2)?;
    let symbol = expect_symbol_id(&args[0])?;
    let resolved = resolve_variable_alias_id(eval, symbol)?;
    let value = args[1];
    if let Some(result) = constant_set_outcome_in_obarray(eval.obarray(), resolved, args[0], value)
    {
        return result;
    }
    let where_value = eval
        .set_runtime_binding_by_id(resolved, value)
        .map(Value::make_buffer)
        .unwrap_or(Value::NIL);
    eval.run_variable_watchers_by_id_with_where(
        resolved,
        &value,
        &Value::NIL,
        "set",
        &where_value,
    )?;
    Ok(value)
}

pub(crate) fn builtin_fset(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("fset", &args, 2)?;
    let symbol = expect_symbol_id(&args[0])?;
    if symbol == intern("nil") && !args[1].is_nil() {
        return Err(signal("setting-constant", vec![Value::symbol("nil")]));
    }
    let def = args[1];
    let would_cycle = {
        let obarray = eval.obarray_mut();
        would_create_function_alias_cycle_in_obarray(obarray, symbol, &def)
    };
    if would_cycle {
        return Err(signal("cyclic-function-indirection", vec![args[0]]));
    }
    eval.note_macro_expansion_mutation();
    eval.obarray_mut().set_symbol_function_id(symbol, def);
    Ok(def)
}

pub(crate) fn would_create_function_alias_cycle(
    eval: &super::eval::Context,
    target_symbol: SymId,
    def: &Value,
) -> bool {
    would_create_function_alias_cycle_in_obarray(eval.obarray(), target_symbol, def)
}

pub(crate) fn would_create_function_alias_cycle_in_obarray(
    obarray: &Obarray,
    target_symbol: SymId,
    def: &Value,
) -> bool {
    let mut current = match symbol_id(def) {
        Some(id) if id == intern("nil") => return false,
        Some(id) => id,
        None => return false,
    };
    let mut seen = HashSet::new();

    loop {
        if current == target_symbol {
            return true;
        }
        if !seen.insert(current) {
            return true;
        }

        let next = match obarray.symbol_function_id(current) {
            Some(function) => match symbol_id(function) {
                Some(id) => id,
                None => return false,
            },
            None => return false,
        };
        current = next;
    }
}

pub(crate) fn builtin_makunbound(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("makunbound", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    let resolved = resolve_variable_alias_id(eval, symbol)?;
    if eval.obarray().is_constant_id(resolved) {
        return Err(signal("setting-constant", vec![args[0]]));
    }
    eval.note_macro_expansion_mutation();
    eval.makunbound_runtime_binding_by_id(resolved);
    eval.run_variable_watchers_by_id(resolved, &Value::NIL, &Value::NIL, "makunbound")?;
    Ok(args[0])
}

pub(crate) fn builtin_defvar_1(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("defvar-1", &args, 2, 3)?;
    let symbol = expect_symbol_id(&args[0])?;
    let documentation = args.get(2).copied().unwrap_or(Value::NIL);
    let was_bound = builtin_default_boundp(eval, vec![args[0]])?.is_truthy();

    if documentation.is_nil() {
        builtin_internal_define_uninitialized_variable(eval, vec![args[0]])?;
    } else {
        builtin_internal_define_uninitialized_variable(eval, vec![args[0], documentation])?;
    }

    if !was_bound {
        builtin_set_default_toplevel_value(eval, vec![args[0], args[1]])?;
    }

    Ok(Value::from_sym_id(symbol))
}

pub(crate) fn builtin_defconst_1(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("defconst-1", &args, 2, 3)?;
    let symbol = expect_symbol_id(&args[0])?;
    let documentation = args.get(2).copied().unwrap_or(Value::NIL);

    if documentation.is_nil() {
        builtin_internal_define_uninitialized_variable(eval, vec![args[0]])?;
    } else {
        builtin_internal_define_uninitialized_variable(eval, vec![args[0], documentation])?;
    }

    let resolved = resolve_variable_alias_id(eval, symbol)?;
    let value = args[1];
    eval.note_macro_expansion_mutation();
    eval.obarray_mut().set_symbol_value_id(resolved, value);
    eval.mark_gc_runtime_settings_dirty_by_id(resolved);
    eval.obarray_mut().set_constant_id(resolved);
    eval.obarray_mut()
        .put_property_id(resolved, intern("risky-local-variable"), Value::T);

    Ok(Value::from_sym_id(symbol))
}

pub(crate) fn builtin_fmakunbound(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("fmakunbound", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    if symbol == intern("nil") || symbol == intern("t") {
        return Err(signal("setting-constant", vec![args[0]]));
    }
    eval.note_macro_expansion_mutation();
    eval.obarray_mut().fmakunbound_id(symbol);
    Ok(args[0])
}

pub(crate) fn builtin_get(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("get", &args, 2)?;
    let sym = expect_symbol_id(&args[0])?;
    if let Some(raw) = symbol_raw_plist_value(eval, sym) {
        return Ok(plist_lookup_value(&raw, &args[1]).unwrap_or(Value::NIL));
    }
    let prop = expect_symbol_id(&args[1])?;
    if is_internal_symbol_plist_property(resolve_sym(prop)) {
        return Ok(Value::NIL);
    }
    Ok(eval
        .obarray()
        .get_property_id(sym, prop)
        .cloned()
        .unwrap_or(Value::NIL))
}

pub(crate) fn builtin_put(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    ctx.note_macro_expansion_mutation();
    put_in_obarray(&mut ctx.obarray, args)
}

pub(crate) fn put_in_obarray(obarray: &mut Obarray, args: Vec<Value>) -> EvalResult {
    expect_args("put", &args, 3)?;
    let sym = expect_symbol_id(&args[0])?;
    let prop = expect_symbol_id(&args[1])?;
    let value = args[2];
    let current_plist = symbol_raw_plist_value_in_obarray(obarray, sym)
        .unwrap_or_else(|| visible_symbol_plist_snapshot_in_obarray(obarray, sym));
    let plist = builtin_plist_put(vec![current_plist, args[1], value])?;
    set_symbol_raw_plist_in_obarray(obarray, sym, plist);
    // Keep direct property lookups in sync with the Lisp-visible plist.
    obarray.put_property_id(sym, prop, value);
    Ok(value)
}

pub(crate) fn builtin_symbol_plist_fn(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("symbol-plist", &args, 1)?;
    let obarray = eval.obarray();
    let symbol = expect_symbol_id(&args[0])?;
    if let Some(raw) = symbol_raw_plist_value_in_obarray(obarray, symbol) {
        return Ok(raw);
    }
    Ok(visible_symbol_plist_snapshot_in_obarray(obarray, symbol))
}

pub(super) fn builtin_register_code_conversion_map(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let obarray = eval.obarray_mut();
    if args.len() == 2 {
        preflight_symbol_plist_put_in_obarray(
            obarray,
            expect_symbol_id(&args[0])?,
            "code-conversion-map",
        )?;
    }
    let map_id = super::ccl::builtin_register_code_conversion_map_impl(args.clone())?;

    let _ = put_in_obarray(
        obarray,
        vec![args[0], Value::symbol("code-conversion-map"), args[1]],
    )?;
    let _ = put_in_obarray(
        obarray,
        vec![args[0], Value::symbol("code-conversion-map-id"), map_id],
    )?;

    Ok(map_id)
}

fn symbol_has_valid_ccl_program_idx_in_obarray(
    obarray: &Obarray,
    symbol: &Value,
) -> Result<bool, Flow> {
    if !symbol.is_symbol() {
        return Ok(false);
    }
    let symbol = expect_symbol_id(symbol)?;
    let idx = obarray
        .get_property_id(symbol, intern("ccl-program-idx"))
        .copied()
        .unwrap_or(Value::NIL);
    Ok(idx.as_int().is_some_and(|n| n >= 0))
}

fn symbol_has_valid_ccl_program_idx(
    eval: &mut super::eval::Context,
    symbol: &Value,
) -> Result<bool, Flow> {
    symbol_has_valid_ccl_program_idx_in_obarray(eval.obarray(), symbol)
}

pub(super) fn builtin_ccl_program_p(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let obarray = eval.obarray();
    if args.len() == 1 && args[0].is_symbol() {
        return Ok(Value::bool_val(
            symbol_has_valid_ccl_program_idx_in_obarray(obarray, &args[0])?,
        ));
    }
    super::ccl::builtin_ccl_program_p_impl(args)
}

pub(super) fn builtin_ccl_execute(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let obarray = eval.obarray();
    if args.first().is_some_and(|v| v.is_symbol())
        && !symbol_has_valid_ccl_program_idx_in_obarray(obarray, &args[0])?
    {
        let mut forced = args.clone();
        forced[0] = Value::fixnum(0);
        return super::ccl::builtin_ccl_execute_impl(forced);
    }
    super::ccl::builtin_ccl_execute_impl(args)
}

pub(super) fn builtin_ccl_execute_on_string(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let obarray = eval.obarray();
    if args.first().is_some_and(|v| v.is_symbol())
        && !symbol_has_valid_ccl_program_idx_in_obarray(obarray, &args[0])?
    {
        let mut forced = args.clone();
        forced[0] = Value::fixnum(0);
        return super::ccl::builtin_ccl_execute_on_string_impl(forced);
    }
    super::ccl::builtin_ccl_execute_on_string_impl(args)
}

pub(super) fn builtin_register_ccl_program(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let obarray = eval.obarray_mut();
    let was_registered = args
        .first()
        .and_then(|v| v.as_symbol_id())
        .is_some_and(super::ccl::is_registered_ccl_program);
    let program_id = super::ccl::builtin_register_ccl_program_impl(args.clone())?;

    if was_registered {
        return Ok(program_id);
    }

    let publish = put_in_obarray(
        obarray,
        vec![args[0], Value::symbol("ccl-program-idx"), program_id],
    );
    if let Err(err) = publish {
        if let Some(name) = args[0].as_symbol_id() {
            super::ccl::unregister_registered_ccl_program(name);
        }
        return Err(err);
    }

    Ok(program_id)
}

fn preflight_symbol_plist_put(
    eval: &mut super::eval::Context,
    symbol: &Value,
    property: &str,
) -> Result<(), Flow> {
    let Some(id) = symbol_id(symbol) else {
        return Ok(());
    };
    preflight_symbol_plist_put_in_obarray(eval.obarray(), id, property)
}

fn preflight_symbol_plist_put_in_obarray(
    obarray: &Obarray,
    symbol: SymId,
    property: &str,
) -> Result<(), Flow> {
    let Some(raw) = symbol_raw_plist_value_in_obarray(obarray, symbol) else {
        return Ok(());
    };
    let _ = builtin_plist_put(vec![raw, Value::symbol(property), Value::NIL])?;
    Ok(())
}

pub(crate) fn builtin_setplist(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("setplist", &args, 2)?;
    let obarray = eval.obarray_mut();
    let symbol = expect_symbol_id(&args[0])?;
    let plist = args[1];
    set_symbol_raw_plist_in_obarray(obarray, symbol, plist);
    Ok(plist)
}

fn macroexpand_environment_binding_by_id(env: &Value, target: SymId) -> Option<Value> {
    let mut cursor = *env;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return None,
            ValueKind::Cons => {
                let entry = cursor.cons_car();
                cursor = cursor.cons_cdr();
                if !entry.is_cons() {
                    continue;
                };
                let entry_car = entry.cons_car();
                let entry_cdr = entry.cons_cdr();
                if matches!(symbol_id(&entry_car), Some(id) if id == target) {
                    return Some(entry_cdr);
                }
            }
            _ => return None,
        }
    }
}

fn macroexpand_environment_callable(binding: &Value) -> Result<Value, Flow> {
    Ok(*binding)
}

#[inline]
fn macroexpand_definition_is_macro(definition: &Value) -> bool {
    matches!(definition.kind(), ValueKind::Veclike(VecLikeType::Macro))
        || (definition.is_cons() && definition.cons_car().is_symbol_named("macro"))
}

#[tracing::instrument(level = "trace", skip(runtime, environment), fields(head))]
fn macroexpand_once_with_environment<R: MacroexpandRuntime>(
    runtime: &mut R,
    form: Value,
    environment: Option<&Value>,
) -> Result<(Value, bool), Flow> {
    if !form.is_cons() {
        return Ok((form, false));
    };
    let form_pair_car = form.cons_car();
    let form_pair_cdr = form.cons_cdr();
    let head = form_pair_car;
    let tail = form_pair_cdr;
    let Some(head_id) = symbol_id(&head) else {
        return Ok((form, false));
    };
    if let Some(env) = environment
        && !env.is_nil()
        && !env.is_cons()
    {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *env],
        ));
    }

    // Match GNU `eval.c:Fmacroexpand`: walk symbol aliases one hop at a time,
    // consulting ENVIRONMENT at each hop before following the function cell.
    let mut current_definition = head;
    let mut current_symbol = head_id;
    let mut environment_binding = None;
    while let Some(definition_symbol) = symbol_id(&current_definition) {
        current_symbol = definition_symbol;
        if let Some(env) = environment {
            if let Some(binding) = macroexpand_environment_binding_by_id(env, definition_symbol) {
                environment_binding = Some(binding);
                break;
            }
        }

        let Some(function) = runtime.symbol_function_by_id(definition_symbol) else {
            current_definition = Value::NIL;
            break;
        };
        current_definition = function;
        if !function.is_nil() {
            continue;
        }
        break;
    }

    let function = if let Some(binding) = environment_binding {
        if binding.is_nil() {
            None
        } else {
            Some(macroexpand_environment_callable(&binding)?)
        }
    } else {
        let mut global = current_definition;
        if super::autoload::is_autoload_value(&global) {
            runtime.autoload_do_load_macro(global, value_from_symbol_id(current_symbol))?;
            global = runtime
                .symbol_function_by_id(current_symbol)
                .unwrap_or(Value::NIL);
        }

        if macroexpand_definition_is_macro(&global) {
            Some(global)
        } else {
            None
        }
    };

    let Some(function) = function else {
        return Ok((form, false));
    };
    let args = list_to_vec(&tail)
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), tail]))?;
    let expanded = runtime.apply_macro_function(form, function, args, environment.copied())?;
    // Match real Emacs (eval.c line 1319): if the macro expander returned
    // the same form object (EQ), treat it as "no expansion occurred".
    let did_expand = !eq_value(&form, &expanded);
    Ok((expanded, did_expand))
}

pub(crate) fn builtin_macroexpand_with_runtime<R: MacroexpandRuntime>(
    runtime: &mut R,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("macroexpand", &args, 1, 2)?;
    let mut form = args[0];
    let environment = args.get(1);
    loop {
        let (expanded, did_expand) = macroexpand_once_with_environment(runtime, form, environment)?;
        if !did_expand {
            return Ok(expanded);
        }
        form = expanded;
    }
}

pub(crate) fn builtin_macroexpand(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_macroexpand_with_runtime(eval, args)
}

pub(crate) fn builtin_indirect_function(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    indirect_function_impl(eval.obarray(), args)
}

/// Obarray-only implementation shared by `builtin_indirect_function` and doc.rs.
pub(crate) fn indirect_function_impl(obarray: &Obarray, args: Vec<Value>) -> EvalResult {
    expect_min_args("indirect-function", &args, 1)?;
    expect_max_args("indirect-function", &args, 2)?;

    if let Some(symbol) = symbol_id(&args[0]) {
        if let Some(function) =
            resolve_indirect_symbol_by_id_in_obarray(obarray, symbol).map(|(_, value)| value)
        {
            return Ok(function);
        }
        return Ok(Value::NIL);
    }

    Ok(args[0])
}

fn pure_builtin_symbol_alias_target(name: &str) -> Option<&'static str> {
    match name {
        "string<" => Some("string-lessp"),
        "string>" => Some("string-greaterp"),
        "string=" => Some("string-equal"),
        _ => None,
    }
}

pub(crate) fn symbol_function_cell_in_obarray(obarray: &Obarray, symbol: SymId) -> Option<Value> {
    if obarray.is_function_unbound_id(symbol) {
        return None;
    }

    if let Some(function) = obarray.symbol_function_id(symbol) {
        return Some(*function);
    }

    if !is_canonical_symbol_id(symbol) {
        return None;
    }

    let current_name = resolve_sym(symbol);

    if let Some(alias_target) = pure_builtin_symbol_alias_target(current_name) {
        return Some(Value::symbol(alias_target));
    }

    None
}

pub(crate) fn resolve_indirect_symbol_by_id_in_obarray(
    obarray: &Obarray,
    symbol: SymId,
) -> Option<(SymId, Value)> {
    let mut current = symbol;
    for _ in 0..128 {
        let function = symbol_function_cell_in_obarray(obarray, current)?;
        if let Some(next) = symbol_id(&function) {
            if next == NIL_SYM_ID {
                return Some((next, Value::NIL));
            }
            current = next;
            continue;
        }
        return Some((current, function));
    }
    None
}

pub(crate) fn resolve_indirect_symbol_by_id(
    eval: &super::eval::Context,
    symbol: SymId,
) -> Option<(SymId, Value)> {
    resolve_indirect_symbol_by_id_in_obarray(eval.obarray(), symbol)
}

fn resolve_indirect_symbol_with_name(
    eval: &super::eval::Context,
    name: &str,
) -> Option<(String, Value)> {
    resolve_indirect_symbol_by_id(eval, intern(name))
        .map(|(resolved, value)| (resolve_sym(resolved).to_string(), value))
}

pub(super) fn resolve_indirect_symbol(eval: &super::eval::Context, name: &str) -> Option<Value> {
    resolve_indirect_symbol_with_name(eval, name).map(|(_, value)| value)
}

pub(crate) fn builtin_macrop(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("macrop", &args, 1)?;
    if let Some(symbol) = symbol_id(&args[0]) {
        if is_canonical_symbol_id(symbol) {
            if let Some(function) =
                startup_virtual_autoload_function_cell(eval, resolve_sym(symbol))
            {
                return super::subr_info::macrop_check(&function);
            }
        }
        if let Some(function) = resolve_indirect_symbol_by_id(eval, symbol).map(|(_, value)| value)
        {
            return super::subr_info::macrop_check(&function);
        }
        return Ok(Value::NIL);
    }

    super::subr_info::macrop_check(&args[0])
}

/// Hash a string for custom obarray bucket index.
pub(crate) fn obarray_hash_lisp_string(s: &crate::heap_types::LispString, len: usize) -> usize {
    let hash = s
        .as_bytes()
        .iter()
        .fold(if s.is_multibyte() { 1u64 } else { 0u64 }, |h, b| {
            h.wrapping_mul(31).wrapping_add(*b as u64)
        });
    hash as usize % len
}

pub(crate) fn obarray_hash(s: &str, len: usize) -> usize {
    let hash = s
        .bytes()
        .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
    hash as usize % len
}

/// Search a bucket chain (cons list) for a symbol with the given name.
/// Returns the symbol Value if found.
pub(crate) fn obarray_bucket_find(
    bucket: Value,
    name: &crate::heap_types::LispString,
) -> Option<Value> {
    let mut current = bucket;
    loop {
        match current.kind() {
            ValueKind::Nil => return None,
            ValueKind::Cons => {
                let car = current.cons_car();
                let cdr = current.cons_cdr();
                if let Some(sym_name) = car.as_symbol_lisp_string() {
                    if sym_name == name {
                        return Some(car);
                    }
                }
                current = cdr;
            }
            _ => return None,
        }
    }
}

pub(crate) fn is_global_obarray_proxy(eval: &super::eval::Context, value: &Value) -> bool {
    eval.obarray()
        .symbol_value("obarray")
        .is_some_and(|proxy| *proxy == *value)
}

pub(crate) fn builtin_intern_fn(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("intern", &args, 1)?;
    expect_max_args("intern", &args, 2)?;
    // Debug: validate string arg before access
    if args[0].is_string() {
        let ptr = args[0].as_string_ptr().unwrap();
        let header = unsafe { &(*(ptr as *const crate::tagged::header::StringObj)).header };
        if !matches!(header.kind, crate::tagged::header::HeapObjectKind::String) {
            // Dump bc_buf state for debugging
            let bc_buf_len = eval.bc_buf.len();
            let bc_frames_len = eval.bc_frames.len();
            let bc_frames_info: Vec<String> = eval
                .bc_frames
                .iter()
                .map(|f| format!("base={} fun={:#x}", f.base, f.fun.0))
                .collect();
            panic!(
                "INTERN BUG: string arg {:#x} (ptr {:?}) has header.kind={:?}\n\
                 bc_buf.len()={}, bc_frames={:?}\n\
                 All args: {:?}",
                args[0].0,
                ptr,
                header.kind,
                bc_buf_len,
                bc_frames_info,
                args.iter()
                    .map(|a| format!("{:#x}", a.0))
                    .collect::<Vec<_>>(),
            );
        }
    }
    if let Some(obarray) = args.get(1) {
        if !obarray.is_nil() && !obarray.is_vector() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("obarrayp"), *obarray],
            ));
        }
    }
    let name = expect_lisp_string(&args[0])?;

    // Custom obarray path
    if let Some(obarray_val) = args
        .get(1)
        .filter(|v| !v.is_nil() && v.is_vector() && !is_global_obarray_proxy(eval, v))
    {
        let vec_data = obarray_val.as_vector_data().unwrap();
        let vec_len = vec_data.len();
        if vec_len == 0 {
            return Err(signal("args-out-of-range", vec![Value::fixnum(0)]));
        }
        let bucket_idx = obarray_hash_lisp_string(name, vec_len);
        let bucket = vec_data[bucket_idx];

        // Check if already interned
        if let Some(sym) = obarray_bucket_find(bucket, name) {
            return Ok(sym);
        }

        // Not found: create symbol and prepend to bucket chain
        let sym = Value::from_sym_id(crate::emacs_core::intern::intern_uninterned_lisp_string(
            name,
        ));
        let new_bucket = Value::cons(sym, bucket);
        let _ = obarray_val.set_vector_slot(bucket_idx, new_bucket);
        return Ok(sym);
    }

    // Global obarray path
    let sym = eval.obarray_mut().intern_lisp_string(name);
    Ok(Value::from_sym_id(sym))
}

pub(crate) fn builtin_intern_soft(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if let Some(obarray) = args.get(1).filter(|v| !v.is_nil()) {
        if is_global_obarray_proxy(eval, obarray) {
            let mut global_args = args;
            global_args.truncate(1);
            return intern_soft_impl(eval.obarray(), global_args);
        }
    }
    intern_soft_impl(eval.obarray(), args)
}

pub(crate) fn intern_soft_impl(obarray: &Obarray, args: Vec<Value>) -> EvalResult {
    expect_min_args("intern-soft", &args, 1)?;
    expect_max_args("intern-soft", &args, 2)?;
    if let Some(obarray) = args.get(1) {
        if !obarray.is_nil() && !obarray.is_vector() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("obarrayp"), *obarray],
            ));
        }
    }

    // Custom obarray path
    if let Some(obarray_val) = args.get(1).filter(|v| !v.is_nil() && v.is_vector()) {
        let name = match args[0].kind() {
            ValueKind::String => std::borrow::Cow::Borrowed(args[0].as_lisp_string().unwrap()),
            ValueKind::Symbol(id) => {
                std::borrow::Cow::Borrowed(crate::emacs_core::intern::resolve_sym_lisp_string(id))
            }
            ValueKind::Nil => std::borrow::Cow::Borrowed(
                crate::emacs_core::intern::resolve_sym_lisp_string(NIL_SYM_ID),
            ),
            ValueKind::T => std::borrow::Cow::Borrowed(
                crate::emacs_core::intern::resolve_sym_lisp_string(T_SYM_ID),
            ),
            _other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), args[0]],
                ));
            }
        };
        let vec_data = obarray_val.as_vector_data().unwrap();
        let vec_len = vec_data.len();
        if vec_len == 0 {
            return Ok(Value::NIL);
        }
        let bucket_idx = obarray_hash_lisp_string(name.as_ref(), vec_len);
        let bucket = vec_data[bucket_idx];
        return Ok(obarray_bucket_find(bucket, name.as_ref()).unwrap_or(Value::NIL));
    }

    // Global obarray path
    let name = match args[0].kind() {
        ValueKind::String => std::borrow::Cow::Borrowed(args[0].as_lisp_string().unwrap()),
        ValueKind::Nil => std::borrow::Cow::Borrowed(
            crate::emacs_core::intern::resolve_sym_lisp_string(NIL_SYM_ID),
        ),
        ValueKind::T => {
            std::borrow::Cow::Borrowed(crate::emacs_core::intern::resolve_sym_lisp_string(T_SYM_ID))
        }
        ValueKind::Symbol(id) => {
            std::borrow::Cow::Borrowed(crate::emacs_core::intern::resolve_sym_lisp_string(id))
        }
        _other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };
    if let Some(id) = obarray.intern_soft_lisp_string(name.as_ref()) {
        Ok(Value::from_sym_id(id))
    } else {
        Ok(Value::NIL)
    }
}

pub(crate) fn builtin_obarray_make(args: Vec<Value>) -> EvalResult {
    expect_range_args("obarray-make", &args, 0, 1)?;
    let size = if args.is_empty() || args[0].is_nil() {
        1511usize
    } else {
        expect_wholenump(&args[0])? as usize
    };
    Ok(Value::vector(vec![Value::NIL; size]))
}

pub(crate) fn expect_obarray_vector_id(value: &Value) -> Result<Value, Flow> {
    if !value.is_vector() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("obarrayp"), *value],
        ));
    };
    let is_obarray = value
        .as_vector_data()
        .unwrap()
        .iter()
        .all(|slot| slot.is_nil() || slot.is_cons());
    if !is_obarray {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("obarrayp"), *value],
        ));
    }
    Ok(*value)
}

pub(crate) fn builtin_obarray_clear(args: Vec<Value>) -> EvalResult {
    expect_args("obarray-clear", &args, 1)?;
    let obarray_val = expect_obarray_vector_id(&args[0])?;
    let vec_len = obarray_val.as_vector_data().map_or(0, |vec| vec.len());
    let _ = obarray_val.replace_vector_data(vec![Value::NIL; vec_len]);
    Ok(Value::NIL)
}

pub(crate) fn builtin_make_temp_file_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("make-temp-file-internal", &args, 4)?;
    if !args[3].is_nil() {
        // MODE is currently accepted for arity and type compatibility.
        let _ = expect_fixnum(&args[3])?;
    }
    super::fileio::builtin_make_temp_file(eval, vec![args[0], args[1], args[2]])
}

pub(crate) fn builtin_minibuffer_innermost_command_loop_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("minibuffer-innermost-command-loop-p", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_minibuffer_prompt_end(args: Vec<Value>) -> EvalResult {
    expect_args("minibuffer-prompt-end", &args, 0)?;
    Ok(Value::fixnum(1))
}

pub(crate) fn builtin_next_frame(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("next-frame", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() {
            let _ = super::window_cmds::resolve_frame_id_in_state(
                &mut eval.frames,
                &mut eval.buffers,
                Some(frame),
                "frame-live-p",
            )?;
        }
    }
    super::window_cmds::builtin_selected_frame(eval, Vec::new())
}

pub(crate) fn builtin_previous_frame(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("previous-frame", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() {
            let _ = super::window_cmds::resolve_frame_id_in_state(
                &mut eval.frames,
                &mut eval.buffers,
                Some(frame),
                "frame-live-p",
            )?;
        }
    }
    super::window_cmds::builtin_selected_frame(eval, Vec::new())
}

pub(crate) fn builtin_raise_frame(args: Vec<Value>) -> EvalResult {
    expect_range_args("raise-frame", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !frame.is_frame() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_redisplay(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("redisplay", &args, 0, 1)?;
    if eval
        .eval_symbol("executing-kbd-macro")
        .is_ok_and(|value| !value.is_nil())
    {
        return Ok(Value::NIL);
    }
    eval.redisplay();
    Ok(Value::T)
}

pub(crate) fn builtin_suspend_emacs(args: Vec<Value>) -> EvalResult {
    expect_range_args("suspend-emacs", &args, 0, 1)?;
    Ok(Value::NIL)
}

/// `(vertical-motion LINES &optional WINDOW CUR-COL)` -> integer
///
/// Move point to the start of the screen line LINES lines down (or up if
/// negative).  Returns the number of lines actually moved.
///
/// In GNU Emacs this uses the full display engine to handle word-wrap,
/// display properties, etc.  Here we approximate with newline counting,
/// which is correct for non-wrapped lines.
pub(crate) fn builtin_vertical_motion(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &mut eval.buffers;
    expect_range_args("vertical-motion", &args, 1, 3)?;
    // First arg can be LINES (integer) or (COLS . LINES) cons pair.
    // When (COLS . LINES), move LINES then position at column COLS.
    let (cols, lines): (Option<i64>, i64) = match args[0].kind() {
        ValueKind::Fixnum(n) => (None, n),
        ValueKind::Cons => {
            let car = args[0].cons_car();
            let cdr = args[0].cons_cdr();
            let cols_val = match car.kind() {
                ValueKind::Fixnum(n) => Some(n),
                ValueKind::Float => Some(car.xfloat() as i64),
                _ => None,
            };
            let lines_val = match cdr.kind() {
                ValueKind::Fixnum(n) => n,
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("fixnump"), cdr],
                    ));
                }
            };
            (cols_val, lines_val)
        }
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), args[0]],
            ));
        }
    };
    // Validate optional WINDOW arg.
    if let Some(window) = args.get(1) {
        if !window.is_nil() && !window.is_window() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *window],
            ));
        }
    }

    let Some(current_id) = buffers.current_buffer_id() else {
        return Ok(Value::fixnum(0));
    };
    let Some(buf) = buffers.get(current_id) else {
        return Ok(Value::fixnum(0));
    };
    let text = buf.text.to_string();
    let pt = buf.pt_byte.clamp(buf.begv_byte, buf.zv_byte);
    let bytes = text.as_bytes();
    let begv = buf.begv_byte;
    let zv = buf.zv_byte;

    if lines == 0 && cols.is_none() {
        // Move to beginning of current screen line (= beginning of line).
        let mut bol = pt;
        while bol > begv && bytes[bol - 1] != b'\n' {
            bol -= 1;
        }
        let _ = buffers.goto_buffer_byte(current_id, bol);
        return Ok(Value::fixnum(0));
    }

    let mut pos = pt;
    let mut moved: i64 = 0;

    if lines > 0 {
        for _ in 0..lines {
            let mut nl = pos;
            while nl < zv && bytes[nl] != b'\n' {
                nl += 1;
            }
            if nl >= zv {
                break;
            }
            pos = nl + 1;
            moved += 1;
        }
    } else if lines < 0 {
        // Move backward: go to beginning of current line first.
        while pos > begv && bytes[pos - 1] != b'\n' {
            pos -= 1;
        }
        let target = (-lines) as usize;
        for _ in 0..target {
            if pos <= begv {
                break;
            }
            pos -= 1;
            while pos > begv && bytes[pos - 1] != b'\n' {
                pos -= 1;
            }
            moved -= 1;
        }
    } else {
        // lines == 0 but cols is Some: stay on current line, go to BOL first
        while pos > begv && bytes[pos - 1] != b'\n' {
            pos -= 1;
        }
    }

    // Now pos is at beginning of target line.
    // If COLS was specified, advance to that column.
    if let Some(target_col) = cols {
        if target_col > 0 {
            let target_col = target_col as usize;
            let mut col: usize = 0;
            while pos < zv && bytes[pos] != b'\n' && col < target_col {
                // Handle tab characters
                if bytes[pos] == b'\t' {
                    col = (col + 8) & !7; // tab stops every 8
                } else {
                    col += 1;
                }
                pos += 1;
            }
        }
    }

    let _ = buffers.goto_buffer_byte(current_id, pos);
    Ok(Value::fixnum(moved))
}

pub(crate) fn builtin_rename_buffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let buffers = &mut eval.buffers;
    expect_range_args("rename-buffer", &args, 1, 2)?;
    let name = expect_strict_string(&args[0])?;

    if name.is_empty() {
        return Err(signal(
            "error",
            vec![Value::string("Empty string is invalid as a buffer name")],
        ));
    }

    let current_id = match buffers.current_buffer() {
        Some(buf) => buf.id,
        None => {
            return Err(signal("error", vec![Value::string("No current buffer")]));
        }
    };

    let unique = args.get(1).copied().unwrap_or(Value::NIL);

    let new_name = match buffers.find_buffer_by_name(&name) {
        Some(existing_id) if existing_id == current_id => {
            // Already has this name, just return it
            name
        }
        Some(_other_id) => {
            // Name is taken by a different buffer
            if unique.is_nil() {
                return Err(signal(
                    "error",
                    vec![Value::string(format!("Buffer name `{}' is in use", name))],
                ));
            }
            buffers.generate_new_buffer_name(&name)
        }
        None => {
            // Name is free
            name
        }
    };

    let _ = buffers.set_buffer_name(current_id, Value::string(new_name.clone()));

    Ok(Value::string(new_name))
}

pub(crate) fn builtin_set_buffer_major_mode(args: Vec<Value>) -> EvalResult {
    expect_args("set-buffer-major-mode", &args, 1)?;
    let _ = expect_buffer_id(&args[0])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_set_buffer_redisplay(args: Vec<Value>) -> EvalResult {
    expect_args("set-buffer-redisplay", &args, 4)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_put_unicode_property_internal(args: Vec<Value>) -> EvalResult {
    expect_args("put-unicode-property-internal", &args, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_re_describe_compiled(args: Vec<Value>) -> EvalResult {
    expect_range_args("re--describe-compiled", &args, 1, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_map_charset_chars(args: Vec<Value>) -> EvalResult {
    expect_range_args("map-charset-chars", &args, 2, 5)?;
    Ok(Value::NIL)
}

// map-keymap and map-keymap-internal are now eval-backed in keymaps.rs

pub(crate) fn builtin_mapbacktrace(args: Vec<Value>) -> EvalResult {
    expect_range_args("mapbacktrace", &args, 1, 2)?;
    match args[0].kind() {
        ValueKind::Nil | ValueKind::T => {
            return Err(signal("void-function", vec![args[0]]));
        }
        ValueKind::Symbol(_)
        | ValueKind::Veclike(VecLikeType::Subr)
        | ValueKind::Veclike(VecLikeType::Lambda)
        | ValueKind::Veclike(VecLikeType::Macro)
        | ValueKind::Veclike(VecLikeType::ByteCode) => {}
        _ => {
            return Err(signal("invalid-function", vec![args[0]]));
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_make_record(args: Vec<Value>) -> EvalResult {
    expect_args("make-record", &args, 3)?;
    let length = expect_wholenump(&args[1])? as usize;
    let mut items = Vec::with_capacity(length + 1);
    items.push(args[0]); // type tag
    for _ in 0..length {
        items.push(args[2]); // init value
    }
    Ok(Value::make_record(items))
}

pub(crate) fn builtin_marker_last_position(args: Vec<Value>) -> EvalResult {
    expect_args("marker-last-position", &args, 1)?;
    if !super::marker::is_marker(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("markerp"), args[0]],
        ));
    }
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Marker) => {
            let marker = args[0].as_marker_data().unwrap();
            Ok(Value::fixnum(marker.position.unwrap_or(0)))
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = args[0].as_vector_data().unwrap().clone();
            Ok(items
                .get(2)
                .and_then(|value| value.as_fixnum())
                .map(Value::fixnum)
                .unwrap_or_else(|| Value::fixnum(0)))
        }
        _ => unreachable!("markerp check above guarantees a tagged marker object"),
    }
}

pub(crate) fn builtin_newline_cache_check(args: Vec<Value>) -> EvalResult {
    expect_range_args("newline-cache-check", &args, 0, 1)?;
    if let Some(buffer) = args.first() {
        if !buffer.is_nil() && !buffer.is_buffer() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("bufferp"), *buffer],
            ));
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_old_selected_frame(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("old-selected-frame", &args, 0)?;
    super::window_cmds::builtin_selected_frame(eval, Vec::new())
}

pub(crate) fn builtin_make_frame_invisible(args: Vec<Value>) -> EvalResult {
    expect_range_args("make-frame-invisible", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !frame.is_frame() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    let force = args.get(1).is_some_and(|arg| !arg.is_nil());
    if force {
        return Ok(Value::NIL);
    }
    Err(signal(
        "error",
        vec![Value::string(
            "Attempt to make invisible the sole visible or iconified frame",
        )],
    ))
}

pub(crate) fn builtin_menu_bar_menu_at_x_y(args: Vec<Value>) -> EvalResult {
    expect_range_args("menu-bar-menu-at-x-y", &args, 2, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_menu_or_popup_active_p(args: Vec<Value>) -> EvalResult {
    expect_args("menu-or-popup-active-p", &args, 0)?;
    Ok(Value::NIL)
}

fn selected_frame_value(eval: &mut super::eval::Context) -> Value {
    let fid =
        super::window_cmds::ensure_selected_frame_id_in_state(&mut eval.frames, &mut eval.buffers);
    Value::make_frame(fid.0)
}

fn maybe_transform_mouse_position(eval: &mut super::eval::Context, value: Value) -> EvalResult {
    let transform = eval
        .obarray
        .symbol_value("mouse-position-function")
        .copied()
        .unwrap_or(Value::NIL);
    if transform.is_truthy() {
        eval.apply(transform, vec![value])
    } else {
        Ok(value)
    }
}

fn pixel_to_char_mouse_position(
    eval: &super::eval::Context,
    frame_id: Option<crate::window::FrameId>,
    x: i64,
    y: i64,
) -> (Value, Value) {
    let Some(frame_id) = frame_id else {
        return (Value::NIL, Value::NIL);
    };
    let Some(frame) = eval.frames.get(frame_id) else {
        return (Value::NIL, Value::NIL);
    };
    let char_width = frame.char_width.max(1.0);
    let char_height = frame.char_height.max(1.0);
    (
        Value::fixnum((x as f32 / char_width).floor() as i64),
        Value::fixnum((y as f32 / char_height).floor() as i64),
    )
}

fn current_mouse_position_value(eval: &mut super::eval::Context, pixel_units: bool) -> EvalResult {
    let selected_frame = selected_frame_value(eval);
    let (frame_value, x, y) = match eval.command_loop.keyboard.mouse_pixel_position() {
        Some(state) => {
            let frame_value = state
                .frame_id
                .map(|frame_id| Value::make_frame(frame_id.0))
                .unwrap_or(Value::NIL);
            let (x, y) = if pixel_units {
                (Value::fixnum(state.x), Value::fixnum(state.y))
            } else {
                pixel_to_char_mouse_position(eval, state.frame_id, state.x, state.y)
            };
            (frame_value, x, y)
        }
        None => (selected_frame, Value::NIL, Value::NIL),
    };
    maybe_transform_mouse_position(eval, Value::cons(frame_value, Value::cons(x, y)))
}

pub(crate) fn builtin_mouse_pixel_position(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("mouse-pixel-position", &args, 0)?;
    current_mouse_position_value(eval, true)
}

pub(crate) fn builtin_mouse_position(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("mouse-position", &args, 0)?;
    current_mouse_position_value(eval, false)
}

pub(crate) fn builtin_native_comp_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("native-comp-available-p", &args, 0)?;
    Ok(Value::T)
}

pub(crate) fn builtin_native_comp_unit_file(args: Vec<Value>) -> EvalResult {
    expect_args("native-comp-unit-file", &args, 1)?;
    let is_native_comp_unit = match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = args[0].as_vector_data().unwrap().clone();
            items
                .first()
                .is_some_and(|v| v.as_symbol_name() == Some(":native-comp-unit"))
        }
        _ => false,
    };
    if !is_native_comp_unit {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("native-comp-unit"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_native_comp_unit_set_file(args: Vec<Value>) -> EvalResult {
    expect_args("native-comp-unit-set-file", &args, 2)?;
    let is_native_comp_unit = match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = args[0].as_vector_data().unwrap().clone();
            items
                .first()
                .is_some_and(|v| v.as_symbol_name() == Some(":native-comp-unit"))
        }
        _ => false,
    };
    if !is_native_comp_unit {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("native-comp-unit"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_native_elisp_load(args: Vec<Value>) -> EvalResult {
    expect_range_args("native-elisp-load", &args, 1, 2)?;
    let file = expect_strict_string(&args[0])?;
    Err(signal(
        "native-lisp-load-failed",
        vec![Value::string("file does not exists"), Value::string(file)],
    ))
}

pub(crate) fn fontset_alias_alist_startup_value() -> Value {
    fontset::fontset_alias_alist_startup_value()
}

pub(super) fn fontset_list_value() -> Value {
    fontset::fontset_list_value()
}

fn dynamic_or_global_symbol_value_in_state(
    obarray: &Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    name: &str,
) -> Option<Value> {
    obarray.symbol_value(name).copied()
}

pub(crate) fn builtin_new_fontset(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("new-fontset", &args, 2)?;
    let obarray = eval.obarray();
    let name = expect_strict_string(&args[0])?;
    let char_script_table =
        dynamic_or_global_symbol_value_in_state(obarray, &[], "char-script-table");
    let charset_script_alist =
        dynamic_or_global_symbol_value_in_state(obarray, &[], "charset-script-alist");
    let font_encoding_alist =
        dynamic_or_global_symbol_value_in_state(obarray, &[], "font-encoding-alist");
    let registered = fontset::new_fontset(
        &name,
        &args[1],
        char_script_table.as_ref(),
        charset_script_alist.as_ref(),
        font_encoding_alist.as_ref(),
    )?;
    Ok(Value::string(registered))
}

pub(crate) fn builtin_open_font(args: Vec<Value>) -> EvalResult {
    expect_range_args("open-font", &args, 1, 3)?;
    let is_font_entity = match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = args[0].as_vector_data().unwrap().clone();
            items
                .first()
                .is_some_and(|v| v.as_symbol_name() == Some(":font-entity"))
        }
        _ => false,
    };
    if !is_font_entity {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-entity"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

/// `(open-dribble-file FILE)` -> nil
///
/// Mirrors GNU `src/keyboard.c:12327-12367`. Opens FILE for
/// writing as the dribble file, where every input event will be
/// logged for debugging. Passing nil closes the current dribble
/// file. Keyboard audit Finding 11 in
/// `drafts/keyboard-command-loop-audit.md`: the previous body
/// validated the argument and silently dropped it.
///
/// The actual writes happen in the keyboard event-ingest path
/// (`KBoard::record_input_event`), which calls
/// `dribble_write_event` whenever the dribble file handle is
/// open.
pub(crate) fn builtin_open_dribble_file(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("open-dribble-file", &args, 1)?;
    if args[0].is_nil() {
        eval.command_loop.keyboard.kboard.close_dribble_file();
        return Ok(Value::NIL);
    }
    let path = expect_strict_string(&args[0])?;
    if let Err(err) = eval.command_loop.keyboard.kboard.open_dribble_file(&path) {
        return Err(signal(
            "file-error",
            vec![
                Value::string("Cannot open dribble file"),
                Value::string(err.to_string()),
            ],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_object_intervals(args: Vec<Value>) -> EvalResult {
    expect_args("object-intervals", &args, 1)?;
    if !(args[0].is_string() || args[0].is_buffer()) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("buffer-or-string-p"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_optimize_char_table(args: Vec<Value>) -> EvalResult {
    expect_range_args("optimize-char-table", &args, 1, 2)?;
    if !super::chartable::is_char_table(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("char-table-p"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_overlay_lists(args: Vec<Value>) -> EvalResult {
    expect_args("overlay-lists", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_overlay_recenter(args: Vec<Value>) -> EvalResult {
    expect_args("overlay-recenter", &args, 1)?;
    let _ = expect_integer_or_marker(&args[0])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_profiler_cpu_log(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-cpu-log", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_profiler_cpu_running_p(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-cpu-running-p", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_profiler_cpu_start(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-cpu-start", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_profiler_cpu_stop(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-cpu-stop", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_profiler_memory_log(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-memory-log", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_profiler_memory_running_p(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-memory-running-p", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_profiler_memory_start(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-memory-start", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_profiler_memory_stop(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-memory-stop", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_pdumper_stats(args: Vec<Value>) -> EvalResult {
    expect_args("pdumper-stats", &args, 0)?;
    Ok(crate::emacs_core::pdump::runtime::pdumper_stats_value().unwrap_or(Value::NIL))
}

pub(crate) fn builtin_position_symbol(args: Vec<Value>) -> EvalResult {
    expect_args("position-symbol", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_play_sound_internal(args: Vec<Value>) -> EvalResult {
    expect_args("play-sound-internal", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_record(args: Vec<Value>) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("record"), Value::fixnum(0)],
        ));
    }
    Ok(Value::make_record(args))
}

pub(crate) fn builtin_recordp(args: Vec<Value>) -> EvalResult {
    expect_args("recordp", &args, 1)?;
    Ok(Value::bool_val(args[0].is_record()))
}

pub(crate) fn builtin_query_font(args: Vec<Value>) -> EvalResult {
    expect_args("query-font", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_query_fontset(args: Vec<Value>) -> EvalResult {
    expect_range_args("query-fontset", &args, 1, 2)?;
    let pattern = expect_strict_string(&args[0])?;
    if pattern.is_empty() {
        return Ok(Value::NIL);
    }
    let regexpp = args.get(1).is_some_and(|v| v.is_truthy());
    Ok(fontset::query_fontset_registry(&pattern, regexpp).map_or(Value::NIL, Value::string))
}

pub(crate) fn builtin_read_positioning_symbols(args: Vec<Value>) -> EvalResult {
    expect_range_args("read-positioning-symbols", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_recent_auto_save_p(args: Vec<Value>) -> EvalResult {
    expect_args("recent-auto-save-p", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_reconsider_frame_fonts(args: Vec<Value>) -> EvalResult {
    expect_args("reconsider-frame-fonts", &args, 1)?;
    if !args[0].is_nil() && !args[0].is_frame() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), args[0]],
        ));
    }
    Err(signal(
        "error",
        vec![Value::string("Window system frame should be used")],
    ))
}

pub(crate) fn builtin_redirect_debugging_output(args: Vec<Value>) -> EvalResult {
    expect_range_args("redirect-debugging-output", &args, 1, 2)?;
    if !args[0].is_nil() {
        let _ = expect_strict_string(&args[0])?;
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_redirect_frame_focus(args: Vec<Value>) -> EvalResult {
    expect_range_args("redirect-frame-focus", &args, 1, 2)?;
    if !args[0].is_nil() && !args[0].is_frame() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("framep"), args[0]],
        ));
    }
    if let Some(focus_frame) = args.get(1) {
        if !focus_frame.is_nil() && !focus_frame.is_frame() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *focus_frame],
            ));
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_remove_pos_from_symbol(args: Vec<Value>) -> EvalResult {
    expect_args("remove-pos-from-symbol", &args, 1)?;
    Ok(args[0])
}

pub(crate) fn builtin_resize_mini_window_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("resize-mini-window-internal", &args, 1)?;
    let wid = args[0].as_window_id().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("window-live-p"), args[0]],
        )
    })?;
    let window_id = crate::window::WindowId(wid);
    let fid = eval
        .frames
        .find_window_frame_id(window_id)
        .ok_or_else(|| signal("error", vec![Value::string("Window not found")]))?;
    let frame = eval
        .frames
        .get(fid)
        .ok_or_else(|| signal("error", vec![Value::string("Frame not found")]))?;
    if frame.minibuffer_window != Some(window_id) {
        return Err(signal(
            "error",
            vec![Value::string("Not a minibuffer window")],
        ));
    }
    // The layout engine drives the actual resize via
    // grow_mini_window/shrink_mini_window during redisplay.
    // This Lisp-callable entry point acknowledges the request.
    Ok(Value::NIL)
}

pub(crate) fn builtin_restore_buffer_modified_p(args: Vec<Value>) -> EvalResult {
    expect_args("restore-buffer-modified-p", &args, 1)?;
    Ok(args[0])
}

pub(crate) fn builtin_set_this_command_keys(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set--this-command-keys", &args, 1)?;
    let keys = expect_strict_string(&args[0])?;
    eval.set_this_command_keys_from_string(&keys)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_set_buffer_auto_saved(args: Vec<Value>) -> EvalResult {
    expect_args("set-buffer-auto-saved", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_set_charset_plist(args: Vec<Value>) -> EvalResult {
    expect_args("set-charset-plist", &args, 2)?;
    let name = match args[0].kind() {
        ValueKind::Symbol(id) => id,
        _other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), args[0]],
            ));
        }
    };
    // Parse the plist argument into (key, value) pairs and store it.
    let mut plist_pairs = Vec::new();
    if let Some(items) = list_to_vec(&args[1]) {
        let mut i = 0;
        while i + 1 < items.len() {
            if let Some(key) = items[i].as_symbol_id() {
                plist_pairs.push((key, items[i + 1]));
            }
            i += 2;
        }
    }
    super::charset::set_charset_plist_registry(name, plist_pairs);
    Ok(args[1])
}

pub(crate) fn builtin_set_fontset_font(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("set-fontset-font", &args, 3, 5)?;
    let obarray = eval.obarray();
    let char_script_table =
        dynamic_or_global_symbol_value_in_state(obarray, &[], "char-script-table");
    let charset_script_alist =
        dynamic_or_global_symbol_value_in_state(obarray, &[], "charset-script-alist");
    let font_encoding_alist =
        dynamic_or_global_symbol_value_in_state(obarray, &[], "font-encoding-alist");
    fontset::set_fontset_font(
        &args[0],
        &args[1],
        &args[2],
        args.get(4),
        char_script_table.as_ref(),
        charset_script_alist.as_ref(),
        font_encoding_alist.as_ref(),
    )
}

pub(crate) fn builtin_set_frame_window_state_change(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-frame-window-state-change", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !frame.is_frame() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(Value::NIL)
}

fn is_known_fringe_bitmap(name: &str) -> bool {
    matches!(
        name,
        "empty-line"
            | "horizontal-bar"
            | "vertical-bar"
            | "hollow-square"
            | "filled-square"
            | "hollow-rectangle"
            | "filled-rectangle"
            | "right-bracket"
            | "left-bracket"
            | "bottom-right-angle"
            | "bottom-left-angle"
            | "top-right-angle"
            | "top-left-angle"
            | "right-triangle"
            | "left-triangle"
            | "large-circle"
            | "right-curly-arrow"
            | "left-curly-arrow"
            | "down-arrow"
            | "up-arrow"
            | "right-arrow"
            | "left-arrow"
            | "exclamation-mark"
            | "question-mark"
    )
}

pub(crate) fn builtin_set_fringe_bitmap_face(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-fringe-bitmap-face", &args, 1, 2)?;
    let bitmap = args[0].as_symbol_name();
    if !bitmap.is_some_and(is_known_fringe_bitmap) {
        return Err(signal(
            "error",
            vec![Value::string("Undefined fringe bitmap")],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_set_minibuffer_window(args: Vec<Value>) -> EvalResult {
    expect_args("set-minibuffer-window", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Window)
            if args[0].as_window_id().unwrap() >= crate::window::MINIBUFFER_WINDOW_ID_BASE =>
        {
            Ok(Value::NIL)
        }
        ValueKind::Veclike(VecLikeType::Window) => Err(signal(
            "error",
            vec![Value::string("Window is not a minibuffer window")],
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("windowp"), args[0]],
        )),
    }
}

pub(crate) fn builtin_set_mouse_pixel_position(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-mouse-pixel-position", &args, 3)?;
    let fid = super::window_cmds::resolve_frame_id_in_state(
        &mut eval.frames,
        &mut eval.buffers,
        Some(&args[0]),
        "frame-live-p",
    )?;
    let x = expect_int(&args[1])?;
    let y = expect_int(&args[2])?;
    eval.command_loop
        .keyboard
        .set_mouse_pixel_position(Some(fid), x, y);
    Ok(Value::NIL)
}

pub(crate) fn builtin_set_mouse_position(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-mouse-position", &args, 3)?;
    let fid = super::window_cmds::resolve_frame_id_in_state(
        &mut eval.frames,
        &mut eval.buffers,
        Some(&args[0]),
        "frame-live-p",
    )?;
    let x = expect_int(&args[1])?;
    let y = expect_int(&args[2])?;
    let Some(frame) = eval.frames.get(fid) else {
        return Err(signal("error", vec![Value::string("Frame not found")]));
    };
    let char_width = frame.char_width.max(1.0).round() as i64;
    let char_height = frame.char_height.max(1.0).round() as i64;
    let pixel_x = x.saturating_mul(char_width).saturating_add(char_width / 2);
    let pixel_y = y
        .saturating_mul(char_height)
        .saturating_add(char_height / 2);
    eval.command_loop
        .keyboard
        .set_mouse_pixel_position(Some(fid), pixel_x, pixel_y);
    Ok(Value::NIL)
}

pub(crate) fn builtin_set_window_new_normal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("set-window-new-normal", &args, 1, 2)?;
    expect_window_valid_or_nil(&args[0])?;
    Ok(super::stubs::set_window_new_normal_value(
        eval,
        &args[0],
        args.get(1).cloned().unwrap_or(Value::NIL),
    ))
}

pub(crate) fn builtin_set_window_new_pixel(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("set-window-new-pixel", &args, 2, 3)?;
    expect_window_valid_or_nil(&args[0])?;
    let size = expect_int(&args[1])?;
    Ok(super::stubs::set_window_new_pixel_value(
        eval,
        &args[0],
        size,
        args.get(2).is_some_and(|v| v.is_truthy()),
    ))
}

pub(crate) fn builtin_set_window_new_total(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("set-window-new-total", &args, 2, 3)?;
    expect_window_valid_or_nil(&args[0])?;
    let size = expect_fixnum(&args[1])?;
    Ok(super::stubs::set_window_new_total_value(
        eval,
        &args[0],
        size,
        args.get(2).is_some_and(|v| v.is_truthy()),
    ))
}

pub(crate) fn builtin_sort_charsets(args: Vec<Value>) -> EvalResult {
    expect_args("sort-charsets", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_split_char(args: Vec<Value>) -> EvalResult {
    expect_args("split-char", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_string_distance(args: Vec<Value>) -> EvalResult {
    expect_range_args("string-distance", &args, 2, 3)?;
    let s1 = expect_strict_string(&args[0])?;
    let s2 = expect_strict_string(&args[1])?;
    let bytecomp = args.get(2).is_some_and(|v| v.is_truthy());

    if bytecomp {
        // Byte-level Levenshtein distance
        let b1 = s1.as_bytes();
        let b2 = s2.as_bytes();
        let dist = levenshtein_distance_bytes(b1, b2);
        Ok(Value::fixnum(dist as i64))
    } else {
        // Character-level Levenshtein distance
        let c1: Vec<char> = s1.chars().collect();
        let c2: Vec<char> = s2.chars().collect();
        let dist = levenshtein_distance_chars(&c1, &c2);
        Ok(Value::fixnum(dist as i64))
    }
}

fn levenshtein_distance_chars(a: &[char], b: &[char]) -> usize {
    let m = a.len();
    let n = b.len();
    let mut prev = vec![0usize; n + 1];
    let mut curr = vec![0usize; n + 1];
    for j in 0..=n {
        prev[j] = j;
    }
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

fn levenshtein_distance_bytes(a: &[u8], b: &[u8]) -> usize {
    let m = a.len();
    let n = b.len();
    let mut prev = vec![0usize; n + 1];
    let mut curr = vec![0usize; n + 1];
    for j in 0..=n {
        prev[j] = j;
    }
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

pub(crate) fn builtin_subr_native_comp_unit(args: Vec<Value>) -> EvalResult {
    expect_args("subr-native-comp-unit", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_subr_native_lambda_list(args: Vec<Value>) -> EvalResult {
    expect_args("subr-native-lambda-list", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_subr_type(args: Vec<Value>) -> EvalResult {
    expect_args("subr-type", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_tool_bar_get_system_style(args: Vec<Value>) -> EvalResult {
    expect_args("tool-bar-get-system-style", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_tool_bar_pixel_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("tool-bar-pixel-width", &args, 0, 1)?;
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_translate_region_internal(args: Vec<Value>) -> EvalResult {
    expect_args("translate-region-internal", &args, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_transpose_regions(args: Vec<Value>) -> EvalResult {
    expect_range_args("transpose-regions", &args, 4, 5)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_tty_output_buffer_size(args: Vec<Value>) -> EvalResult {
    expect_range_args("tty--output-buffer-size", &args, 0, 1)?;
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_tty_set_output_buffer_size(args: Vec<Value>) -> EvalResult {
    expect_range_args("tty--set-output-buffer-size", &args, 1, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_tty_suppress_bold_inverse_default_colors(args: Vec<Value>) -> EvalResult {
    expect_args("tty-suppress-bold-inverse-default-colors", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_unencodable_char_position(args: Vec<Value>) -> EvalResult {
    expect_range_args("unencodable-char-position", &args, 3, 5)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_unicode_property_table_internal(args: Vec<Value>) -> EvalResult {
    expect_args("unicode-property-table-internal", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_unify_charset(args: Vec<Value>) -> EvalResult {
    expect_range_args("unify-charset", &args, 1, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_unix_sync(args: Vec<Value>) -> EvalResult {
    expect_args("unix-sync", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_value_lt(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("value<", &args, 2)?;
    match compare_value_lt(eval, &args[0], &args[1])? {
        std::cmp::Ordering::Less => Ok(Value::T),
        _ => Ok(Value::NIL),
    }
}

pub(crate) fn compare_value_lt(
    eval: &crate::emacs_core::eval::Context,
    lhs: &Value,
    rhs: &Value,
) -> Result<std::cmp::Ordering, Flow> {
    compare_value_lt_inner(eval, lhs, rhs, 200)
}

fn compare_value_lt_inner(
    eval: &crate::emacs_core::eval::Context,
    lhs: &Value,
    rhs: &Value,
    maxdepth: i32,
) -> Result<std::cmp::Ordering, Flow> {
    use std::cmp::Ordering;

    if maxdepth < 0 {
        return Err(signal(
            "error",
            vec![Value::string("Maximum depth exceeded in comparison")],
        ));
    }

    if lhs.bits() == rhs.bits() {
        return Ok(Ordering::Equal);
    }

    if let Some(ordering) = compare_number_values_for_value_lt(lhs, rhs) {
        return Ok(ordering);
    }

    if let (Some(left), Some(right)) =
        (symbol_name_for_value_lt(lhs), symbol_name_for_value_lt(rhs))
    {
        return Ok(compare_lisp_strings(left.as_ref(), right.as_ref()));
    }

    match (lhs.kind(), rhs.kind()) {
        (ValueKind::String, ValueKind::String) => Ok(compare_lisp_strings(
            lhs.as_lisp_string().expect("string"),
            rhs.as_lisp_string().expect("string"),
        )),
        (ValueKind::Cons, ValueKind::Cons) => {
            let left_car = lhs.cons_car();
            let left_cdr = lhs.cons_cdr();
            let right_car = rhs.cons_car();
            let right_cdr = rhs.cons_cdr();

            let car_cmp = compare_value_lt_inner(eval, &left_car, &right_car, maxdepth - 1)?;
            if car_cmp != Ordering::Equal {
                return Ok(car_cmp);
            }

            match (left_cdr.kind(), right_cdr.kind()) {
                (ValueKind::Nil, ValueKind::Cons) => Ok(Ordering::Less),
                (ValueKind::Cons, ValueKind::Nil) => Ok(Ordering::Greater),
                _ => compare_value_lt_inner(eval, &left_cdr, &right_cdr, maxdepth - 1),
            }
        }
        (ValueKind::Veclike(left_ty), ValueKind::Veclike(right_ty)) => {
            if left_ty != right_ty {
                return Err(signal_value_lt_type_mismatch(lhs, rhs));
            }

            match left_ty {
                VecLikeType::Vector => match (vector_value_lt_kind(lhs), vector_value_lt_kind(rhs))
                {
                    (VectorValueLtKind::PlainVector, VectorValueLtKind::PlainVector) => {
                        compare_value_sequences(eval, lhs, rhs, maxdepth - 1)
                    }
                    (VectorValueLtKind::BoolVector, VectorValueLtKind::BoolVector) => {
                        compare_bool_vectors_for_value_lt(lhs, rhs)
                    }
                    (VectorValueLtKind::CharTable, VectorValueLtKind::CharTable) => {
                        Ok(Ordering::Equal)
                    }
                    _ => Err(signal_value_lt_type_mismatch(lhs, rhs)),
                },
                VecLikeType::Record => compare_value_sequences(eval, lhs, rhs, maxdepth - 1),
                VecLikeType::Marker => compare_markers_for_value_lt(eval, lhs, rhs),
                VecLikeType::Buffer => Ok(compare_buffers_for_value_lt(eval, lhs, rhs)),
                VecLikeType::Bignum => unreachable!("bignums are handled in compare_number_values"),
                _ => Ok(Ordering::Equal),
            }
        }
        (ValueKind::Unbound, ValueKind::Unbound) | (ValueKind::Unknown, ValueKind::Unknown) => {
            Ok(Ordering::Equal)
        }
        _ => Err(signal_value_lt_type_mismatch(lhs, rhs)),
    }
}

fn signal_value_lt_type_mismatch(lhs: &Value, rhs: &Value) -> Flow {
    signal("type-mismatch", vec![*lhs, *rhs])
}

fn compare_value_sequences(
    eval: &crate::emacs_core::eval::Context,
    lhs: &Value,
    rhs: &Value,
    maxdepth: i32,
) -> Result<std::cmp::Ordering, Flow> {
    use std::cmp::Ordering;

    let left_values = if lhs.is_vector() {
        lhs.as_vector_data().expect("vector")
    } else {
        lhs.as_record_data().expect("record")
    };
    let right_values = if rhs.is_vector() {
        rhs.as_vector_data().expect("vector")
    } else {
        rhs.as_record_data().expect("record")
    };

    for (left, right) in left_values.iter().zip(right_values.iter()) {
        let cmp = compare_value_lt_inner(eval, left, right, maxdepth)?;
        if cmp != Ordering::Equal {
            return Ok(cmp);
        }
    }

    Ok(left_values.len().cmp(&right_values.len()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VectorValueLtKind {
    PlainVector,
    BoolVector,
    CharTable,
}

fn vector_value_lt_kind(value: &Value) -> VectorValueLtKind {
    if crate::emacs_core::chartable::is_bool_vector(value) {
        VectorValueLtKind::BoolVector
    } else if crate::emacs_core::chartable::is_char_table(value) {
        VectorValueLtKind::CharTable
    } else {
        VectorValueLtKind::PlainVector
    }
}

fn compare_bool_vectors_for_value_lt(lhs: &Value, rhs: &Value) -> Result<std::cmp::Ordering, Flow> {
    let left_len = crate::emacs_core::chartable::bool_vector_length(lhs)
        .ok_or_else(|| signal_value_lt_type_mismatch(lhs, rhs))? as usize;
    let right_len = crate::emacs_core::chartable::bool_vector_length(rhs)
        .ok_or_else(|| signal_value_lt_type_mismatch(lhs, rhs))? as usize;
    let left_values = lhs.as_vector_data().expect("bool-vector");
    let right_values = rhs.as_vector_data().expect("bool-vector");
    let min_len = left_len.min(right_len);

    for idx in 0..min_len {
        let left_bit = left_values[2 + idx]
            .as_fixnum()
            .map(|n| n != 0)
            .unwrap_or(false);
        let right_bit = right_values[2 + idx]
            .as_fixnum()
            .map(|n| n != 0)
            .unwrap_or(false);
        if left_bit != right_bit {
            return Ok(left_bit.cmp(&right_bit));
        }
    }

    Ok(left_len.cmp(&right_len))
}

fn compare_markers_for_value_lt(
    eval: &crate::emacs_core::eval::Context,
    lhs: &Value,
    rhs: &Value,
) -> Result<std::cmp::Ordering, Flow> {
    use std::cmp::Ordering;

    let left_buffer = marker_live_buffer_for_value_lt(eval, lhs);
    let right_buffer = marker_live_buffer_for_value_lt(eval, rhs);
    match (left_buffer, right_buffer) {
        (None, Some(_)) => return Ok(Ordering::Less),
        (Some(_), None) => return Ok(Ordering::Greater),
        (Some(left), Some(right)) => {
            let buffer_cmp = compare_buffer_ids_for_value_lt(eval, left, right);
            if buffer_cmp != Ordering::Equal {
                return Ok(buffer_cmp);
            }
        }
        (None, None) => return Ok(Ordering::Equal),
    }

    let left_pos =
        crate::emacs_core::marker::marker_position_as_int_with_buffers(&eval.buffers, lhs)?;
    let right_pos =
        crate::emacs_core::marker::marker_position_as_int_with_buffers(&eval.buffers, rhs)?;
    Ok(left_pos.cmp(&right_pos))
}

fn marker_live_buffer_for_value_lt(
    eval: &crate::emacs_core::eval::Context,
    value: &Value,
) -> Option<crate::buffer::BufferId> {
    let buffer_id = value.as_marker_data()?.buffer?;
    eval.buffers.get(buffer_id)?;
    Some(buffer_id)
}

fn compare_buffers_for_value_lt(
    eval: &crate::emacs_core::eval::Context,
    lhs: &Value,
    rhs: &Value,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (lhs.as_buffer_id(), rhs.as_buffer_id()) {
        (Some(left), Some(right)) => compare_buffer_ids_for_value_lt(eval, left, right),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_buffer_ids_for_value_lt(
    eval: &crate::emacs_core::eval::Context,
    lhs: crate::buffer::BufferId,
    rhs: crate::buffer::BufferId,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let left_name = eval.buffers.get(lhs).map(|buffer| buffer.name.as_str());
    let right_name = eval.buffers.get(rhs).map(|buffer| buffer.name.as_str());
    match (left_name, right_name) {
        (Some(left), Some(right)) => left.cmp(&right),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_lisp_strings(
    lhs: &crate::heap_types::LispString,
    rhs: &crate::heap_types::LispString,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut left_pos = 0;
    let mut right_pos = 0;
    loop {
        match (
            next_lisp_string_char_for_value_lt(lhs, &mut left_pos),
            next_lisp_string_char_for_value_lt(rhs, &mut right_pos),
        ) {
            (Some(left), Some(right)) if left != right => return left.cmp(&right),
            (Some(_), Some(_)) => {}
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => return Ordering::Equal,
        }
    }
}

fn next_lisp_string_char_for_value_lt(
    string: &crate::heap_types::LispString,
    pos: &mut usize,
) -> Option<u32> {
    let bytes = string.as_bytes();
    if *pos >= bytes.len() {
        return None;
    }

    if string.is_multibyte() {
        let (cp, len) = crate::emacs_core::emacs_char::string_char(&bytes[*pos..]);
        *pos += len;
        Some(cp)
    } else {
        let byte = bytes[*pos] as u32;
        *pos += 1;
        Some(byte)
    }
}

fn compare_number_values_for_value_lt(lhs: &Value, rhs: &Value) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;

    if !lhs.is_number() || !rhs.is_number() {
        return None;
    }

    if lhs.is_float() || rhs.is_float() {
        if let Some(big) = lhs.as_bignum() {
            let right = match rhs.kind() {
                ValueKind::Fixnum(n) => n as f64,
                ValueKind::Float => rhs.xfloat(),
                _ => return None,
            };
            return Some(big.partial_cmp(&right).unwrap_or(Ordering::Equal));
        }
        if let Some(big) = rhs.as_bignum() {
            let left = match lhs.kind() {
                ValueKind::Fixnum(n) => n as f64,
                ValueKind::Float => lhs.xfloat(),
                _ => return None,
            };
            return Some(
                big.partial_cmp(&left)
                    .map(|ordering| ordering.reverse())
                    .unwrap_or(Ordering::Equal),
            );
        }
        let left = match lhs.kind() {
            ValueKind::Fixnum(n) => n as f64,
            ValueKind::Float => lhs.xfloat(),
            _ => return None,
        };
        let right = match rhs.kind() {
            ValueKind::Fixnum(n) => n as f64,
            ValueKind::Float => rhs.xfloat(),
            _ => return None,
        };
        return Some(left.partial_cmp(&right).unwrap_or(Ordering::Equal));
    }

    if !lhs.is_bignum() && !rhs.is_bignum() {
        return match (lhs.kind(), rhs.kind()) {
            (ValueKind::Fixnum(left), ValueKind::Fixnum(right)) => Some(left.cmp(&right)),
            _ => None,
        };
    }

    let left = match lhs.kind() {
        ValueKind::Fixnum(n) => rug::Integer::from(n),
        ValueKind::Veclike(VecLikeType::Bignum) => lhs.as_bignum().expect("bignum").clone(),
        _ => return None,
    };
    let right = match rhs.kind() {
        ValueKind::Fixnum(n) => rug::Integer::from(n),
        ValueKind::Veclike(VecLikeType::Bignum) => rhs.as_bignum().expect("bignum").clone(),
        _ => return None,
    };
    Some(left.cmp(&right))
}

fn symbol_name_for_value_lt(
    value: &Value,
) -> Option<std::borrow::Cow<'static, crate::heap_types::LispString>> {
    match value.kind() {
        ValueKind::Nil => Some(std::borrow::Cow::Borrowed(
            crate::emacs_core::intern::resolve_sym_lisp_string(NIL_SYM_ID),
        )),
        ValueKind::T => Some(std::borrow::Cow::Borrowed(
            crate::emacs_core::intern::resolve_sym_lisp_string(T_SYM_ID),
        )),
        ValueKind::Symbol(id) => Some(std::borrow::Cow::Borrowed(
            crate::emacs_core::intern::resolve_sym_lisp_string(id),
        )),
        _ => None,
    }
}

pub(crate) fn builtin_variable_binding_locus(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("variable-binding-locus", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name_in_obarray(&ctx.obarray, name)?;
    if resolved == "nil" || resolved == "t" || resolved.starts_with(':') {
        return Ok(Value::NIL);
    }
    if let Some(buf) = &ctx.buffers.current_buffer() {
        if buf.has_buffer_local(&resolved) {
            return Ok(Value::make_buffer(buf.id));
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_x_begin_drag(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-begin-drag", &args, 1, 6)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_x_double_buffered_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-double-buffered-p", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_x_menu_bar_open_internal(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-menu-bar-open-internal", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_xw_display_color_p_ctx(
    ctx: &crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("xw-display-color-p", &args, 0, 1)?;
    if let Some(display) = args.first() {
        super::super::display::expect_display_designator_in_state(&ctx.frames, display)?;
    }
    if super::super::display::display_window_system_symbol_in_state(
        &ctx.frames,
        &ctx.obarray,
        &[],
        args.first(),
    )?
    .is_some_and(super::super::display::gui_window_system_active_value)
    {
        Ok(Value::T)
    } else {
        Ok(Value::NIL)
    }
}

pub(crate) fn builtin_innermost_minibuffer_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("innermost-minibuffer-p", &args, 0, 1)?;
    Ok(Value::NIL)
}

fn value_list_to_vec(list: &Value) -> Option<Vec<Value>> {
    let mut values = Vec::new();
    let mut cursor = *list;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return Some(values),
            ValueKind::Cons => {
                values.push(cursor.cons_car());
                cursor = cursor.cons_cdr();
            }
            _ => return None,
        }
    }
}

fn value_is_declare_form(value: &Value) -> bool {
    value.is_cons() && value.cons_car().as_symbol_name() == Some("declare")
}

fn interactive_form_from_value_body(body: &[Value]) -> Option<Value> {
    let mut index = 0;
    if body.first().is_some_and(|value| value.is_string()) {
        index = 1;
    }
    while body.get(index).is_some_and(value_is_declare_form) {
        index += 1;
    }

    for form in &body[index..] {
        if !form.is_cons() || form.cons_car().as_symbol_name() != Some("interactive") {
            continue;
        }
        let pair_cdr = form.cons_cdr();
        let spec = if pair_cdr.is_cons() {
            pair_cdr.cons_car()
        } else {
            Value::NIL
        };
        return Some(Value::list(vec![Value::symbol("interactive"), spec]));
    }

    None
}

fn interactive_form_from_stored_closure_spec(spec: Value) -> Value {
    if spec.is_cons() && spec.cons_car().as_symbol_name() == Some("interactive") {
        spec
    } else if spec.is_vector() {
        let items = spec.as_vector_data().cloned().unwrap_or_default();
        let mut list_items = Vec::with_capacity(items.len() + 1);
        list_items.push(Value::symbol("interactive"));
        list_items.extend(items);
        Value::list(list_items)
    } else {
        Value::list(vec![Value::symbol("interactive"), spec])
    }
}

fn interactive_form_from_quoted_interactive_form(form: &Value) -> Result<Option<Value>, Flow> {
    if !form.is_cons() {
        return Ok(None);
    };
    let pair_car = form.cons_car();
    let pair_cdr = form.cons_cdr();
    if pair_car.as_symbol_name() != Some("interactive") {
        return Ok(None);
    }

    match pair_cdr.kind() {
        ValueKind::Nil => Ok(Some(Value::list(vec![
            Value::symbol("interactive"),
            Value::NIL,
        ]))),
        ValueKind::Cons => {
            let arg_pair_car = pair_cdr.cons_car();
            let arg_pair_cdr = pair_cdr.cons_cdr();
            Ok(Some(Value::list(vec![
                Value::symbol("interactive"),
                arg_pair_car,
            ])))
        }
        _tail => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), pair_cdr],
        )),
    }
}

fn interactive_form_from_quoted_lambda(value: &Value) -> Result<Option<Value>, Flow> {
    if !value.is_cons() {
        return Ok(None);
    };
    let lambda_pair_car = value.cons_car();
    let lambda_pair_cdr = value.cons_cdr();
    if lambda_pair_car.as_symbol_name() != Some("lambda") {
        return Ok(None);
    }
    if !lambda_pair_cdr.is_cons() {
        return Ok(None);
    };
    let _params = lambda_pair_cdr.cons_car();
    let body = lambda_pair_cdr.cons_cdr();
    let mut cursor = body;
    let mut can_skip_doc = true;

    loop {
        match cursor.kind() {
            ValueKind::Nil => return Ok(None),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if can_skip_doc && pair_car.is_string() {
                    can_skip_doc = false;
                    cursor = pair_cdr;
                    continue;
                }
                can_skip_doc = false;
                if let Some(interactive) = interactive_form_from_quoted_interactive_form(&pair_car)?
                {
                    return Ok(Some(interactive));
                }
                cursor = pair_cdr;
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), body],
                ));
            }
        }
    }
}

fn interactive_form_from_bytecode_value(function: Value) -> Option<Value> {
    let bc = function.get_bytecode_data()?;
    let spec = bc.interactive;
    spec.map(|s| {
        let spec_val = if s.is_vector() {
            let vec_data = s.as_vector_data().unwrap();
            if !vec_data.is_empty() { vec_data[0] } else { s }
        } else {
            s
        };
        Value::list(vec![Value::symbol("interactive"), spec_val])
    })
}

pub(crate) enum InteractiveFormPlan {
    Return(Value),
    Autoload { fundef: Value, funname: Value },
}

pub(crate) fn plan_interactive_form_in_state(
    obarray: &Obarray,
    interactive: &crate::emacs_core::interactive::InteractiveRegistry,
    cmd: Value,
) -> Result<InteractiveFormPlan, Flow> {
    let mut function = cmd;

    if let Some(mut current) = symbol_id(&cmd) {
        let Some((_, indirect_function)) =
            resolve_indirect_symbol_by_id_in_obarray(obarray, current)
        else {
            return Ok(InteractiveFormPlan::Return(Value::NIL));
        };
        if indirect_function.is_nil() {
            return Ok(InteractiveFormPlan::Return(Value::NIL));
        }

        loop {
            if let Some(property) = obarray
                .get_property_id(current, intern("interactive-form"))
                .copied()
                .filter(|value| !value.is_nil())
            {
                return Ok(InteractiveFormPlan::Return(property));
            }
            let Some(next_function) = symbol_function_cell_in_obarray(obarray, current) else {
                return Ok(InteractiveFormPlan::Return(Value::NIL));
            };
            function = next_function;
            let Some(next_symbol) = symbol_id(&function) else {
                break;
            };
            current = next_symbol;
        }
    }

    match function.kind() {
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = function.as_subr_id().unwrap();
            let name = resolve_sym(id);
            Ok(InteractiveFormPlan::Return(
                crate::emacs_core::interactive::registry_interactive_form(interactive, id)
                    .or_else(|| crate::emacs_core::interactive::builtin_subr_interactive_form(name))
                    .unwrap_or(Value::NIL),
            ))
        }
        ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::Macro) => {
            // GNU Emacs checks closure vector slot 5 first (data.c:1162-1177).
            // Check our dedicated field first, then fall back to body scanning.
            if let Some(iform_val) = function.closure_interactive().flatten() {
                Ok(InteractiveFormPlan::Return(
                    interactive_form_from_stored_closure_spec(iform_val),
                ))
            } else {
                Ok(InteractiveFormPlan::Return(
                    function
                        .closure_body_value()
                        .and_then(|body| value_list_to_vec(&body))
                        .and_then(|body| interactive_form_from_value_body(&body))
                        .unwrap_or(Value::NIL),
                ))
            }
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => Ok(InteractiveFormPlan::Return(
            interactive_form_from_bytecode_value(function).unwrap_or(Value::NIL),
        )),
        ValueKind::Cons if super::autoload::is_autoload_value(&function) => {
            Ok(InteractiveFormPlan::Autoload {
                fundef: function,
                funname: if symbol_id(&cmd).is_some() {
                    cmd
                } else {
                    Value::NIL
                },
            })
        }
        ValueKind::Cons => Ok(InteractiveFormPlan::Return(
            interactive_form_from_quoted_lambda(&function)?.unwrap_or(Value::NIL),
        )),
        _ => Ok(InteractiveFormPlan::Return(Value::NIL)),
    }
}

/// `(interactive-form CMD)` — matching GNU Emacs data.c:1127-1209 exactly.
///
/// Returns (interactive SPEC) or nil.
/// Handles: symbols (with `interactive-form` property), subrs, closures
/// (including oclosures via genfun dispatch), bytecode, autoloads, and
/// quoted lambda forms.
pub(crate) fn builtin_interactive_form(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("interactive-form", &args, 1)?;
    let cmd = args[0];

    // GNU (data.c:1133): Check indirect-function first for nil.
    let indirect = resolve_indirect_symbol(eval, &format!("{}", cmd));
    if indirect.is_none() && cmd.as_symbol_name().is_some() {
        return Ok(Value::NIL);
    }

    // GNU (data.c:1141-1149): Walk symbol chain checking `interactive-form`
    // property on each symbol in the chain.
    let mut fun = cmd;
    let mut genfun = false;
    while let Some(name) = fun.as_symbol_name() {
        if let Some(prop) = eval
            .obarray
            .get_property(name, "interactive-form")
            .copied()
            .filter(|v| !v.is_nil())
        {
            return Ok(prop);
        }
        match symbol_function_cell_in_obarray(&eval.obarray, intern(name)) {
            Some(next) => fun = next,
            None => return Ok(Value::NIL),
        }
    }

    // Now `fun` is the resolved function value (not a symbol).
    match fun.kind() {
        // GNU (data.c:1151-1161): SUBRP
        ValueKind::Veclike(VecLikeType::Subr) => {
            let id = fun.as_subr_id().unwrap();
            let name = resolve_sym(id);
            let result =
                crate::emacs_core::interactive::registry_interactive_form(&eval.interactive, id)
                    .or_else(|| crate::emacs_core::interactive::builtin_subr_interactive_form(name))
                    .unwrap_or(Value::NIL);
            Ok(result)
        }

        // GNU (data.c:1162-1177): CLOSUREP — check slot 5, then genfun
        ValueKind::Veclike(VecLikeType::Lambda) | ValueKind::Veclike(VecLikeType::Macro) => {
            // Check LambdaData.interactive (mirrors closure vector slot 5)
            if let Some(iform_val) = fun.closure_interactive().flatten() {
                return Ok(interactive_form_from_stored_closure_spec(iform_val));
            }

            // Check body for (interactive ...)
            if let Some(body_iform) = fun
                .closure_body_value()
                .and_then(|body| value_list_to_vec(&body))
                .and_then(|body| interactive_form_from_value_body(&body))
            {
                return Ok(body_iform);
            }

            // GNU (data.c:1172-1177): Check for oclosure (non-docstring doc_form)
            if fun.closure_doc_form().flatten().is_some() {
                genfun = true;
            }

            // Fall through to genfun check below
            if genfun {
                // GNU (data.c:1203-1206): Call (oclosure-interactive-form fun)
                // if available (avoid burping during bootstrap).
                // GNU (data.c:1205): "Avoid burping during bootstrap"
                if !eval
                    .obarray
                    .is_function_unbound("oclosure-interactive-form")
                {
                    if let Ok(result) =
                        eval.apply(Value::symbol("oclosure-interactive-form"), vec![fun])
                    {
                        if !result.is_nil() {
                            return Ok(result);
                        }
                    }
                }
            }
            Ok(Value::NIL)
        }

        // GNU (data.c:1162-1177 for COMPILED_FUNCTION_P): bytecode.
        // First check the COMPILED_INTERACTIVE slot. If absent, check
        // the COMPILED_DOC_STRING slot — if it isn't a valid docstring
        // (i.e. not nil and not a plain string), set `genfun = true`
        // and fall through to `oclosure-interactive-form`. nadvice's
        // `:around` / `:before` / `:after` wrappers go through this
        // path: they're bytecode objects whose doc_form holds the
        // `advice` oclosure tag, and `oclosure-interactive-form`
        // dispatches to the cl-defmethod in nadvice.el.
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            if let Some(iform) = interactive_form_from_bytecode_value(fun) {
                return Ok(iform);
            }
            // Bytecode has no interactive slot. Check for an oclosure
            // tag in the doc slot.
            if let Some(bc) = fun.get_bytecode_data()
                && bc.doc_form.is_some()
            {
                genfun = true;
            }
            if genfun
                && !eval
                    .obarray
                    .is_function_unbound("oclosure-interactive-form")
            {
                if let Ok(result) =
                    eval.apply(Value::symbol("oclosure-interactive-form"), vec![fun])
                {
                    if !result.is_nil() {
                        return Ok(result);
                    }
                }
            }
            Ok(Value::NIL)
        }

        // GNU (data.c:1188-1189): autoload → load then retry
        ValueKind::Cons if super::autoload::is_autoload_value(&fun) => {
            let funname = if cmd.as_symbol_name().is_some() {
                cmd
            } else {
                Value::NIL
            };
            let loaded = super::autoload::builtin_autoload_do_load(eval, vec![fun, funname])?;
            // Retry with the loaded definition
            builtin_interactive_form(eval, vec![loaded])
        }

        // GNU (data.c:1190-1202): lambda list (cons starting with `lambda`)
        ValueKind::Cons => Ok(interactive_form_from_quoted_lambda(&fun)?.unwrap_or(Value::NIL)),

        _ => Ok(Value::NIL),
    }
}

/// `(local-variable-if-set-p VARIABLE &optional BUFFER)` — non-nil
/// if VARIABLE either already has a local binding in BUFFER (the
/// `local-variable-p` test) or is automatically buffer-local
/// (`local_if_set` flag set on its BLV).
///
/// Mirrors GNU `src/data.c:2429-2462`. The two non-trivial cases:
///
/// - SYMBOL_LOCALIZED: if `blv->local_if_set` is set, return `t`.
///   Otherwise fall through to `Flocal_variable_p(variable, buffer)`,
///   which checks for an actual binding in BUFFER. Buffer-local
///   audit Medium 5 in `drafts/buffer-local-variables-audit.md`
///   flagged that the BUFFER argument was previously dropped on
///   the floor here, so a per-buffer check always answered against
///   the current buffer.
///
/// - SYMBOL_FORWARDED with BUFFER_OBJFWD: always return `t` per
///   GNU `data.c:2459`, since BUFFER_OBJFWD slots become local
///   automatically when set.
pub(crate) fn builtin_local_variable_if_set_p(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("local-variable-if-set-p", &args, 1, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name_in_obarray(&ctx.obarray, name)?;
    if resolved == "nil" || resolved == "t" || resolved.starts_with(':') {
        return Ok(Value::NIL);
    }

    // Mirror the GNU switch on `sym->u.s.redirect` at
    // src/data.c:2445-2461 exactly. PLAINVAL short-circuits to nil
    // *before* the BUFFER argument is validated, which is what
    // makes `(local-variable-if-set-p 'plain-var (some bad buffer))`
    // legitimately return nil rather than signaling
    // wrong-type-argument.
    use crate::emacs_core::symbol::SymbolRedirect;
    let resolved_id = crate::emacs_core::intern::intern(&resolved);
    let Some(sym) = ctx.obarray.get_by_id(resolved_id) else {
        return Ok(Value::NIL);
    };
    match sym.redirect() {
        SymbolRedirect::Plainval => Ok(Value::NIL),
        SymbolRedirect::Localized => {
            // GNU `if (blv->local_if_set) return Qt;` short circuit.
            if ctx.custom.is_auto_buffer_local_symbol(resolved_id) {
                return Ok(Value::T);
            }
            // Otherwise defer to local-variable-p with BUFFER
            // forwarded so a per-buffer check answers against the
            // requested buffer rather than the current one.
            crate::emacs_core::custom::builtin_local_variable_p(ctx, args)
        }
        SymbolRedirect::Forwarded => {
            // GNU returns Qt unconditionally for BUFFER_OBJFWD
            // slots since they auto-localize on set
            // (data.c:2459).
            Ok(Value::bool_val(ctx.obarray.is_buffer_local(&resolved)))
        }
        _ => Ok(Value::NIL),
    }
}

pub(crate) fn builtin_lock_buffer(args: Vec<Value>) -> EvalResult {
    expect_range_args("lock-buffer", &args, 0, 1)?;
    if let Some(filename) = args.first() {
        if !filename.is_nil() {
            let _ = expect_strict_string(filename)?;
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_lock_file(args: Vec<Value>) -> EvalResult {
    expect_args("lock-file", &args, 1)?;
    let _ = expect_strict_string(&args[0])?;
    Ok(Value::NIL)
}

thread_local! {
    static LOSSAGE_SIZE: RefCell<i64> = RefCell::new(300);
}

pub(super) fn reset_symbols_thread_locals() {
    fontset::reset_fontset_registry();
    LOSSAGE_SIZE.with(|slot| *slot.borrow_mut() = 300);
}

pub(crate) fn builtin_lossage_size(args: Vec<Value>) -> EvalResult {
    expect_range_args("lossage-size", &args, 0, 1)?;

    if let Some(value) = args.first() {
        if !value.is_nil() {
            let n = match value.kind() {
                ValueKind::Fixnum(n) => n,
                _ => {
                    return Err(signal(
                        "user-error",
                        vec![Value::string("Value must be a positive integer")],
                    ));
                }
            };
            if n < 0 {
                return Err(signal(
                    "user-error",
                    vec![Value::string("Value must be a positive integer")],
                ));
            }
            if n < 100 {
                return Err(signal(
                    "user-error",
                    vec![Value::string("Value must be >= 100")],
                ));
            }
            LOSSAGE_SIZE.with(|slot| *slot.borrow_mut() = n);
        }
    }

    Ok(Value::fixnum(LOSSAGE_SIZE.with(|slot| *slot.borrow())))
}

pub(crate) fn builtin_unlock_buffer(args: Vec<Value>) -> EvalResult {
    expect_args("unlock-buffer", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_unlock_file(args: Vec<Value>) -> EvalResult {
    expect_args("unlock-file", &args, 1)?;
    let _ = expect_strict_string(&args[0])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_internal_track_mouse(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal--track-mouse", &args, 1)?;
    let specpdl_count = ctx.specpdl.len();
    ctx.specbind(intern("track-mouse"), Value::T);
    let result = ctx.apply(args[0], vec![]);
    ctx.unbind_to(specpdl_count);
    result
}

pub(crate) fn builtin_internal_char_font(args: Vec<Value>) -> EvalResult {
    expect_range_args("internal-char-font", &args, 1, 2)?;
    let position = &args[0];
    let ch = args.get(1).copied().unwrap_or(Value::NIL);

    if position.is_nil() {
        let _ = expect_character_code(&ch)?;
        return Ok(Value::NIL);
    }

    let _ = expect_integer_or_marker(position)?;
    if !ch.is_nil() {
        let _ = expect_character_code(&ch)?;
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_internal_complete_buffer(args: Vec<Value>) -> EvalResult {
    expect_args("internal-complete-buffer", &args, 3)?;
    let _ = expect_strict_string(&args[0])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_internal_describe_syntax_value(args: Vec<Value>) -> EvalResult {
    expect_args("internal-describe-syntax-value", &args, 1)?;
    Ok(args[0])
}

pub(crate) fn builtin_internal_event_symbol_parse_modifiers(args: Vec<Value>) -> EvalResult {
    expect_args("internal-event-symbol-parse-modifiers", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;

    // Parse GNU-compatible modifiers including multi-letter ones
    // (down-, double-, triple-, drag-) and implicit click for mouse events.
    let (modifiers_bits, base) = parse_event_modifiers_gnu(name);
    let mut out = vec![Value::symbol(base)];
    // Build modifier list in GNU's canonical order
    for (bit, sym) in [
        (1 << 0, "meta"),
        (1 << 1, "control"),
        (1 << 2, "shift"),
        (1 << 3, "hyper"),
        (1 << 4, "super"),
        (1 << 5, "alt"),
        (1 << 6, "click"),
        (1 << 7, "down"),
        (1 << 8, "drag"),
        (1 << 9, "double"),
        (1 << 10, "triple"),
        (1 << 11, "up"),
    ] {
        if modifiers_bits & bit != 0 {
            out.push(Value::symbol(sym));
        }
    }
    Ok(Value::list(out))
}

/// Parse event symbol modifiers matching GNU keyboard.c logic.
/// Returns (modifier_bitmask, base_event_name).
fn parse_event_modifiers_gnu(name: &str) -> (u32, &str) {
    let mut bits: u32 = 0;
    let mut rest = name;

    loop {
        if let Some(r) = rest.strip_prefix("M-") {
            bits |= 1 << 0;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("C-") {
            bits |= 1 << 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("S-") {
            bits |= 1 << 2;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("H-") {
            bits |= 1 << 3;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("s-") {
            bits |= 1 << 4;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("A-") {
            bits |= 1 << 5;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("down-") {
            bits |= 1 << 7;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("drag-") {
            bits |= 1 << 8;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("double-") {
            bits |= 1 << 9;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("triple-") {
            bits |= 1 << 10;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("up-") {
            bits |= 1 << 11;
            rest = r;
        } else {
            break;
        }
    }

    // GNU: mouse-N events without down/drag/up get implicit click
    if rest.starts_with("mouse-") && (bits & ((1 << 7) | (1 << 8) | (1 << 11))) == 0 {
        bits |= 1 << 6; // click
    }

    (bits, rest)
}

pub(crate) fn builtin_internal_handle_focus_in(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal-handle-focus-in", &args, 1)?;
    if !args[0].is_cons() {
        return Err(signal(
            "error",
            vec![Value::string("invalid focus-in event")],
        ));
    };
    let pair_car = args[0].cons_car();
    let pair_cdr = args[0].cons_cdr();
    if pair_car.as_symbol_name() != Some("focus-in") {
        return Err(signal(
            "error",
            vec![Value::string("invalid focus-in event")],
        ));
    }
    if !pair_cdr.is_cons() {
        return Err(signal(
            "error",
            vec![Value::string("invalid focus-in event")],
        ));
    };
    let frame_value = pair_cdr.cons_car();
    if !frame_value.is_frame() {
        return Err(signal(
            "error",
            vec![Value::string("invalid focus-in event")],
        ));
    };

    let frame_id = crate::window::FrameId(frame_value.as_frame_id().unwrap());
    if let Some(frame) = eval.frames.get(frame_id) {
        eval.command_loop
            .keyboard
            .select_terminal(frame.terminal_id);
    }
    let selected_frame = eval.frames.selected_frame().map(|frame| frame.id);
    let last_event_frame = eval
        .command_loop
        .keyboard
        .kboard
        .internal_last_event_frame();
    let switching = Some(frame_id) != last_event_frame && Some(frame_id) != selected_frame;

    eval.command_loop
        .keyboard
        .kboard
        .set_internal_last_event_frame(frame_id);

    // GNU `kbd_buffer_get_event` (`src/keyboard.c:4033-4045`)
    // assigns Vlast_event_frame whenever the frame of the
    // current event is known. We mirror that here at the
    // focus-in entry point and via the standard event ingest
    // path. Keyboard audit Finding 8 in
    // `drafts/keyboard-command-loop-audit.md`.
    eval.obarray
        .set_symbol_value("last-event-frame", frame_value);

    if switching
        || eval
            .command_loop
            .keyboard
            .kboard
            .unread_selection_event
            .is_some()
    {
        eval.command_loop
            .keyboard
            .kboard
            .set_unread_selection_event(Value::list(vec![
                Value::symbol("switch-frame"),
                frame_value,
            ]));
    }

    Ok(Value::NIL)
}

pub(crate) fn builtin_internal_make_var_non_special(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal-make-var-non-special", &args, 1)?;
    let obarray = eval.obarray_mut();
    let symbol = expect_symbol_id(&args[0])?;
    obarray.make_non_special_id(symbol);
    Ok(Value::NIL)
}

pub(crate) fn builtin_internal_set_lisp_face_attribute_from_resource(
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args(
        "internal-set-lisp-face-attribute-from-resource",
        &args,
        3,
        4,
    )?;
    if args[0].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    let resource_value = expect_strict_string(&args[2])?;

    const VALID_X_RESOURCE_FACE_ATTRIBUTES: &[&str] = &[
        ":family",
        ":foundry",
        ":height",
        ":weight",
        ":slant",
        ":underline",
        ":overline",
        ":strike-through",
        ":box",
        ":inverse-video",
        ":foreground",
        ":distant-foreground",
        ":background",
        ":stipple",
        ":width",
        ":inherit",
        ":extend",
        ":font",
        ":fontset",
        ":bold",
        ":italic",
    ];

    let attr_name = match args[1].kind() {
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        ValueKind::Nil | ValueKind::T => args[1].as_symbol_name().unwrap_or_default().to_string(),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[1]],
            ));
        }
    };
    if !VALID_X_RESOURCE_FACE_ATTRIBUTES.contains(&attr_name.as_str()) {
        if args[1].is_nil() {
            return Err(signal(
                "error",
                vec![Value::string("Invalid face attribute name")],
            ));
        }
        return Err(signal(
            "error",
            vec![Value::string("Invalid face attribute name"), args[1]],
        ));
    }

    const VALID_FACE_WEIGHTS: &[&str] = &[
        "ultra-light",
        "extra-light",
        "light",
        "semi-light",
        "normal",
        "semi-bold",
        "bold",
        "extra-bold",
        "ultra-bold",
    ];
    const VALID_FACE_SLANTS: &[&str] = &[
        "normal",
        "italic",
        "oblique",
        "reverse-italic",
        "reverse-oblique",
    ];
    const VALID_FACE_WIDTHS: &[&str] = &[
        "ultra-condensed",
        "extra-condensed",
        "condensed",
        "semi-condensed",
        "normal",
        "semi-expanded",
        "expanded",
        "extra-expanded",
        "ultra-expanded",
    ];

    let value_lc = resource_value.to_ascii_lowercase();
    match attr_name.as_str() {
        ":width" if !VALID_FACE_WIDTHS.contains(&value_lc.as_str()) => {
            return Err(signal(
                "error",
                vec![
                    Value::string("Invalid face width"),
                    Value::symbol(resource_value),
                ],
            ));
        }
        ":weight" if !VALID_FACE_WEIGHTS.contains(&value_lc.as_str()) => {
            return Err(signal(
                "error",
                vec![
                    Value::string("Invalid face weight"),
                    Value::symbol(resource_value),
                ],
            ));
        }
        ":slant" if !VALID_FACE_SLANTS.contains(&value_lc.as_str()) => {
            return Err(signal(
                "error",
                vec![
                    Value::string("Invalid face slant"),
                    Value::symbol(resource_value),
                ],
            ));
        }
        ":box" if resource_value != "nil" && resource_value != "t" => {
            return Err(signal(
                "error",
                vec![
                    Value::string("Invalid face box"),
                    Value::symbol(resource_value),
                ],
            ));
        }
        ":inverse-video" | ":extend" | ":bold" | ":italic"
            if value_lc != "on"
                && value_lc != "off"
                && value_lc != "true"
                && value_lc != "false" =>
        {
            return Err(signal(
                "error",
                vec![
                    Value::string("Invalid face attribute value from X resource"),
                    Value::string(resource_value),
                ],
            ));
        }
        _ => {}
    }

    Ok(args[0])
}

pub(crate) fn builtin_internal_stack_stats(args: Vec<Value>) -> EvalResult {
    expect_args("internal-stack-stats", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_internal_subr_documentation(args: Vec<Value>) -> EvalResult {
    expect_args("internal-subr-documentation", &args, 1)?;
    // Mirrors GNU `Fsubr_documentation' (`src/doc.c:383-400'). GNU
    // returns a fixnum byte offset into etc/DOC; neomacs stores docs
    // inline in `subr_docs::GNU_SUBR_DOCS' so we return the literal
    // string. Returns `t' (the GNU sentinel for "invalid function")
    // when the value isn't a subr at all -- the cl-defgeneric
    // `function-documentation' caller checks for `t' and signals
    // `invalid-function'.
    let func = args[0];
    let Some(id) = func.as_subr_id() else {
        return Ok(Value::T);
    };
    let name = resolve_sym(id);
    Ok(super::super::subr_docs::lookup(name)
        .map(Value::string)
        .unwrap_or(Value::NIL))
}

pub(crate) fn builtin_malloc_info(args: Vec<Value>) -> EvalResult {
    expect_args("malloc-info", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_malloc_trim(args: Vec<Value>) -> EvalResult {
    expect_range_args("malloc-trim", &args, 0, 1)?;
    if let Some(pad) = args.first() {
        if !pad.is_nil() {
            let _ = expect_wholenump(pad)?;
        }
    }
    Ok(Value::T)
}

pub(crate) fn builtin_memory_info(args: Vec<Value>) -> EvalResult {
    expect_args("memory-info", &args, 0)?;
    let counts = Value::memory_use_counts_snapshot();
    Ok(Value::list(vec![
        Value::fixnum(counts[0]),
        Value::fixnum(counts[1]),
        Value::fixnum(counts[2]),
        Value::fixnum(counts[3]),
    ]))
}

pub(crate) fn builtin_module_load(args: Vec<Value>) -> EvalResult {
    expect_args("module-load", &args, 1)?;
    let path = expect_strict_string(&args[0])?;

    let lib = unsafe { libloading::Library::new(&path) }.map_err(|e| {
        signal(
            "module-open-failed",
            vec![Value::string(path.clone()), Value::string(e.to_string())],
        )
    })?;

    // Check for GPL compatibility symbol
    let has_gpl: Result<libloading::Symbol<*const ()>, _> =
        unsafe { lib.get(b"plugin_is_GPL_compatible") };
    if has_gpl.is_err() {
        drop(lib);
        return Err(signal(
            "module-not-gpl-compatible",
            vec![Value::string(path)],
        ));
    }

    drop(lib);
    Ok(Value::T)
}

pub(crate) fn builtin_dump_emacs_portable(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("dump-emacs-portable", &args, 1, 2)?;

    if !ctx.noninteractive() {
        return Err(signal(
            "error",
            vec![Value::string(
                "Dumping Emacs currently works only in batch mode.  If you'd like it to work interactively, please consider contributing a patch to Emacs.",
            )],
        ));
    }
    if ctx.threads.current_thread_id() != 0 {
        return Err(signal(
            "error",
            vec![Value::string(
                "This function can be called only in the main thread",
            )],
        ));
    }
    if ctx.threads.all_thread_ids().into_iter().any(|id| id != 0) {
        return Err(signal(
            "error",
            vec![Value::string(
                "No other Lisp threads can be running when this function is called",
            )],
        ));
    }

    let path = expect_strict_string(&args[0])?;
    let expanded_path = crate::emacs_core::fileio::expand_file_name(
        &path,
        crate::emacs_core::fileio::default_directory_in_state(&ctx.obarray, &[], &ctx.buffers)
            .as_deref(),
    );
    let dump_path = std::path::Path::new(&expanded_path);
    let saved_post_gc_hook = ctx
        .obarray()
        .symbol_value("post-gc-hook")
        .copied()
        .unwrap_or(Value::NIL);
    let saved_command_line_processed = ctx
        .obarray()
        .symbol_value("command-line-processed")
        .copied()
        .unwrap_or(Value::NIL);
    let saved_process_environment = ctx
        .obarray()
        .symbol_value("process-environment")
        .copied()
        .unwrap_or(Value::NIL);
    ctx.set_variable("post-gc-hook", Value::NIL);
    ctx.gc_collect_exact();
    ctx.set_variable("command-line-processed", Value::NIL);
    ctx.set_variable("process-environment", Value::NIL);
    let dump_result = crate::emacs_core::pdump::dump_to_file(ctx, dump_path);
    ctx.set_variable("post-gc-hook", saved_post_gc_hook);
    ctx.set_variable("command-line-processed", saved_command_line_processed);
    ctx.set_variable("process-environment", saved_process_environment);

    dump_result.map_err(|err| {
        signal(
            "file-error",
            vec![
                Value::string("dump-emacs-portable"),
                Value::string(expanded_path),
                Value::string(err.to_string()),
            ],
        )
    })?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_dump_emacs_portable_sort_predicate(args: Vec<Value>) -> EvalResult {
    expect_args("dump-emacs-portable--sort-predicate", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_dump_emacs_portable_sort_predicate_copied(args: Vec<Value>) -> EvalResult {
    expect_args("dump-emacs-portable--sort-predicate-copied", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_byte_code(args: Vec<Value>) -> EvalResult {
    expect_args("byte-code", &args, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_decode_coding_region(args: Vec<Value>) -> EvalResult {
    expect_range_args("decode-coding-region", &args, 3, 4)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_encode_coding_region(args: Vec<Value>) -> EvalResult {
    expect_range_args("encode-coding-region", &args, 3, 4)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_find_operation_coding_system(args: Vec<Value>) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("find-operation-coding-system"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_handler_bind_1(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("handler-bind-1"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    if args.len() % 2 == 0 {
        return Err(signal(
            "error",
            vec![Value::string(
                "Trailing CONDITIONS without HANDLER in `handler-bind`",
            )],
        ));
    }

    let scope = eval.open_gc_scope();
    for value in &args {
        eval.push_temp_root(*value);
    }

    let bodyfun = args[0];
    let handlers: Vec<(Value, Value)> = args[1..]
        .chunks_exact(2)
        .filter_map(|pair| (!pair[0].is_nil()).then_some((pair[0], pair[1])))
        .collect();

    let condition_stack_base = eval.condition_stack_len();
    for (mute_span, (conditions, handler)) in handlers.iter().rev().enumerate() {
        eval.push_condition_frame(super::eval::ConditionFrame::HandlerBind {
            conditions: *conditions,
            handler: *handler,
            mute_span,
        });
    }

    let body_result = eval.apply(bodyfun, vec![]);
    eval.truncate_condition_stack(condition_stack_base);
    scope.close(eval);
    body_result
}

pub(crate) fn builtin_iso_charset(args: Vec<Value>) -> EvalResult {
    expect_args("iso-charset", &args, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_keymap_get_keyelt(args: Vec<Value>) -> EvalResult {
    expect_args("keymap--get-keyelt", &args, 2)?;
    Ok(args[0])
}

pub(crate) fn builtin_keymap_prompt(args: Vec<Value>) -> EvalResult {
    expect_args("keymap-prompt", &args, 1)?;
    let map = args[0];
    // A keymap is (keymap [PROMPT] . BINDINGS).
    // If the arg is a cons whose car is the symbol `keymap`, check if cadr is a string.
    if map.is_cons() {
        let car = map.cons_car();
        if car.is_symbol_named("keymap") {
            let cdr = map.cons_cdr();
            if cdr.is_cons() {
                let cadr = cdr.cons_car();
                if cadr.is_string() {
                    return Ok(cadr);
                }
            }
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn plan_kill_emacs_request(
    args: &[Value],
) -> Result<super::eval::ShutdownRequest, Flow> {
    expect_range_args("kill-emacs", args, 0, 2)?;
    let exit_code = match args.first().copied().unwrap_or(Value::NIL).kind() {
        ValueKind::Fixnum(n) => n as i32,
        ValueKind::Nil | ValueKind::T => 0,
        _ => 0,
    };
    let restart = args.get(1).is_some_and(|v| v.is_truthy());
    Ok(super::eval::ShutdownRequest { exit_code, restart })
}

pub(crate) fn builtin_kill_emacs(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let request = plan_kill_emacs_request(&args)?;
    let _ = eval.run_hook_if_bound("kill-emacs-hook");
    eval.request_shutdown(request.exit_code, request.restart);
    Err(crate::emacs_core::error::signal_suppressed(
        "kill-emacs",
        vec![],
    ))
}

pub(crate) fn builtin_lower_frame(args: Vec<Value>) -> EvalResult {
    expect_range_args("lower-frame", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_lread_substitute_object_in_subtree(args: Vec<Value>) -> EvalResult {
    expect_args("lread--substitute-object-in-subtree", &args, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_make_byte_code(args: Vec<Value>) -> EvalResult {
    if args.len() < 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-byte-code"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    make_byte_code_from_parts(
        &args[0],
        &args[1],
        &args[2],
        &args[3],
        args.get(4),
        args.get(5),
    )
}

/// Core logic for constructing a `Value::ByteCode` from GNU-style parts.
/// Used by both `make-byte-code` builtin and `sf_byte_code_literal`.
pub(crate) fn make_byte_code_from_parts(
    arglist: &Value,
    bytecode_str: &Value,
    constants_vec: &Value,
    maxdepth: &Value,
    docstring: Option<&Value>,
    interactive: Option<&Value>,
) -> EvalResult {
    use crate::emacs_core::bytecode::ByteCodeFunction;
    use crate::emacs_core::bytecode::decode::{
        decode_gnu_bytecode_with_offset_map, parse_arglist_value, string_value_to_bytes,
    };

    // 1. Parse arglist
    let params = parse_arglist_value(arglist);

    // 2. Extract raw bytes from bytecode string
    let raw_bytes = if let Some(s) = bytecode_str.as_str() {
        string_value_to_bytes(s)
    } else {
        // Could be nil for empty functions
        Vec::new()
    };

    // 3. Extract constants from vector
    let mut constants: Vec<Value> = match constants_vec.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => constants_vec.as_vector_data().unwrap().clone(),
        _ => Vec::new(),
    };

    // 3b. Reify compiled literals embedded in the constants vector.
    // GNU `.elc` constants may contain nested `#[...]` bytecode objects or
    // `#s(hash-table ...)` literals. Convert them into real runtime objects
    // before decoding/executing the bytecode.
    for i in 0..constants.len() {
        constants[i] = try_convert_nested_compiled_literal(constants[i]);
    }

    // 4. Decode GNU bytecodes
    let (ops, gnu_byte_offset_map) =
        decode_gnu_bytecode_with_offset_map(&raw_bytes, &mut constants).map_err(|e| {
            signal(
                "error",
                vec![Value::string(format!("bytecode decode error: {}", e))],
            )
        })?;

    // 5. Extract maxdepth
    let max_stack = match maxdepth.kind() {
        ValueKind::Fixnum(n) => n as u16,
        _ => 16, // fallback
    };

    // 6. Extract closure slot 4.
    // GNU byte-code objects use this slot for either a docstring or an
    // arbitrary documentation form, notably the oclosure type symbol.
    let (doc, doc_form) = match docstring.copied() {
        Some(v) if v.is_string() => (
            Some(
                v.as_lisp_string()
                    .expect("ValueKind::String must carry LispString payload")
                    .clone(),
            ),
            None,
        ),
        Some(v) if !v.is_nil() => (None, Some(v)),
        _ => (None, None),
    };

    // 7. Build ByteCodeFunction
    let bc = ByteCodeFunction {
        ops,
        constants,
        max_stack,
        params,
        lexical: false,
        env: None,
        gnu_byte_offset_map: Some(gnu_byte_offset_map),
        // Preserve original GNU-format bytes so `(aref FN 1)` returns the
        // bytecode string.  Required for `byte-compile-make-closure` which
        // reads the bytes via aref and passes them back to `make-byte-code`
        // when generating closure prototypes.
        gnu_bytecode_bytes: if raw_bytes.is_empty() {
            None
        } else {
            Some(raw_bytes)
        },
        docstring: doc,
        doc_form,
        // GNU Emacs (eval.c:2301-2303): "Bytecode objects are interactive if
        // they are long enough to have an element where the interactive spec
        // is stored."  The mere PRESENCE of the slot (even if nil) means the
        // function is interactive.  We mirror this: if the caller provided an
        // interactive argument at all (even nil), store Some(value).
        interactive: interactive.copied(),
    };

    Ok(Value::make_bytecode(bc))
}

pub(crate) fn make_interpreted_closure_from_parts(
    params_value: &Value,
    body_value: &Value,
    env_value: &Value,
    docstring: Option<&Value>,
    interactive: Option<&Value>,
) -> EvalResult {
    let docstring_value = docstring.copied().unwrap_or(Value::NIL);
    let iform = interactive.copied().unwrap_or(Value::NIL);

    parse_lambda_params_from_value(params_value)?;
    if !body_value.is_nil() && list_to_vec(body_value).is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *body_value],
        ));
    }

    let (docstring, doc_form) = match docstring_value.kind() {
        ValueKind::String => (
            Some(
                docstring_value
                    .as_lisp_string()
                    .expect("ValueKind::String must carry LispString payload")
                    .clone(),
            ),
            None,
        ),
        ValueKind::Nil => (None, None),
        _other => (None, Some(docstring_value)),
    };

    // GNU Emacs (eval.c:535-555): Fmake_interpreted_closure stores the
    // interactive spec in slot 5 of the closure vector.  We mirror this
    // by storing it in LambdaData.interactive.
    //
    // GNU processes iform: (interactive SPEC) → SPEC (single arg)
    //                      (interactive S1 S2 ...) → #[S1 S2 ...]  (vector)
    let interactive_spec = if iform.is_nil() {
        None
    } else if let Some(items) = list_to_vec(&iform) {
        if items.len() >= 2 && items[0].as_symbol_name() == Some("interactive") {
            let ifcdr = &items[1..];
            if ifcdr.len() == 1 {
                Some(ifcdr[0])
            } else {
                Some(Value::vector(ifcdr.to_vec()))
            }
        } else {
            Some(iform)
        }
    } else {
        Some(iform)
    };

    // Store GNU closure slots directly so interpreted closures do not pay a
    // Value -> Expr -> Value round-trip for their runtime bodies.
    Ok(Value::make_lambda_with_slots(vec![
        *params_value,
        *body_value,
        *env_value,
        Value::NIL,
        doc_form
            .or_else(|| docstring.as_ref().map(|d| Value::heap_string(d.clone())))
            .unwrap_or(Value::NIL),
        interactive_spec.unwrap_or(Value::NIL),
    ]))
}

/// Reify nested compiled literals embedded in `.elc` constant vectors.
///
/// GNU compiled constants are first read as ordinary Lisp data. Nested
/// `#[...]` functions arrive as vectors and nested `#s(hash-table ...)`
/// literals arrive as `(make-hash-table-from-literal '(...))` forms. This
/// pass turns them back into actual runtime objects before bytecode decode.
pub(crate) fn try_convert_nested_compiled_literal(val: Value) -> Value {
    if let Some(table) = try_convert_hash_table_literal(val) {
        return table;
    }

    let items = match val.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let v = val.as_vector_data().unwrap().clone();
            if v.len() < 3 {
                return val;
            }
            v
        }
        _ => return val,
    };

    if items.len() >= 4 && items[1].is_string() && (items[2].is_vector() || items[2].is_nil()) {
        return match make_byte_code_from_parts(
            &items[0],
            &items[1],
            &items[2],
            &items[3],
            items.get(4),
            items.get(5),
        ) {
            Ok(bc) => bc,
            Err(_) => val,
        };
    }

    let looks_interpreted_closure = matches!(items.len(), 3 | 5 | 6)
        && (items[0].is_cons() || items[0].is_nil())
        && items[1].is_cons()
        && (items.len() < 4 || items[3].is_nil());
    if !looks_interpreted_closure {
        return val;
    }

    match make_interpreted_closure_from_parts(
        &items[0],
        &items[1],
        &items[2],
        items.get(4),
        items.get(5),
    ) {
        Ok(lambda) => lambda,
        Err(_) => val,
    }
}

fn try_convert_hash_table_literal(val: Value) -> Option<Value> {
    let form = list_to_vec(&val)?;
    if form.len() != 2 {
        return None;
    }
    let head = form[0].as_symbol_name()?;
    if head != "make-hash-table-from-literal" {
        return None;
    }

    let payload = quote_payload_value(form[1])?;
    let spec = list_to_vec(&payload)?;
    if spec.first()?.as_symbol_name()? != "hash-table" {
        return None;
    }

    let mut test = HashTableTest::Eql;
    let mut test_name: Option<SymId> = None;
    let mut size = 0_i64;
    let mut weakness: Option<HashTableWeakness> = None;
    let mut rehash_size = 1.5_f64;
    let mut rehash_threshold = 0.8125_f64;
    let mut data_value: Option<Value> = None;

    let mut i = 1_usize;
    while i + 1 < spec.len() {
        let key = spec[i].as_symbol_name()?;
        let value = spec[i + 1];
        match key {
            "size" => size = value.as_int()?,
            "test" => {
                let name = value.as_symbol_name()?;
                test = match name {
                    "eq" => HashTableTest::Eq,
                    "eql" => HashTableTest::Eql,
                    "equal" => HashTableTest::Equal,
                    _ => return None,
                };
                test_name = Some(intern(name));
            }
            "weakness" => {
                weakness = match value.as_symbol_name() {
                    Some("key") => Some(HashTableWeakness::Key),
                    Some("value") => Some(HashTableWeakness::Value),
                    Some("key-or-value") => Some(HashTableWeakness::KeyOrValue),
                    Some("key-and-value") => Some(HashTableWeakness::KeyAndValue),
                    Some("nil") | None => None,
                    _ => return None,
                };
            }
            "rehash-size" => {
                rehash_size = value.as_float().unwrap_or(value.as_int()? as f64);
            }
            "rehash-threshold" => {
                rehash_threshold = value.as_float().unwrap_or(value.as_int()? as f64);
            }
            "data" => data_value = Some(value),
            _ => {}
        }
        i += 2;
    }

    let table_value =
        Value::hash_table_with_options(test, size, weakness, rehash_size, rehash_threshold);
    if !table_value.is_hash_table() {
        return None;
    };

    {
        let _ = table_value.with_hash_table_mut(|table| {
            table.test_name = test_name;
            if let Some(data) = data_value.and_then(|value| list_to_vec(&value)) {
                let mut idx = 0_usize;
                while idx + 1 < data.len() {
                    let key_value = try_convert_nested_compiled_literal(data[idx]);
                    let val_value = try_convert_nested_compiled_literal(data[idx + 1]);
                    let key = key_value.to_hash_key(&table.test);
                    let inserting_new_key = !table.data.contains_key(&key);
                    table.data.insert(key.clone(), val_value);
                    if inserting_new_key {
                        table.key_snapshots.insert(key.clone(), key_value);
                        table.insertion_order.push(key);
                    }
                    idx += 2;
                }
            }
        });
    }

    Some(table_value)
}

fn quote_payload_value(value: Value) -> Option<Value> {
    let items = list_to_vec(&value)?;
    if items.len() != 2 {
        return None;
    }
    match items[0].as_symbol_name() {
        Some("quote") => Some(items[1]),
        _ => None,
    }
}

pub(crate) fn builtin_make_char(args: Vec<Value>) -> EvalResult {
    expect_range_args("make-char", &args, 1, 5)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_make_closure(args: Vec<Value>) -> EvalResult {
    // (make-closure PROTOTYPE &rest CLOSURE-VARS)
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-closure"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let prototype = &args[0];
    let closure_vars = &args[1..];

    let bc = prototype
        .get_bytecode_data()
        .ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("byte-code-function-p"), args[0]],
            )
        })?
        .clone();

    let mut new_bc = bc;

    if let Some(env_val) = new_bc.env {
        // NeoVM-compiled: replace first N values in env alist
        new_bc.env = Some(replace_env_alist_values(env_val, closure_vars));
    } else {
        // GNU .elc: replace first N entries in constants vector
        if closure_vars.len() > new_bc.constants.len() {
            return Err(signal(
                "error",
                vec![Value::string("Closure vars do not fit in constvec")],
            ));
        }
        for (i, var) in closure_vars.iter().enumerate() {
            new_bc.constants[i] = *var;
        }
    }

    Ok(Value::make_bytecode(new_bc))
}

/// Replace the first N values in a cons alist with closure_vars.
/// Walk env alist and closure_vars in parallel. For the first N entries,
/// create new (sym . new_val) cons pairs. Share the remaining tail unchanged.
fn replace_env_alist_values(env: Value, closure_vars: &[Value]) -> Value {
    if closure_vars.is_empty() {
        return env;
    }

    // Collect alist entries
    let entries = match list_to_vec(&env) {
        Some(v) => v,
        None => return env,
    };

    let mut result_entries = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        if i < closure_vars.len() {
            // Replace value: get the key from (key . old_val), make (key . new_val)
            let key = match entry.kind() {
                ValueKind::Cons => entry.cons_car(),
                _ => *entry, // shouldn't happen in well-formed alist
            };
            result_entries.push(Value::cons(key, closure_vars[i]));
        } else {
            // Share remaining entries unchanged
            result_entries.push(*entry);
        }
    }

    Value::list(result_entries)
}

pub(crate) fn builtin_make_finalizer(args: Vec<Value>) -> EvalResult {
    expect_args("make-finalizer", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_make_interpreted_closure(args: Vec<Value>) -> EvalResult {
    expect_range_args("make-interpreted-closure", &args, 3, 5)?;
    make_interpreted_closure_from_parts(&args[0], &args[1], &args[2], args.get(3), args.get(4))
}
