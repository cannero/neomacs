use super::*;
use crate::emacs_core::fontset;
use crate::emacs_core::intern::lookup_interned;
use crate::emacs_core::symbol::Obarray;

// ===========================================================================
// Symbol operations (need evaluator for obarray access)
// ===========================================================================

pub(crate) const VARIABLE_ALIAS_PROPERTY: &str = "neovm--variable-alias";
pub(crate) const RAW_SYMBOL_PLIST_PROPERTY: &str = "neovm--raw-symbol-plist";

pub(crate) fn is_internal_symbol_plist_property(property: &str) -> bool {
    property == VARIABLE_ALIAS_PROPERTY || property == RAW_SYMBOL_PLIST_PROPERTY
}

pub(crate) fn symbol_id(value: &Value) -> Option<SymId> {
    match value {
        Value::Nil => Some(intern("nil")),
        Value::True => Some(intern("t")),
        Value::Symbol(id) | Value::Keyword(id) => Some(*id),
        _ => None,
    }
}

fn value_from_symbol_id(id: SymId) -> Value {
    let name = resolve_sym(id);
    if lookup_interned(name).is_some_and(|canonical| canonical == id) {
        if name == "nil" {
            return Value::Nil;
        }
        if name == "t" {
            return Value::True;
        }
        if name.starts_with(':') {
            return Value::Keyword(id);
        }
    }
    Value::Symbol(id)
}

pub(crate) trait MacroexpandRuntime {
    fn next_pcase_macroexpand_temp_symbol(&mut self) -> Value;
    fn resolve_indirect_symbol_by_id(&self, symbol: SymId) -> Option<(SymId, Value)>;
    fn is_global_function_placeholder(&self, symbol: SymId) -> bool;
    fn autoload_do_load_macro(&mut self, autoload: Value, head: Value) -> Result<(), Flow>;
    fn apply_macro_function(
        &mut self,
        form: Value,
        function: Value,
        args: Vec<Value>,
    ) -> Result<Value, Flow>;
}

impl MacroexpandRuntime for super::eval::Evaluator {
    fn next_pcase_macroexpand_temp_symbol(&mut self) -> Value {
        self.next_pcase_macroexpand_temp_symbol()
    }

    fn resolve_indirect_symbol_by_id(&self, symbol: SymId) -> Option<(SymId, Value)> {
        resolve_indirect_symbol_by_id(self, symbol)
    }

    fn is_global_function_placeholder(&self, symbol: SymId) -> bool {
        self.obarray().symbol_function_id(symbol).is_none()
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
        function: Value,
        args: Vec<Value>,
    ) -> Result<Value, Flow> {
        let saved_roots = self.save_temp_roots();
        self.push_temp_root(form);
        self.push_temp_root(function);
        for arg in &args {
            self.push_temp_root(*arg);
        }
        let expanded = self.with_macro_expansion_scope(|eval| eval.apply(function, args))?;
        self.restore_temp_roots(saved_roots);
        Ok(expanded)
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
    if name.starts_with(':') && eq_value(&Value::Keyword(symbol), &new_value) {
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
    lookup_interned(resolve_sym(id)).is_some_and(|canonical| canonical == id)
}

pub(crate) fn resolve_variable_alias_id_in_obarray(
    obarray: &Obarray,
    symbol: SymId,
) -> Result<SymId, Flow> {
    use crate::emacs_core::symbol::SymbolValue;

    let mut current = symbol;
    let mut seen = HashSet::new();

    loop {
        if !seen.insert(current) {
            return Err(signal(
                "cyclic-variable-indirection",
                vec![Value::Symbol(symbol)],
            ));
        }
        // Primary: check SymbolValue::Alias variant.
        match obarray.get_by_id(current) {
            Some(sym) => match &sym.value {
                SymbolValue::Alias(target) => current = *target,
                _ => {
                    // Fallback: also check plist for backward compatibility
                    // with symbols that were aliased before the enum refactor.
                    let next = sym
                        .plist
                        .get(&intern(VARIABLE_ALIAS_PROPERTY))
                        .and_then(symbol_id);
                    match next {
                        Some(next_id) => current = next_id,
                        None => return Ok(current),
                    }
                }
            },
            None => return Ok(current),
        }
    }
}

pub(crate) fn resolve_variable_alias_id(
    eval: &super::eval::Evaluator,
    symbol: SymId,
) -> Result<SymId, Flow> {
    resolve_variable_alias_id_in_obarray(eval.obarray(), symbol)
}

pub(crate) fn resolve_variable_alias_name(
    eval: &super::eval::Evaluator,
    name: &str,
) -> Result<String, Flow> {
    resolve_variable_alias_name_in_obarray(eval.obarray(), name)
}

pub(crate) fn resolve_variable_alias_name_in_obarray(
    obarray: &Obarray,
    name: &str,
) -> Result<String, Flow> {
    Ok(resolve_sym(resolve_variable_alias_id_in_obarray(obarray, intern(name))?).to_string())
}

fn would_create_variable_alias_cycle(eval: &super::eval::Evaluator, new: &str, old: &str) -> bool {
    would_create_variable_alias_cycle_in_obarray(eval.obarray(), intern(new), intern(old))
}

pub(crate) fn would_create_variable_alias_cycle_in_obarray(
    obarray: &Obarray,
    new_symbol: SymId,
    old_symbol: SymId,
) -> bool {
    use crate::emacs_core::symbol::SymbolValue;

    let mut current = old_symbol;
    let mut seen = HashSet::new();

    loop {
        if current == new_symbol {
            return true;
        }
        if !seen.insert(current) {
            return true;
        }
        // Primary: check SymbolValue::Alias variant.
        match obarray.get_by_id(current) {
            Some(sym) => match &sym.value {
                SymbolValue::Alias(target) => current = *target,
                _ => {
                    // Fallback: plist for backward compatibility.
                    let next = sym
                        .plist
                        .get(&intern(VARIABLE_ALIAS_PROPERTY))
                        .and_then(symbol_id);
                    match next {
                        Some(next_id) => current = next_id,
                        None => return false,
                    }
                }
            },
            None => return false,
        }
    }
}

pub(crate) fn symbol_raw_plist_value_in_obarray(obarray: &Obarray, symbol: SymId) -> Option<Value> {
    obarray
        .get_property_id(symbol, intern(RAW_SYMBOL_PLIST_PROPERTY))
        .cloned()
}

fn symbol_raw_plist_value(eval: &super::eval::Evaluator, symbol: SymId) -> Option<Value> {
    symbol_raw_plist_value_in_obarray(eval.obarray(), symbol)
}

pub(crate) fn visible_symbol_plist_snapshot_in_obarray(obarray: &Obarray, symbol: SymId) -> Value {
    let Some(sym) = obarray.get_by_id(symbol) else {
        return Value::Nil;
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
        Value::Nil
    } else {
        Value::list(items)
    }
}

fn sync_visible_symbol_plist_entries(
    sym: &mut crate::emacs_core::symbol::SymbolData,
    plist: Value,
) {
    let mut cursor = plist;
    loop {
        match cursor {
            Value::Nil => return,
            Value::Cons(key_cell) => {
                let pair = read_cons(key_cell);
                let key = pair.car;
                let rest = pair.cdr;
                drop(pair);

                let Some(key_id) = symbol_id(&key) else {
                    return;
                };
                let Value::Cons(value_cell) = rest else {
                    return;
                };

                let value_pair = read_cons(value_cell);
                let value = value_pair.car;
                cursor = value_pair.cdr;
                drop(value_pair);

                if is_internal_symbol_plist_property(resolve_sym(key_id)) {
                    continue;
                }
                sym.plist.insert(key_id, value);
            }
            _ => return,
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

    let sym = obarray.ensure_symbol_id(symbol);
    sym.plist.clear();
    for (key, value) in preserved_internal {
        sym.plist.insert(key, value);
    }
    sym.plist.insert(intern(RAW_SYMBOL_PLIST_PROPERTY), plist);
    sync_visible_symbol_plist_entries(sym, plist);
}

fn set_symbol_raw_plist(eval: &mut super::eval::Evaluator, symbol: SymId, plist: Value) {
    set_symbol_raw_plist_in_obarray(eval.obarray_mut(), symbol, plist);
}

pub(crate) fn plist_lookup_value(plist: &Value, prop: &Value) -> Option<Value> {
    let mut cursor = *plist;
    loop {
        match cursor {
            Value::Nil => return None,
            Value::Cons(pair_cell) => {
                let pair = read_cons(pair_cell);
                let key = pair.car;
                let rest = pair.cdr;
                drop(pair);
                let Value::Cons(value_cell) = rest else {
                    return None;
                };
                let value_pair = read_cons(value_cell);
                let value = value_pair.car;
                let next = value_pair.cdr;
                if eq_value(&key, prop) {
                    return Some(value);
                }
                cursor = next;
            }
            _ => return None,
        }
    }
}

pub(crate) fn builtin_boundp(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_boundp_in_state(eval.obarray(), eval.dynamic.as_slice(), &eval.buffers, args)
}

pub(crate) fn builtin_boundp_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("boundp", &args, 1)?;
    let resolved = resolve_variable_alias_id_in_obarray(obarray, expect_symbol_id(&args[0])?)?;
    // specbind writes directly to obarray, so no dynamic stack lookup needed.
    let resolved_name = resolve_sym(resolved);
    if let Some(buf) = buffers.current_buffer() {
        if let Some(binding) = buf.get_buffer_local_binding(resolved_name) {
            return Ok(Value::bool(binding.as_value().is_some()));
        }
    }
    Ok(Value::bool(
        obarray.boundp_id(resolved) || obarray.is_constant_id(resolved),
    ))
}

pub(crate) fn builtin_obarrayp(args: Vec<Value>) -> EvalResult {
    expect_args("obarrayp", &args, 1)?;
    Ok(Value::bool(expect_obarray_vector_id(&args[0]).is_ok()))
}

pub(crate) fn builtin_special_variable_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_special_variable_p_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_special_variable_p_in_obarray(
    obarray: &Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("special-variable-p", &args, 1)?;
    let resolved = resolve_variable_alias_id_in_obarray(obarray, expect_symbol_id(&args[0])?)?;
    Ok(Value::bool(
        obarray.is_special_id(resolved) || obarray.is_constant_id(resolved),
    ))
}

pub(crate) fn builtin_default_boundp(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_default_boundp_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_default_boundp_in_obarray(obarray: &Obarray, args: Vec<Value>) -> EvalResult {
    expect_args("default-boundp", &args, 1)?;
    let resolved = resolve_variable_alias_id_in_obarray(obarray, expect_symbol_id(&args[0])?)?;
    Ok(Value::bool(
        obarray.boundp_id(resolved) || obarray.is_constant_id(resolved),
    ))
}

pub(crate) fn builtin_default_toplevel_value(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_default_toplevel_value_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_default_toplevel_value_in_obarray(
    obarray: &Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("default-toplevel-value", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    let resolved = resolve_variable_alias_id_in_obarray(obarray, symbol)?;
    let resolved_name = resolve_sym(resolved);
    match obarray.symbol_value_id(resolved).cloned() {
        Some(value) => Ok(value),
        None if is_canonical_symbol_id(resolved) && resolved_name.starts_with(':') => {
            Ok(Value::Keyword(resolved))
        }
        None => Err(signal("void-variable", vec![args[0]])),
    }
}

pub(crate) fn builtin_internal_define_uninitialized_variable_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_internal_define_uninitialized_variable_in_obarray(eval.obarray_mut(), args.clone())?;
    let documentation = args.get(1).copied().unwrap_or(Value::Nil);
    if !documentation.is_nil() {
        preflight_symbol_plist_put(eval, &args[0], "variable-documentation")?;
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_internal_define_uninitialized_variable_in_obarray(
    obarray: &mut Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("internal--define-uninitialized-variable", &args, 1, 2)?;
    let symbol = expect_symbol_id(&args[0])?;
    let documentation = args.get(1).copied().unwrap_or(Value::Nil);

    obarray.make_special_id(symbol);

    if !documentation.is_nil() {
        obarray.put_property_id(symbol, intern("variable-documentation"), documentation);
    }

    Ok(Value::Nil)
}

pub(crate) fn builtin_set_default_toplevel_value(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_default_toplevel_value_in_obarray(eval.obarray_mut(), args.clone())?;
    let symbol = expect_symbol_id(&args[0])?;
    let resolved = resolve_variable_alias_id(eval, symbol)?;
    let resolved_name = resolve_sym(resolved);
    let value = args[1];
    eval.run_variable_watchers(resolved_name, &value, &Value::Nil, "set")?;
    if resolved != symbol {
        eval.run_variable_watchers(resolved_name, &value, &Value::Nil, "set")?;
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_set_default_toplevel_value_in_obarray(
    obarray: &mut Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-default-toplevel-value", &args, 2)?;
    let symbol = expect_symbol_id(&args[0])?;
    let resolved = resolve_variable_alias_id_in_obarray(obarray, symbol)?;
    if obarray.is_constant_id(resolved) {
        return Err(signal("setting-constant", vec![args[0]]));
    }
    let value = args[1];
    obarray.set_symbol_value_id(resolved, value);
    Ok(Value::Nil)
}

pub(crate) fn builtin_defvaralias_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let state_change = builtin_defvaralias_in_state(eval.obarray_mut(), args.clone())?;
    eval.run_variable_watchers(
        &state_change.previous_target,
        &state_change.base_variable,
        &Value::Nil,
        "defvaralias",
    )?;
    eval.watchers.clear_watchers(&state_change.alias_name);
    // GNU Emacs updates `variable-documentation` through plist machinery after
    // installing alias state, so malformed raw plists still raise
    // `(wrong-type-argument plistp ...)` with the alias edge retained.
    builtin_put_in_obarray(
        eval.obarray_mut(),
        vec![
            args[0],
            Value::symbol("variable-documentation"),
            state_change.docstring,
        ],
    )?;
    Ok(state_change.result)
}

pub(crate) struct DefvaraliasStateChange {
    pub(crate) alias_name: String,
    pub(crate) previous_target: String,
    pub(crate) base_variable: Value,
    pub(crate) docstring: Value,
    pub(crate) result: Value,
}

pub(crate) fn builtin_defvaralias_in_state(
    obarray: &mut Obarray,
    args: Vec<Value>,
) -> Result<DefvaraliasStateChange, Flow> {
    expect_range_args("defvaralias", &args, 2, 3)?;
    let new_symbol = expect_symbol_id(&args[0])?;
    let old_symbol = expect_symbol_id(&args[1])?;
    let new_name = resolve_sym(new_symbol).to_string();
    if obarray.is_constant_id(new_symbol) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Cannot make a constant an alias: {new_name}"
            ))],
        ));
    }
    if would_create_variable_alias_cycle_in_obarray(obarray, new_symbol, old_symbol) {
        return Err(signal("cyclic-variable-indirection", vec![args[1]]));
    }
    let previous_target = resolve_variable_alias_name_in_obarray(obarray, &new_name)?;
    {
        let sym = obarray.ensure_symbol_id(new_symbol);
        sym.special = true;
        // Keep the plist entry for backward compatibility during transition.
        sym.plist.insert(intern(VARIABLE_ALIAS_PROPERTY), args[1]);
    }
    // Primary mechanism: set the SymbolValue::Alias variant.
    obarray.make_alias(new_symbol, old_symbol);
    obarray.make_special_id(old_symbol);
    preflight_symbol_plist_put_in_obarray(obarray, new_symbol, "variable-documentation")?;
    let docstring = args.get(2).cloned().unwrap_or(Value::Nil);
    Ok(DefvaraliasStateChange {
        alias_name: new_name,
        previous_target,
        base_variable: args[1],
        docstring,
        result: args[1],
    })
}

pub(crate) fn builtin_indirect_variable_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_indirect_variable_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_indirect_variable_in_obarray(
    obarray: &Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("indirect-variable", &args, 1)?;
    let Some(symbol) = symbol_id(&args[0]) else {
        return Ok(args[0]);
    };
    let resolved = resolve_variable_alias_id_in_obarray(obarray, symbol)?;
    Ok(value_from_symbol_id(resolved))
}

pub(crate) fn builtin_fboundp_in_obarray(obarray: &Obarray, args: &[Value]) -> EvalResult {
    expect_args("fboundp", args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    let name = resolve_sym(symbol);
    if obarray.is_function_unbound_id(symbol) {
        return Ok(Value::Nil);
    }
    if let Some(function) = obarray.symbol_function_id(symbol) {
        let result = !function.is_nil();
        return Ok(Value::bool(result));
    }
    if !is_canonical_symbol_id(symbol) {
        return Ok(Value::Nil);
    }
    let macro_bound = super::subr_info::is_evaluator_macro_name(name);
    let result = super::subr_info::is_special_form(name)
        || macro_bound
        || super::subr_info::is_evaluator_callable_name(name)
        || super::builtin_registry::is_dispatch_builtin_name(name)
        || name.parse::<PureBuiltinId>().is_ok();
    Ok(Value::bool(result))
}

pub(crate) fn builtin_fboundp(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_fboundp_in_obarray(eval.obarray(), &args)
}

pub(crate) fn builtin_symbol_value(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_symbol_value_in_state(eval.obarray(), eval.dynamic.as_slice(), &eval.buffers, args)
}

pub(crate) fn builtin_symbol_value_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("symbol-value", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    let resolved = resolve_variable_alias_id_in_obarray(obarray, symbol)?;
    let resolved_name = resolve_sym(resolved);
    let resolved_is_canonical = is_canonical_symbol_id(resolved);
    // specbind writes directly to obarray, so no dynamic stack lookup needed.
    // Buffer-local bindings are keyed by canonical symbol names only.
    if resolved_is_canonical && let Some(buf) = buffers.current_buffer() {
        if let Some(binding) = buf.get_buffer_local_binding(resolved_name) {
            return binding
                .as_value()
                .ok_or_else(|| signal("void-variable", vec![args[0]]));
        }
    }
    match obarray.symbol_value_id(resolved).cloned() {
        Some(value) => Ok(value),
        None if resolved_is_canonical && resolved_name.starts_with(':') => {
            Ok(Value::Keyword(resolved))
        }
        None => Err(signal("void-variable", vec![args[0]])),
    }
}

pub(super) fn startup_virtual_autoload_function_cell(
    _eval: &super::eval::Evaluator,
    _name: &str,
) -> Option<Value> {
    None
}

pub(crate) fn builtin_symbol_function(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_symbol_function_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_symbol_function_in_obarray(
    obarray: &Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("symbol-function", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    let name = resolve_sym(symbol);
    if obarray.is_function_unbound_id(symbol) {
        return Ok(Value::Nil);
    }

    if let Some(function) = obarray.symbol_function_id(symbol) {
        // GNU Emacs exposes this symbol as autoload-shaped in startup state,
        // then subr-shaped after first invocation triggers autoload materialization.
        if name == "kmacro-name-last-macro"
            && matches!(function, Value::Subr(subr) if resolve_sym(*subr) == "kmacro-name-last-macro")
            && obarray
                .get_property_id(symbol, intern("neovm--kmacro-autoload-promoted"))
                .is_none()
        {
            return Ok(Value::list(vec![
                Value::symbol("autoload"),
                Value::string("kmacro"),
                Value::string("Assign a name to the last keyboard macro defined."),
                Value::True,
                Value::Nil,
            ]));
        }
        return Ok(*function);
    }

    if !is_canonical_symbol_id(symbol) {
        return Ok(Value::Nil);
    }

    Ok(symbol_function_cell_in_obarray(obarray, symbol).unwrap_or(Value::Nil))
}

/// `(function-get F PROP &optional AUTOLOAD)` — Rust implementation
/// matching subr.el. Avoids excessive eval depth by not going through
/// the Elisp evaluator for get/fboundp/symbol-function calls.
pub(crate) fn builtin_function_get(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("function-get", &args, 2)?;
    expect_max_args("function-get", &args, 3)?;
    let prop = args[1];
    let autoload = args.get(2).copied().unwrap_or(Value::Nil);
    let prop_id = match &prop {
        Value::Symbol(id) | Value::Keyword(id) => *id,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), prop],
            ));
        }
    };
    let mut f = args[0];
    let mut val = Value::Nil;
    let mut iterations = 0;
    while let Value::Symbol(sym_id) = f {
        // Check property
        if let Some(v) = eval.obarray.get_property_id(sym_id, prop_id) {
            val = *v;
            break;
        }
        // Check fboundp
        if eval.obarray.symbol_function_id(sym_id).is_none()
            && !super::super::builtin_registry::is_dispatch_builtin_name(resolve_sym(sym_id))
        {
            break;
        }
        let fundef =
            builtin_symbol_function_in_obarray(eval.obarray(), vec![f]).unwrap_or(Value::Nil);
        if fundef.is_nil() {
            break;
        }
        // Handle autoloads
        if autoload.is_truthy() && super::super::autoload::is_autoload_value(&fundef) {
            let loaded = super::super::autoload::builtin_autoload_do_load(
                eval,
                vec![
                    fundef,
                    f,
                    if autoload.is_symbol_named("macro") {
                        Value::symbol("macro")
                    } else {
                        Value::Nil
                    },
                ],
            );
            if let Ok(new_def) = loaded {
                if new_def != fundef {
                    continue; // Re-try get on same f
                }
            }
        }
        f = fundef;
        iterations += 1;
        if iterations > 100 {
            // Prevent infinite loops from cyclic function aliases
            break;
        }
    }
    Ok(val)
}

pub(crate) fn builtin_func_arity_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_func_arity_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_func_arity_in_obarray(obarray: &Obarray, args: Vec<Value>) -> EvalResult {
    expect_args("func-arity", &args, 1)?;

    if let Some(name) = args[0].as_symbol_name() {
        if let Some(function) =
            resolve_indirect_symbol_by_id_in_obarray(obarray, intern(name)).map(|(_, value)| value)
        {
            if function.is_nil() {
                return Err(signal("void-function", vec![Value::symbol(name)]));
            }
            if super::subr_info::is_special_form(name) {
                return super::subr_info::builtin_func_arity(vec![Value::Subr(intern(name))]);
            }
            if let Some(arity) =
                dispatch_symbol_func_arity_override_in_obarray(obarray, name, &function)
            {
                return Ok(arity);
            }
            return super::subr_info::builtin_func_arity(vec![function]);
        }
        return Err(signal("void-function", vec![Value::symbol(name)]));
    }

    super::subr_info::builtin_func_arity(vec![args[0]])
}

fn has_startup_subr_wrapper_in_obarray(obarray: &Obarray, name: &str) -> bool {
    let wrapper = format!("neovm--startup-subr-wrapper-{name}");
    matches!(
        obarray.symbol_function(&wrapper),
        Some(Value::Subr(subr_id)) if resolve_sym(*subr_id) == name
    )
}

fn dispatch_symbol_func_arity_override_in_obarray(
    obarray: &Obarray,
    name: &str,
    function: &Value,
) -> Option<Value> {
    if !super::builtin_registry::is_dispatch_builtin_name(name) {
        return None;
    }

    if super::autoload::is_autoload_value(function)
        || (matches!(function, Value::ByteCode(_))
            && has_startup_subr_wrapper_in_obarray(obarray, name))
    {
        return Some(super::subr_info::dispatch_subr_arity_value(name));
    }

    None
}

pub(crate) fn builtin_set(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
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
        .map(Value::Buffer)
        .unwrap_or(Value::Nil);
    eval.run_variable_watchers_with_where(
        resolve_sym(resolved),
        &value,
        &Value::Nil,
        "set",
        &where_value,
    )?;
    Ok(value)
}

pub(crate) fn builtin_fset(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_fset_in_obarray(eval.obarray_mut(), args)
}

pub(crate) fn builtin_fset_in_obarray(obarray: &mut Obarray, args: Vec<Value>) -> EvalResult {
    expect_args("fset", &args, 2)?;
    let symbol = expect_symbol_id(&args[0])?;
    if symbol == intern("nil") && !args[1].is_nil() {
        return Err(signal("setting-constant", vec![Value::symbol("nil")]));
    }
    let def = args[1];
    if would_create_function_alias_cycle_in_obarray(obarray, symbol, &def) {
        return Err(signal("cyclic-function-indirection", vec![args[0]]));
    }
    obarray.set_symbol_function_id(symbol, def);
    Ok(def)
}

pub(crate) fn would_create_function_alias_cycle(
    eval: &super::eval::Evaluator,
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

pub(crate) fn builtin_makunbound(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("makunbound", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    let resolved = resolve_variable_alias_id(eval, symbol)?;
    if eval.obarray().is_constant_id(resolved) {
        return Err(signal("setting-constant", vec![args[0]]));
    }
    eval.makunbound_runtime_binding_by_id(resolved);
    eval.run_variable_watchers(
        resolve_sym(resolved),
        &Value::Nil,
        &Value::Nil,
        "makunbound",
    )?;
    Ok(args[0])
}

pub(crate) fn builtin_defvar_1_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("defvar-1", &args, 2, 3)?;
    let symbol = expect_symbol_id(&args[0])?;
    let documentation = args.get(2).copied().unwrap_or(Value::Nil);
    let was_bound = builtin_default_boundp(eval, vec![args[0]])?.is_truthy();

    if documentation.is_nil() {
        builtin_internal_define_uninitialized_variable_eval(eval, vec![args[0]])?;
    } else {
        builtin_internal_define_uninitialized_variable_eval(eval, vec![args[0], documentation])?;
    }

    if !was_bound {
        builtin_set_default_toplevel_value(eval, vec![args[0], args[1]])?;
    }

    Ok(Value::Symbol(symbol))
}

pub(crate) fn builtin_defconst_1_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("defconst-1", &args, 2, 3)?;
    let symbol = expect_symbol_id(&args[0])?;
    let documentation = args.get(2).copied().unwrap_or(Value::Nil);

    if documentation.is_nil() {
        builtin_internal_define_uninitialized_variable_eval(eval, vec![args[0]])?;
    } else {
        builtin_internal_define_uninitialized_variable_eval(eval, vec![args[0], documentation])?;
    }

    let resolved = resolve_variable_alias_id(eval, symbol)?;
    let value = args[1];
    eval.obarray_mut().set_symbol_value_id(resolved, value);
    eval.obarray_mut().ensure_symbol_id(resolved).constant = true;
    eval.obarray_mut()
        .put_property_id(resolved, intern("risky-local-variable"), Value::True);

    Ok(Value::Symbol(symbol))
}

pub(crate) fn builtin_fmakunbound(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_fmakunbound_in_obarray(eval.obarray_mut(), args)
}

pub(crate) fn builtin_fmakunbound_in_obarray(
    obarray: &mut Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("fmakunbound", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    if symbol == intern("nil") || symbol == intern("t") {
        return Err(signal("setting-constant", vec![args[0]]));
    }
    obarray.fmakunbound_id(symbol);
    Ok(args[0])
}

pub(crate) fn builtin_get(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("get", &args, 2)?;
    let sym = expect_symbol_id(&args[0])?;
    if let Some(raw) = symbol_raw_plist_value(eval, sym) {
        return Ok(plist_lookup_value(&raw, &args[1]).unwrap_or(Value::Nil));
    }
    let prop = expect_symbol_id(&args[1])?;
    if is_internal_symbol_plist_property(resolve_sym(prop)) {
        return Ok(Value::Nil);
    }
    Ok(eval
        .obarray()
        .get_property_id(sym, prop)
        .cloned()
        .unwrap_or(Value::Nil))
}

pub(crate) fn builtin_put(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_put_in_obarray(eval.obarray_mut(), args)
}

pub(crate) fn builtin_put_in_obarray(obarray: &mut Obarray, args: Vec<Value>) -> EvalResult {
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_symbol_plist_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_symbol_plist_in_obarray(obarray: &Obarray, args: Vec<Value>) -> EvalResult {
    expect_args("symbol-plist", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    if let Some(raw) = symbol_raw_plist_value_in_obarray(obarray, symbol) {
        return Ok(raw);
    }
    Ok(visible_symbol_plist_snapshot_in_obarray(obarray, symbol))
}

pub(super) fn builtin_register_code_conversion_map_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_register_code_conversion_map_in_obarray(eval.obarray_mut(), args)
}

pub(crate) fn builtin_register_code_conversion_map_in_obarray(
    obarray: &mut Obarray,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() == 2 {
        preflight_symbol_plist_put_in_obarray(
            obarray,
            expect_symbol_id(&args[0])?,
            "code-conversion-map",
        )?;
    }
    let map_id = super::ccl::builtin_register_code_conversion_map(args.clone())?;

    let _ = builtin_put_in_obarray(
        obarray,
        vec![args[0], Value::symbol("code-conversion-map"), args[1]],
    )?;
    let _ = builtin_put_in_obarray(
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
        .unwrap_or(Value::Nil);
    Ok(idx.as_int().is_some_and(|n| n >= 0))
}

fn symbol_has_valid_ccl_program_idx(
    eval: &mut super::eval::Evaluator,
    symbol: &Value,
) -> Result<bool, Flow> {
    symbol_has_valid_ccl_program_idx_in_obarray(eval.obarray(), symbol)
}

pub(super) fn builtin_ccl_program_p_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_ccl_program_p_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_ccl_program_p_in_obarray(obarray: &Obarray, args: Vec<Value>) -> EvalResult {
    if args.len() == 1 && args[0].is_symbol() {
        return Ok(Value::bool(symbol_has_valid_ccl_program_idx_in_obarray(
            obarray, &args[0],
        )?));
    }
    super::ccl::builtin_ccl_program_p(args)
}

pub(super) fn builtin_ccl_execute_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_ccl_execute_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_ccl_execute_in_obarray(obarray: &Obarray, args: Vec<Value>) -> EvalResult {
    if args.first().is_some_and(Value::is_symbol)
        && !symbol_has_valid_ccl_program_idx_in_obarray(obarray, &args[0])?
    {
        let mut forced = args.clone();
        forced[0] = Value::Int(0);
        return super::ccl::builtin_ccl_execute(forced);
    }
    super::ccl::builtin_ccl_execute(args)
}

pub(super) fn builtin_ccl_execute_on_string_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_ccl_execute_on_string_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_ccl_execute_on_string_in_obarray(
    obarray: &Obarray,
    args: Vec<Value>,
) -> EvalResult {
    if args.first().is_some_and(Value::is_symbol)
        && !symbol_has_valid_ccl_program_idx_in_obarray(obarray, &args[0])?
    {
        let mut forced = args.clone();
        forced[0] = Value::Int(0);
        return super::ccl::builtin_ccl_execute_on_string(forced);
    }
    super::ccl::builtin_ccl_execute_on_string(args)
}

pub(super) fn builtin_register_ccl_program_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_register_ccl_program_in_obarray(eval.obarray_mut(), args)
}

pub(crate) fn builtin_register_ccl_program_in_obarray(
    obarray: &mut Obarray,
    args: Vec<Value>,
) -> EvalResult {
    let was_registered = args
        .first()
        .and_then(Value::as_symbol_name)
        .is_some_and(super::ccl::is_registered_ccl_program);
    let program_id = super::ccl::builtin_register_ccl_program(args.clone())?;

    if was_registered {
        return Ok(program_id);
    }

    let publish = builtin_put_in_obarray(
        obarray,
        vec![args[0], Value::symbol("ccl-program-idx"), program_id],
    );
    if let Err(err) = publish {
        if let Some(name) = args[0].as_symbol_name() {
            super::ccl::unregister_registered_ccl_program(name);
        }
        return Err(err);
    }

    Ok(program_id)
}

fn preflight_symbol_plist_put(
    eval: &mut super::eval::Evaluator,
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
    let _ = builtin_plist_put(vec![raw, Value::symbol(property), Value::Nil])?;
    Ok(())
}

pub(crate) fn builtin_setplist_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_setplist_in_obarray(eval.obarray_mut(), args)
}

pub(crate) fn builtin_setplist_in_obarray(obarray: &mut Obarray, args: Vec<Value>) -> EvalResult {
    expect_args("setplist", &args, 2)?;
    let symbol = expect_symbol_id(&args[0])?;
    let plist = args[1];
    set_symbol_raw_plist_in_obarray(obarray, symbol, plist);
    Ok(plist)
}

fn macroexpand_environment_binding_by_id(env: &Value, target: SymId) -> Option<Value> {
    let mut cursor = *env;
    loop {
        match cursor {
            Value::Nil => return None,
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                let entry = pair.car;
                cursor = pair.cdr;
                drop(pair);
                let Value::Cons(entry_cell) = entry else {
                    continue;
                };
                let entry_pair = read_cons(entry_cell);
                if matches!(symbol_id(&entry_pair.car), Some(id) if id == target) {
                    return Some(entry_pair.cdr);
                }
            }
            _ => return None,
        }
    }
}

fn macroexpand_environment_callable(binding: &Value) -> Result<Value, Flow> {
    Ok(*binding)
}

enum SimpleBackquoteListPattern {
    Proper(Vec<Value>),
    Dotted { heads: Vec<Value>, tail: Value },
    Vector(Vec<Value>),
}

fn parse_simple_backquote_list_unquotes(pattern: &Value) -> Option<SimpleBackquoteListPattern> {
    fn is_backquote_symbol(value: &Value) -> bool {
        matches!(value.as_symbol_name(), Some("`"))
    }
    fn parse_unquoted_symbol(item: &Value) -> Option<Value> {
        let unquote = list_to_vec(item)?;
        if unquote.len() != 2 || !matches!(unquote[0].as_symbol_name(), Some(",")) {
            return None;
        }
        unquote[1].as_symbol_name()?;
        Some(unquote[1])
    }

    let outer = list_to_vec(pattern)?;
    if outer.len() != 2 || !is_backquote_symbol(&outer[0]) {
        return None;
    }
    let items = if let Some(items) = list_to_vec(&outer[1]) {
        items
    } else if let Value::Vector(items) = &outer[1] {
        let items = with_heap(|h| h.get_vector(*items).clone());
        if items.is_empty() {
            return None;
        }
        let mut vars = Vec::with_capacity(items.len());
        for item in &items {
            vars.push(parse_unquoted_symbol(item)?);
        }
        return Some(SimpleBackquoteListPattern::Vector(vars));
    } else {
        return None;
    };
    if items.is_empty() {
        return None;
    }

    if let Some(dot_idx) = items
        .iter()
        .position(|item| item.as_symbol_name().is_some_and(|name| name == ","))
    {
        if dot_idx == 0 || dot_idx + 2 != items.len() {
            return None;
        }
        let mut heads = Vec::with_capacity(dot_idx);
        for item in &items[..dot_idx] {
            heads.push(parse_unquoted_symbol(item)?);
        }
        if heads.is_empty() {
            return None;
        }
        let tail = items[dot_idx + 1];
        tail.as_symbol_name()?;
        return Some(SimpleBackquoteListPattern::Dotted { heads, tail });
    }

    let mut vars = Vec::with_capacity(items.len());
    for item in &items {
        vars.push(parse_unquoted_symbol(item)?);
    }
    Some(SimpleBackquoteListPattern::Proper(vars))
}

fn expand_simple_backquote_list_pcase_let_star(
    next_temp_symbol: &mut impl FnMut() -> Value,
    value_expr: &Value,
    pattern: &SimpleBackquoteListPattern,
    body_forms: &[Value],
) -> Option<Value> {
    let should_wrap_source = match value_expr {
        Value::Cons(cell) => {
            let head = with_heap(|h| h.cons_car(*cell));
            !matches!(head.as_symbol_name(), Some("quote" | "function"))
        }
        _ => false,
    };
    let source_expr = if should_wrap_source {
        Value::symbol("val")
    } else {
        *value_expr
    };

    let (head_vars, tail_var) = match pattern {
        SimpleBackquoteListPattern::Proper(vars) => (vars.as_slice(), None),
        SimpleBackquoteListPattern::Dotted { heads, tail } => (heads.as_slice(), Some(tail)),
        SimpleBackquoteListPattern::Vector(vars) => {
            if vars.is_empty() {
                return None;
            }

            let length_sym = next_temp_symbol();
            let mut elem_bindings = Vec::with_capacity(vars.len());
            let mut var_bindings = Vec::with_capacity(vars.len());
            for (idx, var) in vars.iter().enumerate() {
                let temp = next_temp_symbol();
                elem_bindings.push(Value::list(vec![
                    temp,
                    Value::list(vec![
                        Value::symbol("aref"),
                        source_expr,
                        Value::Int(idx as i64),
                    ]),
                ]));
                var_bindings.push(Value::list(vec![*var, temp]));
            }

            let mut let_body = Vec::with_capacity(body_forms.len() + 2);
            let_body.push(Value::symbol("let"));
            let_body.push(Value::list(var_bindings));
            let_body.extend_from_slice(body_forms);

            let mut expanded = Value::list(vec![
                Value::symbol("progn"),
                Value::list(vec![
                    Value::symbol("ignore"),
                    Value::list(vec![Value::symbol("vectorp"), source_expr]),
                ]),
                Value::list(vec![
                    Value::symbol("let*"),
                    Value::list(vec![Value::list(vec![
                        length_sym,
                        Value::list(vec![Value::symbol("length"), source_expr]),
                    ])]),
                    Value::list(vec![
                        Value::symbol("progn"),
                        Value::list(vec![
                            Value::symbol("ignore"),
                            Value::list(vec![
                                Value::symbol("eql"),
                                length_sym,
                                Value::Int(vars.len() as i64),
                            ]),
                        ]),
                        Value::list(vec![
                            Value::symbol("let*"),
                            Value::list(elem_bindings),
                            Value::list(let_body),
                        ]),
                    ]),
                ]),
            ]);
            if should_wrap_source {
                expanded = Value::list(vec![
                    Value::symbol("let*"),
                    Value::list(vec![Value::list(vec![Value::symbol("val"), *value_expr])]),
                    expanded,
                ]);
            }
            return Some(expanded);
        }
    };
    if head_vars.is_empty() {
        return None;
    }

    let mut steps = Vec::with_capacity(head_vars.len());
    let mut source = source_expr;
    for _ in head_vars {
        let head = next_temp_symbol();
        let tail = next_temp_symbol();
        steps.push((source, head, tail));
        source = tail;
    }

    let mut var_bindings = Vec::with_capacity(head_vars.len() + usize::from(tail_var.is_some()));
    for (var, (_, head, _)) in head_vars.iter().zip(steps.iter()) {
        var_bindings.push(Value::list(vec![*var, *head]));
    }
    let (_, _, last_tail) = steps.last()?;
    if let Some(tail_name) = tail_var {
        var_bindings.push(Value::list(vec![(*tail_name), *last_tail]));
    }
    let mut let_forms = Vec::with_capacity(body_forms.len() + 2);
    let_forms.push(Value::symbol("let"));
    let_forms.push(Value::list(var_bindings));
    let_forms.extend_from_slice(body_forms);

    let mut expanded = if tail_var.is_some() {
        Value::list(let_forms)
    } else {
        Value::list(vec![
            Value::symbol("progn"),
            Value::list(vec![
                Value::symbol("ignore"),
                Value::list(vec![Value::symbol("null"), *last_tail]),
            ]),
            Value::list(let_forms),
        ])
    };

    for (source_expr, head, tail) in steps.into_iter().rev() {
        expanded = Value::list(vec![
            Value::symbol("progn"),
            Value::list(vec![
                Value::symbol("ignore"),
                Value::list(vec![Value::symbol("consp"), source_expr]),
            ]),
            Value::list(vec![
                Value::symbol("let*"),
                Value::list(vec![
                    Value::list(vec![
                        head,
                        Value::list(vec![Value::symbol("car-safe"), source_expr]),
                    ]),
                    Value::list(vec![
                        tail,
                        Value::list(vec![Value::symbol("cdr-safe"), source_expr]),
                    ]),
                ]),
                expanded,
            ]),
        ]);
    }

    if should_wrap_source {
        expanded = Value::list(vec![
            Value::symbol("let*"),
            Value::list(vec![Value::list(vec![Value::symbol("val"), *value_expr])]),
            expanded,
        ]);
    }

    Some(expanded)
}

fn collapse_macroexpand_body_forms(body_forms: &[Value]) -> Value {
    match body_forms.len() {
        0 => Value::Nil,
        1 => body_forms[0],
        _ => {
            let mut forms = Vec::with_capacity(body_forms.len() + 1);
            forms.push(Value::symbol("progn"));
            forms.extend_from_slice(body_forms);
            Value::list(forms)
        }
    }
}

#[derive(Clone)]
struct PcaseFallbackBinding {
    original: Value,
    pattern: Value,
    value_tail: Value,
    value_expr: Value,
}

fn parse_pcase_fallback_binding(binding: &Value) -> Result<PcaseFallbackBinding, Flow> {
    let Value::Cons(cell) = *binding else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *binding],
        ));
    };
    let pair = read_cons(cell);
    let pattern = pair.car;
    let cdr = pair.cdr;
    drop(pair);
    let value_tail = cdr;

    let value_expr = match cdr {
        Value::Nil => Value::Nil,
        Value::Cons(cdr_cell) => with_heap(|h| h.cons_car(cdr_cell)),
        other => other,
    };

    Ok(PcaseFallbackBinding {
        original: *binding,
        pattern,
        value_tail,
        value_expr,
    })
}

fn collect_pcase_fallback_bindings(bindings: &Value) -> Result<Vec<PcaseFallbackBinding>, Flow> {
    let mut cursor = *bindings;
    let mut parsed = Vec::new();
    loop {
        match cursor {
            Value::Nil => return Ok(parsed),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                let binding = pair.car;
                cursor = pair.cdr;
                drop(pair);
                parsed.push(parse_pcase_fallback_binding(&binding)?);
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), other],
                ));
            }
        }
    }
}

fn macroexpand_known_fallback_macro(
    next_temp_symbol: &mut impl FnMut() -> Value,
    name: &str,
    args: &[Value],
) -> Result<Option<Value>, Flow> {
    match name {
        "when" => {
            if args.is_empty() {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![Value::cons(Value::Int(1), Value::Int(1)), Value::Int(0)],
                ));
            }
            if args.len() == 1 {
                return Ok(Some(Value::list(vec![
                    Value::symbol("progn"),
                    args[0],
                    Value::Nil,
                ])));
            }
            let mut then_forms = Vec::with_capacity(args.len());
            then_forms.push(Value::symbol("progn"));
            then_forms.extend_from_slice(&args[1..]);
            Ok(Some(Value::list(vec![
                Value::symbol("if"),
                args[0],
                Value::list(then_forms),
            ])))
        }
        "unless" => {
            if args.is_empty() {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![Value::cons(Value::Int(1), Value::Int(1)), Value::Int(0)],
                ));
            }
            if args.len() == 1 {
                return Ok(Some(Value::list(vec![
                    Value::symbol("progn"),
                    args[0],
                    Value::Nil,
                ])));
            }
            let mut forms = Vec::with_capacity(args.len() + 2);
            forms.push(Value::symbol("if"));
            forms.push(args[0]);
            forms.push(Value::Nil);
            forms.extend_from_slice(&args[1..]);
            Ok(Some(Value::list(forms)))
        }
        "save-match-data" => {
            let saved = Value::symbol("saved-match-data");
            let binding = Value::list(vec![saved, Value::list(vec![Value::symbol("match-data")])]);
            let mut protected_forms = Vec::with_capacity(args.len() + 1);
            protected_forms.push(Value::symbol("progn"));
            protected_forms.extend_from_slice(args);
            let protected = Value::list(protected_forms);
            let restore = Value::list(vec![Value::symbol("set-match-data"), saved, Value::True]);
            Ok(Some(Value::list(vec![
                Value::symbol("let"),
                Value::list(vec![binding]),
                Value::list(vec![Value::symbol("unwind-protect"), protected, restore]),
            ])))
        }
        "save-mark-and-excursion" => {
            let saved = Value::symbol("saved-marker");
            let binding = Value::list(vec![
                saved,
                Value::list(vec![Value::symbol("save-mark-and-excursion--save")]),
            ]);
            let mut protected_forms = Vec::with_capacity(args.len() + 1);
            protected_forms.push(Value::symbol("save-excursion"));
            protected_forms.extend_from_slice(args);
            let protected = Value::list(protected_forms);
            let restore = Value::list(vec![
                Value::symbol("save-mark-and-excursion--restore"),
                saved,
            ]);
            Ok(Some(Value::list(vec![
                Value::symbol("let"),
                Value::list(vec![binding]),
                Value::list(vec![Value::symbol("unwind-protect"), protected, restore]),
            ])))
        }
        "save-window-excursion" => {
            let saved = Value::symbol("wconfig");
            let binding = Value::list(vec![
                saved,
                Value::list(vec![Value::symbol("current-window-configuration")]),
            ]);
            let mut protected_forms = Vec::with_capacity(args.len() + 1);
            protected_forms.push(Value::symbol("progn"));
            protected_forms.extend_from_slice(args);
            let protected = Value::list(protected_forms);
            let restore = Value::list(vec![Value::symbol("set-window-configuration"), saved]);
            Ok(Some(Value::list(vec![
                Value::symbol("let"),
                Value::list(vec![binding]),
                Value::list(vec![Value::symbol("unwind-protect"), protected, restore]),
            ])))
        }
        "save-selected-window" => {
            let saved = Value::symbol("save-selected-window--state");
            let binding = Value::list(vec![
                saved,
                Value::list(vec![Value::symbol("internal--before-save-selected-window")]),
            ]);
            let mut protected_forms = Vec::with_capacity(args.len() + 1);
            protected_forms.push(Value::symbol("progn"));
            protected_forms.extend_from_slice(args);
            let protected = Value::list(protected_forms);
            let restore = Value::list(vec![
                Value::symbol("internal--after-save-selected-window"),
                saved,
            ]);
            let unwind = Value::list(vec![Value::symbol("unwind-protect"), protected, restore]);
            Ok(Some(Value::list(vec![
                Value::symbol("let"),
                Value::list(vec![binding]),
                Value::list(vec![Value::symbol("save-current-buffer"), unwind]),
            ])))
        }
        "with-local-quit" => {
            let binding = Value::list(vec![Value::symbol("inhibit-quit"), Value::Nil]);
            let mut let_forms = Vec::with_capacity(args.len() + 2);
            let_forms.push(Value::symbol("let"));
            let_forms.push(Value::list(vec![binding]));
            let_forms.extend_from_slice(args);
            let body = Value::list(let_forms);
            let handler = Value::list(vec![
                Value::symbol("quit"),
                Value::list(vec![
                    Value::symbol("setq"),
                    Value::symbol("quit-flag"),
                    Value::True,
                ]),
                Value::list(vec![
                    Value::symbol("eval"),
                    Value::list(vec![
                        Value::symbol("quote"),
                        Value::list(vec![Value::symbol("ignore"), Value::Nil]),
                    ]),
                    Value::True,
                ]),
            ]);
            Ok(Some(Value::list(vec![
                Value::symbol("condition-case"),
                Value::Nil,
                body,
                handler,
            ])))
        }
        "with-temp-message" => {
            if args.is_empty() {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![
                        Value::cons(Value::Int(1), Value::symbol("many")),
                        Value::Int(0),
                    ],
                ));
            }

            let temp = Value::symbol("with-temp-message");
            let current = Value::symbol("current-message");
            let bindings = Value::list(vec![
                Value::list(vec![temp, args[0]]),
                Value::list(vec![current]),
            ]);

            let when_form = Value::list(vec![
                Value::symbol("when"),
                temp,
                Value::list(vec![
                    Value::symbol("setq"),
                    current,
                    Value::list(vec![Value::symbol("current-message")]),
                ]),
                Value::list(vec![Value::symbol("message"), Value::string("%s"), temp]),
            ]);

            let mut protected_forms = Vec::with_capacity(args.len() + 1);
            protected_forms.push(Value::symbol("progn"));
            protected_forms.push(when_form);
            protected_forms.extend_from_slice(&args[1..]);
            let protected = Value::list(protected_forms);

            let restore = Value::list(vec![
                Value::symbol("and"),
                temp,
                Value::list(vec![
                    Value::symbol("if"),
                    current,
                    Value::list(vec![Value::symbol("message"), Value::string("%s"), current]),
                    Value::list(vec![Value::symbol("message"), Value::Nil]),
                ]),
            ]);

            Ok(Some(Value::list(vec![
                Value::symbol("let"),
                bindings,
                Value::list(vec![Value::symbol("unwind-protect"), protected, restore]),
            ])))
        }
        "with-demoted-errors" => {
            if args.is_empty() {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![Value::cons(Value::Int(1), Value::Int(1)), Value::Int(0)],
                ));
            }

            let (format, body_forms): (Value, Vec<Value>) = if args[0].is_string() {
                if args.len() == 1 {
                    (args[0], vec![args[0]])
                } else {
                    (args[0], args[1..].to_vec())
                }
            } else {
                (Value::string("Error: %S"), args.to_vec())
            };

            let body = if body_forms.len() == 1 {
                body_forms[0]
            } else {
                let mut forms = Vec::with_capacity(body_forms.len() + 1);
                forms.push(Value::symbol("progn"));
                forms.extend(body_forms);
                Value::list(forms)
            };

            Ok(Some(Value::list(vec![
                Value::symbol("condition-case"),
                Value::symbol("err"),
                body,
                Value::list(vec![
                    Value::list(vec![Value::symbol("debug"), Value::symbol("error")]),
                    Value::list(vec![Value::symbol("message"), format, Value::symbol("err")]),
                    Value::Nil,
                ]),
            ])))
        }
        "bound-and-true-p" => {
            if args.len() != 1 {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![
                        Value::cons(Value::Int(1), Value::Int(1)),
                        Value::Int(args.len() as i64),
                    ],
                ));
            }
            if args[0].as_symbol_name().is_none() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), args[0]],
                ));
            }
            let var = args[0];
            Ok(Some(Value::list(vec![
                Value::symbol("and"),
                Value::list(vec![
                    Value::symbol("boundp"),
                    Value::list(vec![Value::symbol("quote"), var]),
                ]),
                var,
            ])))
        }
        "pcase-let" | "pcase-let*" => {
            if args.is_empty() {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![Value::cons(Value::Int(1), Value::Int(1)), Value::Int(0)],
                ));
            }
            if name == "pcase-let*" && args.len() == 1 {
                return Ok(Some(Value::Nil));
            }
            if name == "pcase-let" && args.len() == 1 {
                if let Some(bindings) = list_to_vec(&args[0]) {
                    if bindings.len() <= 1 {
                        return Ok(Some(Value::Nil));
                    }
                } else {
                    let _ = collect_pcase_fallback_bindings(&args[0])?;
                    return Ok(Some(Value::Nil));
                }
            }

            let bindings_src = collect_pcase_fallback_bindings(&args[0])?;
            if name == "pcase-let"
                && bindings_src.len() == 1
                && bindings_src[0].pattern.as_symbol_name().is_none()
            {
                let mut star_args = Vec::with_capacity(args.len());
                star_args.push(Value::list(vec![bindings_src[0].original]));
                star_args.extend_from_slice(&args[1..]);
                return macroexpand_known_fallback_macro(
                    next_temp_symbol,
                    "pcase-let*",
                    &star_args,
                );
            }

            if name == "pcase-let*" {
                enum ParsedPcaseLetStarBinding {
                    Symbol(Value),
                    Pattern {
                        spec: SimpleBackquoteListPattern,
                        value_expr: Value,
                    },
                }

                let mut parsed = Vec::with_capacity(bindings_src.len());
                let mut has_pattern = false;
                let mut unknown_pattern = None;

                for binding in &bindings_src {
                    if binding.pattern.as_symbol_name().is_some() {
                        parsed.push(ParsedPcaseLetStarBinding::Symbol(binding.original));
                        continue;
                    }

                    let Some(spec) = parse_simple_backquote_list_unquotes(&binding.pattern) else {
                        if unknown_pattern.is_none() {
                            unknown_pattern = Some(binding.pattern);
                        }
                        continue;
                    };
                    has_pattern = true;
                    parsed.push(ParsedPcaseLetStarBinding::Pattern {
                        spec,
                        value_expr: binding.value_expr,
                    });
                }

                if unknown_pattern.is_some() {
                    // Pattern too complex for the Rust fast-path (e.g., `,(pred symbolp)`).
                    // Fall through to the Elisp pcase.el macro which handles all patterns.
                    return Ok(None);
                }

                let mut expanded = collapse_macroexpand_body_forms(&args[1..]);
                if !has_pattern {
                    let mut symbol_bindings = Vec::with_capacity(parsed.len());
                    for binding in parsed {
                        let ParsedPcaseLetStarBinding::Symbol(symbol_binding) = binding else {
                            continue;
                        };
                        symbol_bindings.push(symbol_binding);
                    }
                    if symbol_bindings.is_empty() {
                        return Ok(Some(expanded));
                    }
                    return Ok(Some(Value::list(vec![
                        Value::symbol("let*"),
                        Value::list(symbol_bindings),
                        expanded,
                    ])));
                }

                let mut i = parsed.len();
                while i > 0 {
                    let mut symbol_group = Vec::new();
                    while i > 0 {
                        match &parsed[i - 1] {
                            ParsedPcaseLetStarBinding::Symbol(binding) => {
                                symbol_group.push(*binding);
                                i -= 1;
                            }
                            ParsedPcaseLetStarBinding::Pattern { .. } => break,
                        }
                    }
                    if !symbol_group.is_empty() {
                        symbol_group.reverse();
                        expanded = Value::list(vec![
                            Value::symbol("let*"),
                            Value::list(symbol_group),
                            expanded,
                        ]);
                    }
                    if i == 0 {
                        break;
                    }
                    i -= 1;
                    let ParsedPcaseLetStarBinding::Pattern { spec, value_expr } = &parsed[i] else {
                        continue;
                    };
                    let destructure_body = [expanded];
                    expanded = match expand_simple_backquote_list_pcase_let_star(
                        next_temp_symbol,
                        value_expr,
                        spec,
                        &destructure_body,
                    ) {
                        Some(form) => form,
                        None => return Ok(None),
                    };
                }

                return Ok(Some(expanded));
            }

            let mut symbol_bindings = Vec::with_capacity(bindings_src.len());
            let mut pattern_bindings = Vec::new();
            for binding in &bindings_src {
                if binding.pattern.as_symbol_name().is_some() {
                    symbol_bindings.push(binding.original);
                    continue;
                }
                let temp = Value::symbol(format!("x{}", symbol_bindings.len()));
                symbol_bindings.push(Value::cons(temp, binding.value_tail));
                pattern_bindings.push(Value::list(vec![binding.pattern, temp]));
            }

            if !pattern_bindings.is_empty() {
                pattern_bindings.reverse();
                let mut pcase_let_star_forms = Vec::with_capacity(args.len() + 1);
                pcase_let_star_forms.push(Value::symbol("pcase-let*"));
                pcase_let_star_forms.push(Value::list(pattern_bindings));
                pcase_let_star_forms.extend_from_slice(&args[1..]);
                return Ok(Some(Value::list(vec![
                    Value::symbol("let"),
                    Value::list(symbol_bindings),
                    Value::list(pcase_let_star_forms),
                ])));
            }

            if symbol_bindings.is_empty() {
                return Ok(Some(collapse_macroexpand_body_forms(&args[1..])));
            }

            if symbol_bindings.len() == 1 {
                let mut forms = Vec::with_capacity(args.len() + 1);
                forms.push(Value::symbol("let*"));
                forms.push(Value::list(symbol_bindings));
                forms.extend_from_slice(&args[1..]);
                return Ok(Some(Value::list(forms)));
            }

            let mut pcase_let_star_forms = Vec::with_capacity(args.len() + 1);
            pcase_let_star_forms.push(Value::symbol("pcase-let*"));
            pcase_let_star_forms.push(Value::Nil);
            pcase_let_star_forms.extend_from_slice(&args[1..]);
            Ok(Some(Value::list(vec![
                Value::symbol("let"),
                Value::list(symbol_bindings),
                Value::list(pcase_let_star_forms),
            ])))
        }
        "pcase-dolist" => {
            if args.is_empty() {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![Value::cons(Value::Int(1), Value::Int(1)), Value::Int(0)],
                ));
            }

            let spec = match list_to_vec(&args[0]) {
                Some(spec) => spec,
                None => {
                    let mut cursor = args[0];
                    while let Value::Cons(cell) = cursor {
                        cursor = with_heap(|h| h.cons_cdr(cell));
                    }
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), cursor],
                    ));
                }
            };
            if !(2..=3).contains(&spec.len()) {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![
                        Value::cons(Value::Int(2), Value::Int(3)),
                        Value::Int(spec.len() as i64),
                    ],
                ));
            }

            let pattern = spec[0];
            let sequence = spec[1];
            let result_expr = if spec.len() == 3 { Some(spec[2]) } else { None };
            let tail_var = Value::symbol("tail");
            let binding = Value::list(vec![tail_var, sequence]);
            let step = Value::list(vec![
                Value::symbol("setq"),
                tail_var,
                Value::list(vec![Value::symbol("cdr"), tail_var]),
            ]);
            let inner = if pattern.as_symbol_name().is_some_and(|name| name != "_") {
                let value_binding = Value::list(vec![
                    pattern,
                    Value::list(vec![Value::symbol("car"), tail_var]),
                ]);
                let mut forms = Vec::with_capacity(args.len() + 3);
                forms.push(Value::symbol("let"));
                forms.push(Value::list(vec![value_binding]));
                forms.extend_from_slice(&args[1..]);
                forms.push(step);
                Value::list(forms)
            } else {
                let car_binding = Value::list(vec![
                    Value::symbol("x0"),
                    Value::list(vec![Value::symbol("car"), tail_var]),
                ]);
                let pcase_binding = Value::list(vec![pattern, Value::symbol("x0")]);
                let mut pcase_let_star_forms = Vec::with_capacity(args.len() + 1);
                pcase_let_star_forms.push(Value::symbol("pcase-let*"));
                pcase_let_star_forms.push(Value::list(vec![pcase_binding]));
                pcase_let_star_forms.extend_from_slice(&args[1..]);
                let pcase_let_star = Value::list(pcase_let_star_forms);
                Value::list(vec![
                    Value::symbol("let"),
                    Value::list(vec![car_binding]),
                    pcase_let_star,
                    step,
                ])
            };

            let loop_form = Value::list(vec![Value::symbol("while"), tail_var, inner]);

            let mut forms = Vec::with_capacity(4);
            forms.push(Value::symbol("let"));
            forms.push(Value::list(vec![binding]));
            forms.push(loop_form);
            if let Some(result) = result_expr {
                forms.push(result);
            }
            Ok(Some(Value::list(forms)))
        }
        _ => Ok(None),
    }
}

#[tracing::instrument(level = "trace", skip(runtime, environment), fields(head))]
fn macroexpand_once_with_environment<R: MacroexpandRuntime>(
    runtime: &mut R,
    form: Value,
    environment: Option<&Value>,
) -> Result<(Value, bool), Flow> {
    let Value::Cons(form_cell) = form else {
        return Ok((form, false));
    };
    let form_pair = read_cons(form_cell);
    let head = form_pair.car;
    let tail = form_pair.cdr;
    let Some(head_id) = symbol_id(&head) else {
        return Ok((form, false));
    };
    let head_name = resolve_sym(head_id);

    // NeoVM handles certain forms as evaluator special forms where the
    // Elisp macro definition would produce incompatible expansions.
    // For example, pcase.el defines `(defmacro pcase ...)` but NeoVM
    // handles `pcase` directly in Rust.  If we let macroexpand call the
    // Elisp pcase macro, it produces internal markers (`:pcase--succeed`,
    // `pcase--placeholder`) that the evaluator cannot process.
    // Only skip forms that have BOTH a Rust special form handler AND
    // a conflicting Elisp macro — not fallback macros like `when`/`unless`
    // which are intentionally expanded by macroexpand.
    if super::subr_info::is_evaluator_sf_skip_macroexpand(head_name) {
        return Ok((form, false));
    }

    let mut env_bound = false;
    let mut function = None;
    if let Some(env) = environment {
        if !env.is_list() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), *env],
            ));
        }
        if let Some(binding) = macroexpand_environment_binding_by_id(env, head_id) {
            env_bound = true;
            if !binding.is_nil() {
                function = Some(macroexpand_environment_callable(&binding)?);
            }
        }
    }
    if env_bound && function.is_none() {
        return Ok((form, false));
    }
    let mut resolved_name = head_name.to_string();
    let mut fallback_placeholder = false;
    if function.is_none() {
        if let Some((resolved_id, global)) = runtime.resolve_indirect_symbol_by_id(head_id) {
            let resolved = resolve_sym(resolved_id);
            // Check for Value::Macro (native macros) AND cons-cell macros
            // `(macro . fn)` — matches real Emacs eval.c which checks
            // `EQ (XCAR (def), Qmacro)`.
            let is_macro = matches!(global, Value::Macro(_))
                || (global.is_cons() && global.cons_car().is_symbol_named("macro"));
            if is_macro {
                fallback_placeholder = is_canonical_symbol_id(resolved_id)
                    && super::subr_info::has_fallback_macro(resolved)
                    && runtime.is_global_function_placeholder(resolved_id);
                resolved_name = resolved.to_string();
                function = Some(if global.is_cons() {
                    // Extract the function from (macro . fn)
                    global.cons_cdr()
                } else {
                    global
                });
            } else if super::autoload::is_autoload_value(&global) {
                // Like Emacs eval.c macroexpand: if the function cell is an
                // autoload, trigger the load and retry — the loaded file may
                // define a macro for this symbol.
                // Pass macro_only=Qmacro so we only load if the autoload's
                // TYPE field is `t` or `macro`.  This matches GNU Emacs
                // eval.c which calls Fautoload_do_load(def, sym, Qmacro).
                runtime.autoload_do_load_macro(global, value_from_symbol_id(head_id))?;
                // Re-check the function cell after loading
                if let Some((resolved_id2, global2)) =
                    runtime.resolve_indirect_symbol_by_id(head_id)
                {
                    let resolved2 = resolve_sym(resolved_id2);
                    let is_macro2 = matches!(global2, Value::Macro(_))
                        || (global2.is_cons() && global2.cons_car().is_symbol_named("macro"));
                    if is_macro2 {
                        fallback_placeholder = is_canonical_symbol_id(resolved_id2)
                            && super::subr_info::has_fallback_macro(resolved2)
                            && runtime.is_global_function_placeholder(resolved_id2);
                        resolved_name = resolved2.to_string();
                        function = Some(if global2.is_cons() {
                            global2.cons_cdr()
                        } else {
                            global2
                        });
                    }
                }
            }
        }
    }
    let Some(function) = function else {
        return Ok((form, false));
    };
    let args = list_to_vec(&tail)
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), tail]))?;
    if fallback_placeholder {
        if let Some(expanded) = macroexpand_known_fallback_macro(
            &mut || runtime.next_pcase_macroexpand_temp_symbol(),
            &resolved_name,
            &args,
        )? {
            return Ok((expanded, true));
        }
        return Ok((form, false));
    }
    let expanded = runtime.apply_macro_function(form, function, args)?;
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

pub(crate) fn builtin_macroexpand_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_macroexpand_with_runtime(eval, args)
}

pub(crate) fn builtin_indirect_function(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_indirect_function_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_indirect_function_in_obarray(
    obarray: &Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("indirect-function", &args, 1)?;
    expect_max_args("indirect-function", &args, 2)?;

    if let Some(symbol) = symbol_id(&args[0]) {
        if let Some(function) =
            resolve_indirect_symbol_by_id_in_obarray(obarray, symbol).map(|(_, value)| value)
        {
            return Ok(function);
        }
        return Ok(Value::Nil);
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

    if let Some(function) = super::subr_info::fallback_macro_value(current_name) {
        return Some(function);
    }

    if let Some(alias_target) = pure_builtin_symbol_alias_target(current_name) {
        return Some(Value::symbol(alias_target));
    }

    if super::subr_info::is_special_form(current_name)
        || super::subr_info::is_evaluator_callable_name(current_name)
        || super::builtin_registry::is_dispatch_builtin_name(current_name)
        || current_name.parse::<PureBuiltinId>().is_ok()
    {
        return Some(Value::Subr(symbol));
    }

    None
}

pub(crate) fn resolve_indirect_symbol_by_id_in_obarray(
    obarray: &Obarray,
    symbol: SymId,
) -> Option<(SymId, Value)> {
    let mut current = symbol;
    let mut seen = HashSet::new();

    loop {
        if !seen.insert(current) {
            return None;
        }

        let function = symbol_function_cell_in_obarray(obarray, current)?;
        if let Some(next) = symbol_id(&function) {
            if next == intern("nil") {
                return Some((next, Value::Nil));
            }
            current = next;
            continue;
        }
        return Some((current, function));
    }
}

pub(crate) fn resolve_indirect_symbol_by_id(
    eval: &super::eval::Evaluator,
    symbol: SymId,
) -> Option<(SymId, Value)> {
    resolve_indirect_symbol_by_id_in_obarray(eval.obarray(), symbol)
}

fn resolve_indirect_symbol_with_name(
    eval: &super::eval::Evaluator,
    name: &str,
) -> Option<(String, Value)> {
    resolve_indirect_symbol_by_id(eval, intern(name))
        .map(|(resolved, value)| (resolve_sym(resolved).to_string(), value))
}

pub(super) fn resolve_indirect_symbol(eval: &super::eval::Evaluator, name: &str) -> Option<Value> {
    resolve_indirect_symbol_with_name(eval, name).map(|(_, value)| value)
}

pub(crate) fn builtin_macrop_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("macrop", &args, 1)?;
    if let Some(symbol) = symbol_id(&args[0]) {
        if is_canonical_symbol_id(symbol) {
            if let Some(function) =
                startup_virtual_autoload_function_cell(eval, resolve_sym(symbol))
            {
                return super::subr_info::builtin_macrop(vec![function]);
            }
        }
        if let Some(function) = resolve_indirect_symbol_by_id(eval, symbol).map(|(_, value)| value)
        {
            return super::subr_info::builtin_macrop(vec![function]);
        }
        return Ok(Value::Nil);
    }

    super::subr_info::builtin_macrop(args)
}

/// Hash a string for custom obarray bucket index.
pub(crate) fn obarray_hash(s: &str, len: usize) -> usize {
    let hash = s
        .bytes()
        .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
    hash as usize % len
}

/// Search a bucket chain (cons list) for a symbol with the given name.
/// Returns the symbol Value if found.
pub(crate) fn obarray_bucket_find(bucket: Value, name: &str) -> Option<Value> {
    let mut current = bucket;
    loop {
        match current {
            Value::Nil => return None,
            Value::Cons(id) => {
                let (car, cdr) = with_heap(|h| (h.cons_car(id), h.cons_cdr(id)));
                if let Some(sym_name) = car.as_symbol_name() {
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

pub(crate) fn is_global_obarray_proxy(eval: &super::eval::Evaluator, value: &Value) -> bool {
    eval.obarray()
        .symbol_value("obarray")
        .is_some_and(|proxy| *proxy == *value)
}

pub(crate) fn builtin_intern_fn(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("intern", &args, 1)?;
    expect_max_args("intern", &args, 2)?;
    if let Some(obarray) = args.get(1) {
        if !obarray.is_nil() && !matches!(obarray, Value::Vector(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("obarrayp"), *obarray],
            ));
        }
    }
    let name = expect_string(&args[0])?;

    // Custom obarray path
    if let Some(Value::Vector(vec_id)) = args
        .get(1)
        .filter(|v| !v.is_nil() && !is_global_obarray_proxy(eval, v))
    {
        let vec_id = *vec_id;
        let vec_len = with_heap(|h| h.get_vector(vec_id).len());
        if vec_len == 0 {
            return Err(signal("args-out-of-range", vec![Value::Int(0)]));
        }
        let bucket_idx = obarray_hash(&name, vec_len);
        let bucket = with_heap(|h| h.get_vector(vec_id)[bucket_idx]);

        // Check if already interned
        if let Some(sym) = obarray_bucket_find(bucket, &name) {
            return Ok(sym);
        }

        // Not found: create symbol and prepend to bucket chain
        let sym = Value::Symbol(intern_uninterned(&name));
        let new_bucket = Value::cons(sym, bucket);
        with_heap_mut(|h| {
            h.get_vector_mut(vec_id)[bucket_idx] = new_bucket;
        });
        return Ok(sym);
    }

    // Global obarray path
    eval.obarray_mut().intern(&name);
    Ok(Value::symbol(name))
}

pub(crate) fn builtin_intern_soft(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(obarray) = args.get(1).filter(|v| !v.is_nil()) {
        if is_global_obarray_proxy(eval, obarray) {
            let mut global_args = args;
            global_args.truncate(1);
            return builtin_intern_soft_in_obarray(eval.obarray(), global_args);
        }
    }
    builtin_intern_soft_in_obarray(eval.obarray(), args)
}

pub(crate) fn builtin_intern_soft_in_obarray(obarray: &Obarray, args: Vec<Value>) -> EvalResult {
    expect_min_args("intern-soft", &args, 1)?;
    expect_max_args("intern-soft", &args, 2)?;
    if let Some(obarray) = args.get(1) {
        if !obarray.is_nil() && !matches!(obarray, Value::Vector(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("obarrayp"), *obarray],
            ));
        }
    }

    // Custom obarray path
    if let Some(Value::Vector(vec_id)) = args.get(1).filter(|v| !v.is_nil()) {
        let vec_id = *vec_id;
        let name = match &args[0] {
            Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
            Value::Symbol(id) | Value::Keyword(id) => resolve_sym(*id).to_owned(),
            Value::Nil => "nil".to_owned(),
            Value::True => "t".to_owned(),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *other],
                ));
            }
        };
        let vec_len = with_heap(|h| h.get_vector(vec_id).len());
        if vec_len == 0 {
            return Ok(Value::Nil);
        }
        let bucket_idx = obarray_hash(&name, vec_len);
        let bucket = with_heap(|h| h.get_vector(vec_id)[bucket_idx]);
        return Ok(obarray_bucket_find(bucket, &name).unwrap_or(Value::Nil));
    }

    // Global obarray path
    let name = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        Value::Nil => "nil".to_owned(),
        Value::True => "t".to_owned(),
        Value::Keyword(id) | Value::Symbol(id) => resolve_sym(*id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    if obarray.intern_soft(&name).is_some() {
        Ok(Value::symbol(name))
    } else {
        Ok(Value::Nil)
    }
}

pub(crate) fn builtin_obarray_make(args: Vec<Value>) -> EvalResult {
    expect_range_args("obarray-make", &args, 0, 1)?;
    let size = if args.is_empty() || args[0].is_nil() {
        1511usize
    } else {
        expect_wholenump(&args[0])? as usize
    };
    Ok(Value::vector(vec![Value::Nil; size]))
}

pub(crate) fn expect_obarray_vector_id(value: &Value) -> Result<ObjId, Flow> {
    let Value::Vector(id) = value else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("obarrayp"), *value],
        ));
    };
    let is_obarray = with_heap(|h| {
        h.get_vector(*id)
            .iter()
            .all(|slot| slot.is_nil() || matches!(slot, Value::Cons(_)))
    });
    if !is_obarray {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("obarrayp"), *value],
        ));
    }
    Ok(*id)
}

pub(crate) fn builtin_obarray_clear(args: Vec<Value>) -> EvalResult {
    expect_args("obarray-clear", &args, 1)?;
    let id = expect_obarray_vector_id(&args[0])?;
    with_heap_mut(|h| {
        let vec = h.get_vector_mut(id);
        for slot in vec.iter_mut() {
            *slot = Value::Nil;
        }
    });
    Ok(Value::Nil)
}

pub(crate) fn builtin_make_temp_file_internal(args: Vec<Value>) -> EvalResult {
    expect_args("make-temp-file-internal", &args, 4)?;
    if !args[3].is_nil() {
        // MODE is currently accepted for arity and type compatibility.
        let _ = expect_fixnum(&args[3])?;
    }
    super::fileio::builtin_make_temp_file(vec![args[0], args[1], args[2]])
}

pub(crate) fn builtin_minibuffer_innermost_command_loop_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("minibuffer-innermost-command-loop-p", &args, 0, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_minibuffer_prompt_end(args: Vec<Value>) -> EvalResult {
    expect_args("minibuffer-prompt-end", &args, 0)?;
    Ok(Value::Int(1))
}

pub(crate) fn builtin_next_frame(args: Vec<Value>) -> EvalResult {
    expect_range_args("next-frame", &args, 0, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_next_frame_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_next_frame_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_next_frame_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("next-frame", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() {
            let _ = super::window_cmds::resolve_frame_id_in_state(
                frames,
                buffers,
                Some(frame),
                "frame-live-p",
            )?;
        }
    }
    super::window_cmds::builtin_selected_frame_in_state(frames, buffers, Vec::new())
}

pub(crate) fn builtin_previous_frame(args: Vec<Value>) -> EvalResult {
    expect_range_args("previous-frame", &args, 0, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_previous_frame_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_previous_frame_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_previous_frame_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("previous-frame", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() {
            let _ = super::window_cmds::resolve_frame_id_in_state(
                frames,
                buffers,
                Some(frame),
                "frame-live-p",
            )?;
        }
    }
    super::window_cmds::builtin_selected_frame_in_state(frames, buffers, Vec::new())
}

pub(crate) fn builtin_raise_frame(args: Vec<Value>) -> EvalResult {
    expect_range_args("raise-frame", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !matches!(frame, Value::Frame(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_redisplay(args: Vec<Value>) -> EvalResult {
    expect_range_args("redisplay", &args, 0, 1)?;
    Ok(Value::True)
}

pub(crate) fn builtin_redisplay_eval(
    eval: &mut crate::emacs_core::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("redisplay", &args, 0, 1)?;
    if eval
        .eval_symbol("executing-kbd-macro")
        .is_ok_and(|value| !value.is_nil())
    {
        return Ok(Value::Nil);
    }
    eval.redisplay();
    Ok(Value::True)
}

pub(crate) fn builtin_suspend_emacs(args: Vec<Value>) -> EvalResult {
    expect_range_args("suspend-emacs", &args, 0, 1)?;
    Ok(Value::Nil)
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_vertical_motion_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_vertical_motion_in_buffers(
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("vertical-motion", &args, 1, 3)?;
    // First arg can be LINES (integer) or (COLS . LINES) cons pair.
    // When (COLS . LINES), move LINES then position at column COLS.
    let (cols, lines) = match args[0] {
        Value::Int(n) => (None, n),
        Value::Cons(cell) => {
            let pair = super::value::read_cons(cell);
            let cols_val = match pair.car {
                Value::Int(n) => Some(n),
                Value::Float(f, _) => Some(f as i64),
                _ => None,
            };
            let lines_val = match pair.cdr {
                Value::Int(n) => n,
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("fixnump"), pair.cdr],
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
        if !window.is_nil() && !matches!(window, Value::Window(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *window],
            ));
        }
    }

    let Some(current_id) = buffers.current_buffer_id() else {
        return Ok(Value::Int(0));
    };
    let Some(buf) = buffers.get(current_id) else {
        return Ok(Value::Int(0));
    };
    let text = buf.text.to_string();
    let pt = buf.pt.clamp(buf.begv, buf.zv);
    let bytes = text.as_bytes();
    let begv = buf.begv;
    let zv = buf.zv;

    if lines == 0 && cols.is_none() {
        // Move to beginning of current screen line (= beginning of line).
        let mut bol = pt;
        while bol > begv && bytes[bol - 1] != b'\n' {
            bol -= 1;
        }
        let _ = buffers.goto_buffer_byte(current_id, bol);
        return Ok(Value::Int(0));
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
    Ok(Value::Int(moved))
}

pub(crate) fn builtin_rename_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_rename_buffer_in_manager(&mut eval.buffers, args)
}

pub(crate) fn builtin_rename_buffer_in_manager(
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
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

    let unique = args.get(1).copied().unwrap_or(Value::Nil);

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

    let _ = buffers.set_buffer_name(current_id, new_name.clone());

    Ok(Value::string(new_name))
}

pub(crate) fn builtin_set_buffer_major_mode(args: Vec<Value>) -> EvalResult {
    expect_args("set-buffer-major-mode", &args, 1)?;
    let _ = expect_buffer_id(&args[0])?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_set_buffer_redisplay(args: Vec<Value>) -> EvalResult {
    expect_args("set-buffer-redisplay", &args, 4)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_put_unicode_property_internal(args: Vec<Value>) -> EvalResult {
    expect_args("put-unicode-property-internal", &args, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_re_describe_compiled(args: Vec<Value>) -> EvalResult {
    expect_range_args("re--describe-compiled", &args, 1, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_map_charset_chars(args: Vec<Value>) -> EvalResult {
    expect_range_args("map-charset-chars", &args, 2, 5)?;
    Ok(Value::Nil)
}

// map-keymap and map-keymap-internal are now eval-backed in keymaps.rs

pub(crate) fn builtin_mapbacktrace(args: Vec<Value>) -> EvalResult {
    expect_range_args("mapbacktrace", &args, 1, 2)?;
    match &args[0] {
        Value::Nil | Value::True => {
            return Err(signal("void-function", vec![args[0]]));
        }
        Value::Symbol(_)
        | Value::Subr(_)
        | Value::Lambda(_)
        | Value::Macro(_)
        | Value::ByteCode(_) => {}
        _ => {
            return Err(signal("invalid-function", vec![args[0]]));
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_make_record(args: Vec<Value>) -> EvalResult {
    expect_args("make-record", &args, 3)?;
    let length = expect_wholenump(&args[1])? as usize;
    let mut items = Vec::with_capacity(length + 1);
    items.push(args[0]); // type tag
    for _ in 0..length {
        items.push(args[2]); // init value
    }
    let id = with_heap_mut(|h| h.alloc_vector(items));
    Ok(Value::Record(id))
}

pub(crate) fn builtin_marker_last_position(args: Vec<Value>) -> EvalResult {
    expect_args("marker-last-position", &args, 1)?;
    if !super::marker::is_marker(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("markerp"), args[0]],
        ));
    }
    match &args[0] {
        Value::Vector(vec) => {
            let items = with_heap(|h| h.get_vector(*vec).clone());
            if let Some(Value::Int(pos)) = items.get(2) {
                Ok(Value::Int(*pos))
            } else {
                Ok(Value::Int(0))
            }
        }
        _ => unreachable!("markerp check above guarantees marker vector"),
    }
}

pub(crate) fn builtin_newline_cache_check(args: Vec<Value>) -> EvalResult {
    expect_range_args("newline-cache-check", &args, 0, 1)?;
    if let Some(buffer) = args.first() {
        if !buffer.is_nil() && !matches!(buffer, Value::Buffer(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("bufferp"), *buffer],
            ));
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_old_selected_frame(args: Vec<Value>) -> EvalResult {
    expect_args("old-selected-frame", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_old_selected_frame_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_old_selected_frame_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_old_selected_frame_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("old-selected-frame", &args, 0)?;
    super::window_cmds::builtin_selected_frame_in_state(frames, buffers, Vec::new())
}

pub(crate) fn builtin_make_frame_invisible(args: Vec<Value>) -> EvalResult {
    expect_range_args("make-frame-invisible", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !matches!(frame, Value::Frame(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    let force = args.get(1).is_some_and(|arg| !arg.is_nil());
    if force {
        return Ok(Value::Nil);
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
    Ok(Value::Nil)
}

pub(crate) fn builtin_menu_or_popup_active_p(args: Vec<Value>) -> EvalResult {
    expect_args("menu-or-popup-active-p", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_mouse_pixel_position(args: Vec<Value>) -> EvalResult {
    expect_args("mouse-pixel-position", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_mouse_pixel_position_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_mouse_pixel_position_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_mouse_pixel_position_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("mouse-pixel-position", &args, 0)?;
    let frame = super::window_cmds::builtin_selected_frame_in_state(frames, buffers, Vec::new())?;
    Ok(Value::list(vec![frame, Value::Nil]))
}

pub(crate) fn builtin_mouse_position(args: Vec<Value>) -> EvalResult {
    expect_args("mouse-position", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_mouse_position_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_mouse_position_in_state(&mut eval.frames, &mut eval.buffers, args)
}

pub(crate) fn builtin_mouse_position_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("mouse-position", &args, 0)?;
    let frame = super::window_cmds::builtin_selected_frame_in_state(frames, buffers, Vec::new())?;
    Ok(Value::list(vec![frame, Value::Nil]))
}

pub(crate) fn builtin_native_comp_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("native-comp-available-p", &args, 0)?;
    Ok(Value::True)
}

pub(crate) fn builtin_native_comp_unit_file(args: Vec<Value>) -> EvalResult {
    expect_args("native-comp-unit-file", &args, 1)?;
    let is_native_comp_unit = match &args[0] {
        Value::Vector(items) => {
            let items = with_heap(|h| h.get_vector(*items).clone());
            matches!(
                items.first(),
                Some(Value::Keyword(tag)) if resolve_sym(*tag) == "native-comp-unit"
            )
        }
        _ => false,
    };
    if !is_native_comp_unit {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("native-comp-unit"), args[0]],
        ));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_native_comp_unit_set_file(args: Vec<Value>) -> EvalResult {
    expect_args("native-comp-unit-set-file", &args, 2)?;
    let is_native_comp_unit = match &args[0] {
        Value::Vector(items) => {
            let items = with_heap(|h| h.get_vector(*items).clone());
            matches!(
                items.first(),
                Some(Value::Keyword(tag)) if resolve_sym(*tag) == "native-comp-unit"
            )
        }
        _ => false,
    };
    if !is_native_comp_unit {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("native-comp-unit"), args[0]],
        ));
    }
    Ok(Value::Nil)
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
    dynamic: &[OrderedRuntimeBindingMap],
    name: &str,
) -> Option<Value> {
    let name_id = intern(name);
    for frame in dynamic.iter().rev() {
        if let Some(value) = frame.get(&name_id) {
            return Some(*value);
        }
    }
    obarray.symbol_value(name).copied()
}

pub(crate) fn builtin_new_fontset(args: Vec<Value>) -> EvalResult {
    expect_args("new-fontset", &args, 2)?;
    let name = expect_strict_string(&args[0])?;
    let registered = fontset::new_fontset(&name, &args[1], None, None, None)?;
    Ok(Value::string(registered))
}

pub(crate) fn builtin_new_fontset_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    args: Vec<Value>,
) -> EvalResult {
    expect_args("new-fontset", &args, 2)?;
    let name = expect_strict_string(&args[0])?;
    let char_script_table =
        dynamic_or_global_symbol_value_in_state(obarray, dynamic, "char-script-table");
    let charset_script_alist =
        dynamic_or_global_symbol_value_in_state(obarray, dynamic, "charset-script-alist");
    let font_encoding_alist =
        dynamic_or_global_symbol_value_in_state(obarray, dynamic, "font-encoding-alist");
    let registered = fontset::new_fontset(
        &name,
        &args[1],
        char_script_table.as_ref(),
        charset_script_alist.as_ref(),
        font_encoding_alist.as_ref(),
    )?;
    Ok(Value::string(registered))
}

pub(crate) fn builtin_new_fontset_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_new_fontset_in_state(eval.obarray(), eval.dynamic.as_slice(), args)
}

pub(crate) fn builtin_open_font(args: Vec<Value>) -> EvalResult {
    expect_range_args("open-font", &args, 1, 3)?;
    let is_font_entity = match &args[0] {
        Value::Vector(items) => {
            let items = with_heap(|h| h.get_vector(*items).clone());
            matches!(
                items.first(),
                Some(Value::Keyword(tag)) if resolve_sym(*tag) == "font-entity"
            )
        }
        _ => false,
    };
    if !is_font_entity {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-entity"), args[0]],
        ));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_open_dribble_file(args: Vec<Value>) -> EvalResult {
    expect_args("open-dribble-file", &args, 1)?;
    if !args[0].is_nil() {
        let _ = expect_strict_string(&args[0])?;
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_object_intervals(args: Vec<Value>) -> EvalResult {
    expect_args("object-intervals", &args, 1)?;
    if !matches!(args[0], Value::Str(_) | Value::Buffer(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("buffer-or-string-p"), args[0]],
        ));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_optimize_char_table(args: Vec<Value>) -> EvalResult {
    expect_range_args("optimize-char-table", &args, 1, 2)?;
    if !super::chartable::is_char_table(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("char-table-p"), args[0]],
        ));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_overlay_lists(args: Vec<Value>) -> EvalResult {
    expect_args("overlay-lists", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_overlay_recenter(args: Vec<Value>) -> EvalResult {
    expect_args("overlay-recenter", &args, 1)?;
    let _ = expect_integer_or_marker(&args[0])?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_profiler_cpu_log(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-cpu-log", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_profiler_cpu_running_p(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-cpu-running-p", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_profiler_cpu_start(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-cpu-start", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_profiler_cpu_stop(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-cpu-stop", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_profiler_memory_log(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-memory-log", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_profiler_memory_running_p(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-memory-running-p", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_profiler_memory_start(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-memory-start", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_profiler_memory_stop(args: Vec<Value>) -> EvalResult {
    expect_args("profiler-memory-stop", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_pdumper_stats(args: Vec<Value>) -> EvalResult {
    expect_args("pdumper-stats", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_position_symbol(args: Vec<Value>) -> EvalResult {
    expect_args("position-symbol", &args, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_posn_at_point(args: Vec<Value>) -> EvalResult {
    expect_range_args("posn-at-point", &args, 0, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_posn_at_x_y(args: Vec<Value>) -> EvalResult {
    expect_range_args("posn-at-x-y", &args, 2, 4)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_play_sound_internal(args: Vec<Value>) -> EvalResult {
    expect_args("play-sound-internal", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_record(args: Vec<Value>) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("record"), Value::Int(0)],
        ));
    }
    let id = with_heap_mut(|h| h.alloc_vector(args));
    Ok(Value::Record(id))
}

pub(crate) fn builtin_recordp(args: Vec<Value>) -> EvalResult {
    expect_args("recordp", &args, 1)?;
    Ok(Value::bool(args[0].is_record()))
}

pub(crate) fn builtin_query_font(args: Vec<Value>) -> EvalResult {
    expect_args("query-font", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_query_fontset(args: Vec<Value>) -> EvalResult {
    expect_range_args("query-fontset", &args, 1, 2)?;
    let pattern = expect_strict_string(&args[0])?;
    if pattern.is_empty() {
        return Ok(Value::Nil);
    }
    let regexpp = args.get(1).is_some_and(Value::is_truthy);
    Ok(fontset::query_fontset_registry(&pattern, regexpp).map_or(Value::Nil, Value::string))
}

pub(crate) fn builtin_read_positioning_symbols(args: Vec<Value>) -> EvalResult {
    expect_range_args("read-positioning-symbols", &args, 0, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_recent_auto_save_p(args: Vec<Value>) -> EvalResult {
    expect_args("recent-auto-save-p", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_reconsider_frame_fonts(args: Vec<Value>) -> EvalResult {
    expect_args("reconsider-frame-fonts", &args, 1)?;
    if !args[0].is_nil() && !matches!(args[0], Value::Frame(_)) {
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
    Ok(Value::Nil)
}

pub(crate) fn builtin_redirect_frame_focus(args: Vec<Value>) -> EvalResult {
    expect_range_args("redirect-frame-focus", &args, 1, 2)?;
    if !args[0].is_nil() && !matches!(args[0], Value::Frame(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("framep"), args[0]],
        ));
    }
    if let Some(focus_frame) = args.get(1) {
        if !focus_frame.is_nil() && !matches!(focus_frame, Value::Frame(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *focus_frame],
            ));
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_remove_pos_from_symbol(args: Vec<Value>) -> EvalResult {
    expect_args("remove-pos-from-symbol", &args, 1)?;
    Ok(args[0])
}

pub(crate) fn builtin_resize_mini_window_internal(args: Vec<Value>) -> EvalResult {
    expect_args("resize-mini-window-internal", &args, 1)?;
    match args[0] {
        Value::Window(id) if id >= crate::window::MINIBUFFER_WINDOW_ID_BASE => Err(signal(
            "error",
            vec![Value::string("Cannot resize mini window")],
        )),
        Value::Window(_) => Err(signal(
            "error",
            vec![Value::string("Not a valid minibuffer window")],
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-live-p"), args[0]],
        )),
    }
}

pub(crate) fn builtin_restore_buffer_modified_p(args: Vec<Value>) -> EvalResult {
    expect_args("restore-buffer-modified-p", &args, 1)?;
    Ok(args[0])
}

pub(crate) fn builtin_set_this_command_keys(args: Vec<Value>) -> EvalResult {
    expect_args("set--this-command-keys", &args, 1)?;
    let _ = expect_strict_string(&args[0])?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_set_buffer_auto_saved(args: Vec<Value>) -> EvalResult {
    expect_args("set-buffer-auto-saved", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_set_charset_plist(args: Vec<Value>) -> EvalResult {
    expect_args("set-charset-plist", &args, 2)?;
    let name = match &args[0] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), *other],
            ));
        }
    };
    // Parse the plist argument into (key, value) pairs and store it.
    let mut plist_pairs = Vec::new();
    if let Some(items) = list_to_vec(&args[1]) {
        let mut i = 0;
        while i + 1 < items.len() {
            if let Some(key) = items[i].as_symbol_name() {
                plist_pairs.push((key.to_string(), items[i + 1]));
            }
            i += 2;
        }
    }
    super::charset::set_charset_plist_registry(&name, plist_pairs);
    Ok(args[1])
}

pub(crate) fn builtin_set_fontset_font(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-fontset-font", &args, 3, 5)?;
    fontset::set_fontset_font(&args[0], &args[1], &args[2], args.get(4), None, None, None)
}

pub(crate) fn builtin_set_fontset_font_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("set-fontset-font", &args, 3, 5)?;
    let char_script_table =
        dynamic_or_global_symbol_value_in_state(obarray, dynamic, "char-script-table");
    let charset_script_alist =
        dynamic_or_global_symbol_value_in_state(obarray, dynamic, "charset-script-alist");
    let font_encoding_alist =
        dynamic_or_global_symbol_value_in_state(obarray, dynamic, "font-encoding-alist");
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

pub(crate) fn builtin_set_fontset_font_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_fontset_font_in_state(eval.obarray(), eval.dynamic.as_slice(), args)
}

pub(crate) fn builtin_set_frame_window_state_change(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-frame-window-state-change", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !matches!(frame, Value::Frame(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(Value::Nil)
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
    Ok(Value::Nil)
}

pub(crate) fn builtin_set_minibuffer_window(args: Vec<Value>) -> EvalResult {
    expect_args("set-minibuffer-window", &args, 1)?;
    match args[0] {
        Value::Window(id) if id >= crate::window::MINIBUFFER_WINDOW_ID_BASE => Ok(Value::Nil),
        Value::Window(_) => Err(signal(
            "error",
            vec![Value::string("Window is not a minibuffer window")],
        )),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("windowp"), args[0]],
        )),
    }
}

pub(crate) fn builtin_set_mouse_pixel_position(args: Vec<Value>) -> EvalResult {
    expect_args("set-mouse-pixel-position", &args, 3)?;
    if !matches!(args[0], Value::Frame(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), args[0]],
        ));
    }
    let _ = expect_int(&args[1])?;
    let _ = expect_int(&args[2])?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_set_mouse_position(args: Vec<Value>) -> EvalResult {
    expect_args("set-mouse-position", &args, 3)?;
    if !matches!(args[0], Value::Frame(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), args[0]],
        ));
    }
    let _ = expect_int(&args[1])?;
    let _ = expect_int(&args[2])?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_set_window_combination_limit(args: Vec<Value>) -> EvalResult {
    expect_args("set-window-combination-limit", &args, 2)?;
    if !matches!(args[0], Value::Window(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-valid-p"), args[0]],
        ));
    }
    Err(signal(
        "error",
        vec![Value::string(
            "Combination limit is meaningful for internal windows only",
        )],
    ))
}

pub(crate) fn builtin_set_window_new_normal(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-window-new-normal", &args, 1, 2)?;
    expect_window_valid_or_nil(&args[0])?;
    Ok(super::stubs::set_window_new_normal_value(
        &args[0],
        args.get(1).cloned().unwrap_or(Value::Nil),
    ))
}

pub(crate) fn builtin_set_window_new_pixel(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-window-new-pixel", &args, 2, 3)?;
    expect_window_valid_or_nil(&args[0])?;
    let size = expect_int(&args[1])?;
    Ok(super::stubs::set_window_new_pixel_value(
        &args[0],
        size,
        args.get(2).is_some_and(Value::is_truthy),
    ))
}

pub(crate) fn builtin_set_window_new_total(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-window-new-total", &args, 2, 3)?;
    expect_window_valid_or_nil(&args[0])?;
    let size = expect_fixnum(&args[1])?;
    Ok(super::stubs::set_window_new_total_value(
        &args[0],
        size,
        args.get(2).is_some_and(Value::is_truthy),
    ))
}

pub(crate) fn builtin_sort_charsets(args: Vec<Value>) -> EvalResult {
    expect_args("sort-charsets", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_split_char(args: Vec<Value>) -> EvalResult {
    expect_args("split-char", &args, 1)?;
    Ok(Value::Nil)
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
        Ok(Value::Int(dist as i64))
    } else {
        // Character-level Levenshtein distance
        let c1: Vec<char> = s1.chars().collect();
        let c2: Vec<char> = s2.chars().collect();
        let dist = levenshtein_distance_chars(&c1, &c2);
        Ok(Value::Int(dist as i64))
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
    Ok(Value::Nil)
}

pub(crate) fn builtin_subr_native_lambda_list(args: Vec<Value>) -> EvalResult {
    expect_args("subr-native-lambda-list", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_subr_type(args: Vec<Value>) -> EvalResult {
    expect_args("subr-type", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_this_single_command_keys(args: Vec<Value>) -> EvalResult {
    expect_args("this-single-command-keys", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_this_single_command_raw_keys(args: Vec<Value>) -> EvalResult {
    expect_args("this-single-command-raw-keys", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_thread_blocker(args: Vec<Value>) -> EvalResult {
    expect_args("thread--blocker", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_tool_bar_get_system_style(args: Vec<Value>) -> EvalResult {
    expect_args("tool-bar-get-system-style", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_tool_bar_pixel_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("tool-bar-pixel-width", &args, 0, 1)?;
    Ok(Value::Int(0))
}

pub(crate) fn builtin_translate_region_internal(args: Vec<Value>) -> EvalResult {
    expect_args("translate-region-internal", &args, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_transpose_regions(args: Vec<Value>) -> EvalResult {
    expect_range_args("transpose-regions", &args, 4, 5)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_tty_output_buffer_size(args: Vec<Value>) -> EvalResult {
    expect_range_args("tty--output-buffer-size", &args, 0, 1)?;
    Ok(Value::Int(0))
}

pub(crate) fn builtin_tty_set_output_buffer_size(args: Vec<Value>) -> EvalResult {
    expect_range_args("tty--set-output-buffer-size", &args, 1, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_tty_suppress_bold_inverse_default_colors(args: Vec<Value>) -> EvalResult {
    expect_args("tty-suppress-bold-inverse-default-colors", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_unencodable_char_position(args: Vec<Value>) -> EvalResult {
    expect_range_args("unencodable-char-position", &args, 3, 5)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_unicode_property_table_internal(args: Vec<Value>) -> EvalResult {
    expect_args("unicode-property-table-internal", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_unify_charset(args: Vec<Value>) -> EvalResult {
    expect_range_args("unify-charset", &args, 1, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_unix_sync(args: Vec<Value>) -> EvalResult {
    expect_args("unix-sync", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_value_lt(args: Vec<Value>) -> EvalResult {
    expect_args("value<", &args, 2)?;
    match compare_value_lt(&args[0], &args[1]) {
        Ok(std::cmp::Ordering::Less) => Ok(Value::True),
        Ok(_) => Ok(Value::Nil),
        Err((lhs, rhs)) => Err(signal("type-mismatch", vec![lhs, rhs])),
    }
}

pub(crate) fn builtin_variable_binding_locus(args: Vec<Value>) -> EvalResult {
    expect_args("variable-binding-locus", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn compare_value_lt(
    lhs: &Value,
    rhs: &Value,
) -> Result<std::cmp::Ordering, (Value, Value)> {
    if let (Some(left), Some(right)) = (as_number_for_value_lt(lhs), as_number_for_value_lt(rhs)) {
        return Ok(left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal));
    }

    if let (Some(left), Some(right)) =
        (symbol_name_for_value_lt(lhs), symbol_name_for_value_lt(rhs))
    {
        return Ok(left.cmp(right));
    }

    match (lhs, rhs) {
        (Value::Str(left_id), Value::Str(right_id)) => Ok(with_heap(|h| {
            h.get_string(*left_id).cmp(h.get_string(*right_id))
        })),
        (Value::Cons(left_id), Value::Cons(right_id)) => {
            let left_pair = read_cons(*left_id);
            let right_pair = read_cons(*right_id);

            let car_cmp = compare_value_lt(&left_pair.car, &right_pair.car)?;
            if car_cmp != std::cmp::Ordering::Equal {
                return Ok(car_cmp);
            }

            match (&left_pair.cdr, &right_pair.cdr) {
                (Value::Nil, Value::Cons(_)) => Ok(std::cmp::Ordering::Less),
                (Value::Cons(_), Value::Nil) => Ok(std::cmp::Ordering::Greater),
                _ => compare_value_lt(&left_pair.cdr, &right_pair.cdr),
            }
        }
        (Value::Vector(left_id), Value::Vector(right_id)) => {
            let (pairs, left_len, right_len) = with_heap(|h| {
                let lv = h.get_vector(*left_id);
                let rv = h.get_vector(*right_id);
                let pairs: Vec<(Value, Value)> =
                    lv.iter().copied().zip(rv.iter().copied()).collect();
                (pairs, lv.len(), rv.len())
            });
            for (l, r) in &pairs {
                let cmp = compare_value_lt(l, r)?;
                if cmp != std::cmp::Ordering::Equal {
                    return Ok(cmp);
                }
            }
            Ok(left_len.cmp(&right_len))
        }
        _ => Err((*lhs, *rhs)),
    }
}

fn as_number_for_value_lt(value: &Value) -> Option<f64> {
    match value {
        Value::Int(n) => Some(*n as f64),
        Value::Char(c) => Some(*c as u32 as f64),
        Value::Float(f, _) => Some(*f),
        _ => None,
    }
}

fn symbol_name_for_value_lt(value: &Value) -> Option<&str> {
    match value {
        Value::Nil => Some("nil"),
        Value::True => Some("t"),
        Value::Symbol(id) => Some(resolve_sym(*id)),
        Value::Keyword(id) => Some(resolve_sym(*id)),
        _ => None,
    }
}

pub(crate) fn builtin_variable_binding_locus_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_variable_binding_locus_in_state(eval.obarray(), &eval.buffers, args)
}

pub(crate) fn builtin_variable_binding_locus_in_state(
    obarray: &Obarray,
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("variable-binding-locus", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name_in_obarray(obarray, name)?;
    if resolved == "nil" || resolved == "t" || resolved.starts_with(':') {
        return Ok(Value::Nil);
    }
    if let Some(buf) = buffers.current_buffer() {
        if buf.has_buffer_local(&resolved) {
            return Ok(Value::Buffer(buf.id));
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_x_begin_drag(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-begin-drag", &args, 1, 6)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_x_double_buffered_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-double-buffered-p", &args, 0, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_x_menu_bar_open_internal(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-menu-bar-open-internal", &args, 0, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_xw_color_defined_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("xw-color-defined-p", &args, 1, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_xw_color_values(args: Vec<Value>) -> EvalResult {
    expect_range_args("xw-color-values", &args, 1, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_xw_display_color_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("xw-display-color-p", &args, 0, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_xw_display_color_p_eval(
    eval: &super::super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("xw-display-color-p", &args, 0, 1)?;
    if super::super::display::display_window_system_symbol_in_state(
        &eval.frames,
        &eval.obarray,
        &eval.dynamic,
        args.first(),
    )?
    .is_some_and(super::super::display::gui_window_system_active_value)
    {
        Ok(Value::True)
    } else {
        Ok(Value::Nil)
    }
}

pub(crate) fn builtin_xw_display_color_p_in_state(
    frames: &crate::window::FrameManager,
    obarray: &crate::emacs_core::symbol::Obarray,
    dynamic: &[crate::emacs_core::value::OrderedRuntimeBindingMap],
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("xw-display-color-p", &args, 0, 1)?;
    if let Some(display) = args.first() {
        super::super::display::expect_display_designator_in_state(frames, display)?;
    }
    if super::super::display::display_window_system_symbol_in_state(
        frames,
        obarray,
        dynamic,
        args.first(),
    )?
    .is_some_and(super::super::display::gui_window_system_active_value)
    {
        Ok(Value::True)
    } else {
        Ok(Value::Nil)
    }
}

pub(crate) fn builtin_innermost_minibuffer_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("innermost-minibuffer-p", &args, 0, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_interactive_form(args: Vec<Value>) -> EvalResult {
    expect_args("interactive-form", &args, 1)?;
    if args[0].as_symbol_name() == Some("ignore") {
        return Ok(Value::list(vec![Value::symbol("interactive"), Value::Nil]));
    }
    Ok(Value::Nil)
}

fn interactive_form_from_expr_body(body: &[super::expr::Expr]) -> Option<Value> {
    fn expr_is_declare_form(expr: &super::expr::Expr) -> bool {
        matches!(
            expr,
            super::expr::Expr::List(items)
                if matches!(items.first(), Some(super::expr::Expr::Symbol(head_id)) if resolve_sym(*head_id) == "declare")
        )
    }

    let mut index = 0;
    if matches!(body.first(), Some(super::expr::Expr::Str(_))) {
        index = 1;
    }
    while body.get(index).is_some_and(expr_is_declare_form) {
        index += 1;
    }

    for expr in &body[index..] {
        let super::expr::Expr::List(items) = expr else {
            continue;
        };
        let super::expr::Expr::Symbol(head_id) = items.first()? else {
            continue;
        };
        if resolve_sym(*head_id) != "interactive" {
            continue;
        }
        let mut interactive = vec![Value::symbol("interactive")];
        match items.get(1).map(super::eval::quote_to_value) {
            Some(spec) => interactive.push(spec),
            None => interactive.push(Value::Nil),
        }
        return Some(Value::list(interactive));
    }

    None
}

fn interactive_form_from_quoted_interactive_form(form: &Value) -> Result<Option<Value>, Flow> {
    let Value::Cons(cell) = form else {
        return Ok(None);
    };
    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("interactive") {
        return Ok(None);
    }

    match pair.cdr {
        Value::Nil => Ok(Some(Value::list(vec![
            Value::symbol("interactive"),
            Value::Nil,
        ]))),
        Value::Cons(arg_cell) => {
            let arg_pair = read_cons(arg_cell);
            Ok(Some(Value::list(vec![
                Value::symbol("interactive"),
                arg_pair.car,
            ])))
        }
        tail => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), tail],
        )),
    }
}

fn interactive_form_from_quoted_lambda(value: &Value) -> Result<Option<Value>, Flow> {
    let Value::Cons(lambda_cell) = value else {
        return Ok(None);
    };
    let lambda_pair = read_cons(*lambda_cell);
    if lambda_pair.car.as_symbol_name() != Some("lambda") {
        return Ok(None);
    }
    let Value::Cons(params_cell) = lambda_pair.cdr else {
        return Ok(None);
    };
    let params_pair = read_cons(params_cell);
    let body = params_pair.cdr;
    let mut cursor = body;
    let mut can_skip_doc = true;

    loop {
        match cursor {
            Value::Nil => return Ok(None),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                if can_skip_doc && matches!(pair.car, Value::Str(_)) {
                    can_skip_doc = false;
                    cursor = pair.cdr;
                    continue;
                }
                can_skip_doc = false;
                if let Some(interactive) = interactive_form_from_quoted_interactive_form(&pair.car)?
                {
                    return Ok(Some(interactive));
                }
                cursor = pair.cdr;
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
    let Value::ByteCode(id) = function else {
        return None;
    };
    let spec = with_heap(|h| h.get_bytecode(id).interactive);
    spec.map(|s| {
        let spec_val = if let Value::Vector(vid) = s {
            with_heap(|h| {
                if h.vector_len(vid) > 0 {
                    h.vector_ref(vid, 0)
                } else {
                    s
                }
            })
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
            return Ok(InteractiveFormPlan::Return(Value::Nil));
        };
        if indirect_function.is_nil() {
            return Ok(InteractiveFormPlan::Return(Value::Nil));
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
                return Ok(InteractiveFormPlan::Return(Value::Nil));
            };
            function = next_function;
            let Some(next_symbol) = symbol_id(&function) else {
                break;
            };
            current = next_symbol;
        }
    }

    match function {
        Value::Subr(id) => {
            let name = resolve_sym(id);
            Ok(InteractiveFormPlan::Return(
                crate::emacs_core::interactive::registry_interactive_form(interactive, name)
                    .or_else(|| crate::emacs_core::interactive::builtin_subr_interactive_form(name))
                    .unwrap_or(Value::Nil),
            ))
        }
        Value::Lambda(id) | Value::Macro(id) => {
            let body = with_heap(|h| h.get_lambda(id).body.clone());
            Ok(InteractiveFormPlan::Return(
                interactive_form_from_expr_body(&body).unwrap_or(Value::Nil),
            ))
        }
        Value::ByteCode(_) => Ok(InteractiveFormPlan::Return(
            interactive_form_from_bytecode_value(function).unwrap_or(Value::Nil),
        )),
        Value::Cons(_) if super::autoload::is_autoload_value(&function) => {
            Ok(InteractiveFormPlan::Autoload {
                fundef: function,
                funname: if symbol_id(&cmd).is_some() {
                    cmd
                } else {
                    Value::Nil
                },
            })
        }
        Value::Cons(_) => Ok(InteractiveFormPlan::Return(
            interactive_form_from_quoted_lambda(&function)?.unwrap_or(Value::Nil),
        )),
        _ => Ok(InteractiveFormPlan::Return(Value::Nil)),
    }
}

pub(crate) fn builtin_interactive_form_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("interactive-form", &args, 1)?;
    let mut target = args[0];
    loop {
        let plan = plan_interactive_form_in_state(&eval.obarray, &eval.interactive, target)?;
        match plan {
            InteractiveFormPlan::Return(value) => return Ok(value),
            InteractiveFormPlan::Autoload { fundef, funname } => {
                let mut load_args = vec![fundef];
                if !funname.is_nil() {
                    load_args.push(funname);
                }
                target = super::autoload::builtin_autoload_do_load(eval, load_args)?;
            }
        }
    }
}

pub(crate) fn builtin_local_variable_if_set_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("local-variable-if-set-p", &args, 1, 2)?;
    if args[0].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_local_variable_if_set_p_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_local_variable_if_set_p_in_state(eval.obarray(), &eval.custom, args)
}

pub(crate) fn builtin_local_variable_if_set_p_in_state(
    obarray: &Obarray,
    custom: &crate::emacs_core::custom::CustomManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("local-variable-if-set-p", &args, 1, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name_in_obarray(obarray, name)?;
    if resolved == "nil" || resolved == "t" || resolved.starts_with(':') {
        return Ok(Value::Nil);
    }
    Ok(Value::bool(
        obarray.is_buffer_local(&resolved) || custom.is_auto_buffer_local(&resolved),
    ))
}

pub(crate) fn builtin_lock_buffer(args: Vec<Value>) -> EvalResult {
    expect_range_args("lock-buffer", &args, 0, 1)?;
    if let Some(filename) = args.first() {
        if !filename.is_nil() {
            let _ = expect_strict_string(filename)?;
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_lock_file(args: Vec<Value>) -> EvalResult {
    expect_args("lock-file", &args, 1)?;
    let _ = expect_strict_string(&args[0])?;
    Ok(Value::Nil)
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
            let n = match value {
                Value::Int(n) => *n,
                Value::Char(c) => *c as i64,
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

    Ok(Value::Int(LOSSAGE_SIZE.with(|slot| *slot.borrow())))
}

pub(crate) fn builtin_unlock_buffer(args: Vec<Value>) -> EvalResult {
    expect_args("unlock-buffer", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_unlock_file(args: Vec<Value>) -> EvalResult {
    expect_args("unlock-file", &args, 1)?;
    let _ = expect_strict_string(&args[0])?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_internal_track_mouse(args: Vec<Value>) -> EvalResult {
    expect_args("internal--track-mouse", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_internal_char_font(args: Vec<Value>) -> EvalResult {
    expect_range_args("internal-char-font", &args, 1, 2)?;
    let position = &args[0];
    let ch = args.get(1).copied().unwrap_or(Value::Nil);

    if position.is_nil() {
        let _ = expect_character_code(&ch)?;
        return Ok(Value::Nil);
    }

    let _ = expect_integer_or_marker(position)?;
    if !ch.is_nil() {
        let _ = expect_character_code(&ch)?;
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_internal_complete_buffer(args: Vec<Value>) -> EvalResult {
    expect_args("internal-complete-buffer", &args, 3)?;
    let _ = expect_strict_string(&args[0])?;
    Ok(Value::Nil)
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

pub(crate) fn builtin_internal_handle_focus_in(args: Vec<Value>) -> EvalResult {
    expect_args("internal-handle-focus-in", &args, 1)?;
    Err(signal(
        "error",
        vec![Value::string("invalid focus-in event")],
    ))
}

pub(crate) fn builtin_internal_make_var_non_special_in_obarray(
    obarray: &mut crate::emacs_core::symbol::Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal-make-var-non-special", &args, 1)?;
    let symbol = expect_symbol_id(&args[0])?;
    obarray.make_non_special_id(symbol);
    Ok(Value::Nil)
}

pub(crate) fn builtin_internal_make_var_non_special_eval(
    eval: &mut crate::emacs_core::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_internal_make_var_non_special_in_obarray(eval.obarray_mut(), args)
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

    let attr_name = match &args[1] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        Value::Keyword(id) => {
            let s = resolve_sym(*id);
            if s.starts_with(':') {
                s.to_owned()
            } else {
                format!(":{s}")
            }
        }
        Value::Nil | Value::True => args[1].as_symbol_name().unwrap_or_default().to_string(),
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
    Ok(Value::Nil)
}

pub(crate) fn builtin_internal_subr_documentation(args: Vec<Value>) -> EvalResult {
    expect_args("internal-subr-documentation", &args, 1)?;
    Ok(Value::True)
}

pub(crate) fn builtin_malloc_info(args: Vec<Value>) -> EvalResult {
    expect_args("malloc-info", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_malloc_trim(args: Vec<Value>) -> EvalResult {
    expect_range_args("malloc-trim", &args, 0, 1)?;
    if let Some(pad) = args.first() {
        if !pad.is_nil() {
            let _ = expect_wholenump(pad)?;
        }
    }
    Ok(Value::True)
}

pub(crate) fn builtin_memory_info(args: Vec<Value>) -> EvalResult {
    expect_args("memory-info", &args, 0)?;
    let counts = Value::memory_use_counts_snapshot();
    Ok(Value::list(vec![
        Value::Int(counts[0]),
        Value::Int(counts[1]),
        Value::Int(counts[2]),
        Value::Int(counts[3]),
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
    Ok(Value::True)
}

pub(crate) fn builtin_dump_emacs_portable(args: Vec<Value>) -> EvalResult {
    expect_range_args("dump-emacs-portable", &args, 1, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_dump_emacs_portable_sort_predicate(args: Vec<Value>) -> EvalResult {
    expect_args("dump-emacs-portable--sort-predicate", &args, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_dump_emacs_portable_sort_predicate_copied(args: Vec<Value>) -> EvalResult {
    expect_args("dump-emacs-portable--sort-predicate-copied", &args, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_byte_code(args: Vec<Value>) -> EvalResult {
    expect_args("byte-code", &args, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_decode_coding_region(args: Vec<Value>) -> EvalResult {
    expect_range_args("decode-coding-region", &args, 3, 4)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_encode_coding_region(args: Vec<Value>) -> EvalResult {
    expect_range_args("encode-coding-region", &args, 3, 4)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_find_operation_coding_system(args: Vec<Value>) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("find-operation-coding-system"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    Ok(Value::Nil)
}

fn push_signal_temp_roots(eval: &mut super::eval::Evaluator, sig: &super::error::SignalData) {
    for value in &sig.data {
        eval.push_temp_root(*value);
    }
    if let Some(raw) = &sig.raw_data {
        eval.push_temp_root(*raw);
    }
}

fn resume_handler_bind_signal(
    eval: &mut super::eval::Evaluator,
    handlers: &[(Value, Value)],
    start: usize,
    sig: super::error::SignalData,
) -> EvalResult {
    let saved = eval.save_temp_roots();
    push_signal_temp_roots(eval, &sig);

    for (idx, (conditions, handler)) in handlers.iter().enumerate().skip(start) {
        if !crate::emacs_core::errors::signal_matches_condition_value(
            eval.obarray(),
            sig.symbol_name(),
            conditions,
        ) {
            continue;
        }

        let result = eval.apply(
            *handler,
            vec![super::error::make_signal_binding_value(&sig)],
        );
        eval.restore_temp_roots(saved);
        return match result {
            Ok(_) => resume_handler_bind_signal(eval, handlers, idx + 1, sig),
            Err(Flow::Signal(next_sig)) => {
                resume_handler_bind_signal(eval, handlers, idx + 1, next_sig)
            }
            Err(flow @ Flow::Throw { .. }) => Err(flow),
        };
    }

    eval.restore_temp_roots(saved);
    Err(Flow::Signal(sig))
}

pub(crate) fn builtin_handler_bind_1_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("handler-bind-1"),
                Value::Int(args.len() as i64),
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

    let saved = eval.save_temp_roots();
    for value in &args {
        eval.push_temp_root(*value);
    }

    let bodyfun = args[0];
    let handlers: Vec<(Value, Value)> = args[1..]
        .chunks_exact(2)
        .filter_map(|pair| (!pair[0].is_nil()).then_some((pair[0], pair[1])))
        .collect();

    let result = match eval.apply(bodyfun, vec![]) {
        Ok(value) => Ok(value),
        Err(Flow::Signal(sig)) => resume_handler_bind_signal(eval, &handlers, 0, sig),
        Err(flow @ Flow::Throw { .. }) => Err(flow),
    };

    eval.restore_temp_roots(saved);
    result
}

pub(crate) fn builtin_defconst_1(args: Vec<Value>) -> EvalResult {
    expect_range_args("defconst-1", &args, 2, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_defvar_1(args: Vec<Value>) -> EvalResult {
    expect_range_args("defvar-1", &args, 2, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_iso_charset(args: Vec<Value>) -> EvalResult {
    expect_args("iso-charset", &args, 3)?;
    Ok(Value::Nil)
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
    if let Value::Cons(_) = map {
        let car = map.cons_car();
        if car.is_symbol_named("keymap") {
            let cdr = map.cons_cdr();
            if let Value::Cons(_) = cdr {
                let cadr = cdr.cons_car();
                if let Value::Str(_) = cadr {
                    return Ok(cadr);
                }
            }
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_kill_emacs(args: Vec<Value>) -> EvalResult {
    expect_range_args("kill-emacs", &args, 0, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn plan_kill_emacs_request(
    args: &[Value],
) -> Result<super::eval::ShutdownRequest, Flow> {
    expect_range_args("kill-emacs", args, 0, 2)?;
    let exit_code = match args.first().copied().unwrap_or(Value::Nil) {
        Value::Int(n) => n as i32,
        Value::Nil | Value::True => 0,
        _ => 0,
    };
    let restart = args.get(1).is_some_and(Value::is_truthy);
    Ok(super::eval::ShutdownRequest { exit_code, restart })
}

pub(crate) fn builtin_kill_emacs_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let request = plan_kill_emacs_request(&args)?;
    let _ = eval.run_hook_if_bound("kill-emacs-hook");
    eval.request_shutdown(request.exit_code, request.restart);
    Ok(Value::Nil)
}

pub(crate) fn builtin_lower_frame(args: Vec<Value>) -> EvalResult {
    expect_range_args("lower-frame", &args, 0, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_lread_substitute_object_in_subtree(args: Vec<Value>) -> EvalResult {
    expect_args("lread--substitute-object-in-subtree", &args, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_make_byte_code(args: Vec<Value>) -> EvalResult {
    if args.len() < 4 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("make-byte-code"),
                Value::Int(args.len() as i64),
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
    let mut constants: Vec<Value> = match constants_vec {
        Value::Vector(id) => with_heap(|h| h.get_vector(*id).clone()),
        _ => Vec::new(),
    };

    // 3b. Reify compiled literals embedded in the constants vector.
    // GNU `.elc` constants may contain nested `#[...]` bytecode objects or
    // `#s(hash-table ...)` literals. At this point they are still represented
    // as ordinary Values produced by `quote_to_value`, so convert them into
    // real runtime objects before decoding/executing the bytecode.
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
    let max_stack = match maxdepth {
        Value::Int(n) => *n as u16,
        _ => 16, // fallback
    };

    // 6. Extract closure slot 4.
    // GNU byte-code objects use this slot for either a docstring or an
    // arbitrary documentation form, notably the oclosure type symbol.
    let (doc, doc_form) = match docstring.copied() {
        Some(v) if v.is_string() => (v.as_str().map(str::to_string), None),
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
        docstring: doc,
        doc_form,
        interactive: interactive.copied().filter(|v| !v.is_nil()),
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
    let docstring_value = docstring.copied().unwrap_or(Value::Nil);
    let _iform = interactive.copied().unwrap_or(Value::Nil);

    let params_expr = super::eval::value_to_expr(params_value);
    let params = parse_lambda_params_from_expr(&params_expr)?;

    let body_exprs: Vec<super::super::expr::Expr> = if body_value.is_nil() {
        vec![]
    } else {
        let body_items = list_to_vec(body_value).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), *body_value],
            )
        })?;
        body_items.iter().map(super::eval::value_to_expr).collect()
    };

    let env = if env_value.is_nil() {
        None
    } else {
        Some(*env_value)
    };

    let (docstring, doc_form) = match &docstring_value {
        Value::Str(id) => (Some(with_heap(|h| h.get_string(*id).to_owned())), None),
        Value::Nil => (None, None),
        other => (None, Some(*other)),
    };

    Ok(Value::make_lambda(LambdaData {
        params,
        body: body_exprs.into(),
        env,
        docstring,
        doc_form,
    }))
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

    let items = match val {
        Value::Vector(id) => {
            let v = with_heap(|h| h.get_vector(id).clone());
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
        && matches!(items[0], Value::Cons(_) | Value::Nil)
        && matches!(items[1], Value::Cons(_))
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
    let Value::HashTable(table_ref) = table_value else {
        return None;
    };

    with_heap_mut(|heap| {
        let table = heap.get_hash_table_mut(table_ref);
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
    Ok(Value::Nil)
}

pub(crate) fn builtin_make_closure(args: Vec<Value>) -> EvalResult {
    // (make-closure PROTOTYPE &rest CLOSURE-VARS)
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("make-closure"), Value::Int(args.len() as i64)],
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
            let key = match entry {
                Value::Cons(cell) => with_heap(|h| h.cons_car(*cell)),
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
    Ok(Value::Nil)
}

pub(crate) fn builtin_make_interpreted_closure(args: Vec<Value>) -> EvalResult {
    expect_range_args("make-interpreted-closure", &args, 3, 5)?;
    make_interpreted_closure_from_parts(&args[0], &args[1], &args[2], args.get(3), args.get(4))
}

fn parse_lambda_params_from_expr(expr: &super::super::expr::Expr) -> Result<LambdaParams, Flow> {
    use super::super::expr::Expr;
    match expr {
        Expr::Symbol(id) if resolve_sym(*id) == "nil" => Ok(LambdaParams::simple(vec![])),
        Expr::List(items) => {
            let mut required = Vec::new();
            let mut optional = Vec::new();
            let mut rest = None;
            let mut mode = 0;

            for item in items {
                let Expr::Symbol(id) = item else {
                    return Err(signal("wrong-type-argument", vec![]));
                };
                let name = resolve_sym(*id);
                match name {
                    "&optional" => {
                        mode = 1;
                        continue;
                    }
                    "&rest" => {
                        mode = 2;
                        continue;
                    }
                    _ => {}
                }
                match mode {
                    0 => required.push(*id),
                    1 => optional.push(*id),
                    2 => {
                        rest = Some(*id);
                        break;
                    }
                    _ => unreachable!(),
                }
            }

            Ok(LambdaParams {
                required,
                optional,
                rest,
            })
        }
        _ => Err(signal("wrong-type-argument", vec![])),
    }
}

pub(crate) fn builtin_treesit_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-available-p", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_compiled_query_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-compiled-query-p", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_induce_sparse_tree(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-induce-sparse-tree", &args, 2, 4)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_language_abi_version(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-language-abi-version", &args, 0, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_language_available_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-language-available-p", &args, 1, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_library_abi_version(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-library-abi-version", &args, 0, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_check(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-check", &args, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_child(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-child", &args, 2, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_child_by_field_name(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-child-by-field-name", &args, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_child_count(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-child-count", &args, 1, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_descendant_for_range(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-descendant-for-range", &args, 3, 4)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_end(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-end", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_eq(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-eq", &args, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_field_name_for_child(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-field-name-for-child", &args, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_first_child_for_pos(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-first-child-for-pos", &args, 2, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_match_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-match-p", &args, 2, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_next_sibling(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-next-sibling", &args, 1, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-p", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_parent(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-parent", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_parser(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-parser", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_prev_sibling(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-node-prev-sibling", &args, 1, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_start(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-start", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_string(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-string", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_node_type(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-node-type", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_add_notifier(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-add-notifier", &args, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_buffer(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-buffer", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_create(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-parser-create", &args, 1, 4)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_delete(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-delete", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_included_ranges(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-included-ranges", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_language(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-language", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_list(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-parser-list", &args, 0, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_notifiers(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-notifiers", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-p", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_remove_notifier(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-remove-notifier", &args, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_root_node(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-root-node", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_set_included_ranges(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-set-included-ranges", &args, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_parser_tag(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-tag", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_pattern_expand(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-pattern-expand", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_query_capture(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-query-capture", &args, 2, 5)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_query_compile(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-query-compile", &args, 2, 3)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_query_expand(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-expand", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_query_language(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-language", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_query_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-p", &args, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_search_forward(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-search-forward", &args, 2, 4)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_search_subtree(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-search-subtree", &args, 2, 5)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_treesit_subtree_stat(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-subtree-stat", &args, 1)?;
    Ok(Value::Nil)
}
