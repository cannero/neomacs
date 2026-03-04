use super::*;

// ===========================================================================
// Symbol operations (need evaluator for obarray access)
// ===========================================================================

const VARIABLE_ALIAS_PROPERTY: &str = "neovm--variable-alias";
const RAW_SYMBOL_PLIST_PROPERTY: &str = "neovm--raw-symbol-plist";

fn is_internal_symbol_plist_property(property: &str) -> bool {
    property == VARIABLE_ALIAS_PROPERTY || property == RAW_SYMBOL_PLIST_PROPERTY
}

pub(crate) fn resolve_variable_alias_name(
    eval: &super::eval::Evaluator,
    name: &str,
) -> Result<String, Flow> {
    let mut current = name.to_string();
    let mut seen = HashSet::new();

    loop {
        if !seen.insert(current.clone()) {
            return Err(signal(
                "cyclic-variable-indirection",
                vec![Value::symbol(name)],
            ));
        }
        let next = eval
            .obarray()
            .get_property(&current, VARIABLE_ALIAS_PROPERTY)
            .and_then(|value| value.as_symbol_name())
            .map(|value| value.to_string());
        match next {
            Some(next_name) => current = next_name,
            None => return Ok(current),
        }
    }
}

fn would_create_variable_alias_cycle(eval: &super::eval::Evaluator, new: &str, old: &str) -> bool {
    let mut current = old.to_string();
    let mut seen = HashSet::new();

    loop {
        if current == new {
            return true;
        }
        if !seen.insert(current.clone()) {
            return true;
        }
        let next = eval
            .obarray()
            .get_property(&current, VARIABLE_ALIAS_PROPERTY)
            .and_then(|value| value.as_symbol_name())
            .map(|value| value.to_string());
        match next {
            Some(next_name) => current = next_name,
            None => return false,
        }
    }
}

fn symbol_raw_plist_value(eval: &super::eval::Evaluator, name: &str) -> Option<Value> {
    eval.obarray()
        .get_property(name, RAW_SYMBOL_PLIST_PROPERTY)
        .cloned()
}

fn set_symbol_raw_plist(eval: &mut super::eval::Evaluator, name: &str, plist: Value) {
    let sym = eval.obarray_mut().get_or_intern(name);
    let alias = sym.plist.get(&intern(VARIABLE_ALIAS_PROPERTY)).cloned();
    sym.plist.clear();
    if let Some(value) = alias {
        sym.plist.insert(intern(VARIABLE_ALIAS_PROPERTY), value);
    }
    sym.plist.insert(intern(RAW_SYMBOL_PLIST_PROPERTY), plist);
}

fn plist_lookup_value(plist: &Value, prop: &Value) -> Option<Value> {
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
    expect_args("boundp", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    Ok(Value::bool(
        eval.obarray().boundp(&resolved) || eval.obarray().is_constant(&resolved),
    ))
}

pub(crate) fn builtin_obarrayp_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("obarrayp", &args, 1)?;
    let current_obarray = eval
        .obarray()
        .symbol_value("neovm--obarray-object")
        .or_else(|| eval.obarray().symbol_value("obarray"));
    Ok(Value::bool(
        current_obarray.is_some_and(|obarray| eq_value(obarray, &args[0])),
    ))
}

pub(crate) fn builtin_special_variable_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("special-variable-p", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    Ok(Value::bool(
        eval.obarray().is_special(&resolved) || eval.obarray().is_constant(&resolved),
    ))
}

pub(crate) fn builtin_default_boundp(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("default-boundp", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    Ok(Value::bool(
        eval.obarray().boundp(&resolved) || eval.obarray().is_constant(&resolved),
    ))
}

pub(crate) fn builtin_default_toplevel_value(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("default-toplevel-value", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    match eval.obarray().symbol_value(&resolved).cloned() {
        Some(value) => Ok(value),
        None if resolved.starts_with(':') => Ok(Value::symbol(resolved)),
        None => Err(signal("void-variable", vec![Value::symbol(name)])),
    }
}

pub(crate) fn builtin_set_default_toplevel_value(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-default-toplevel-value", &args, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    if eval.obarray().is_constant(&resolved) {
        return Err(signal("setting-constant", vec![Value::symbol(name)]));
    }
    let value = args[1];
    eval.obarray.set_symbol_value(&resolved, value);
    eval.run_variable_watchers(&resolved, &value, &Value::Nil, "set")?;
    if resolved != name {
        eval.run_variable_watchers(&resolved, &value, &Value::Nil, "set")?;
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_defvaralias_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("defvaralias", &args, 2, 3)?;
    let new_name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let old_name = args[1].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[1]],
        )
    })?;
    if eval.obarray().is_constant(new_name) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Cannot make a constant an alias: {new_name}"
            ))],
        ));
    }
    if would_create_variable_alias_cycle(eval, new_name, old_name) {
        return Err(signal(
            "cyclic-variable-indirection",
            vec![Value::symbol(old_name)],
        ));
    }
    let previous_target = resolve_variable_alias_name(eval, new_name)?;
    {
        let sym = eval.obarray_mut().get_or_intern(new_name);
        sym.special = true;
        sym.plist
            .insert(intern(VARIABLE_ALIAS_PROPERTY), Value::symbol(old_name));
    }
    eval.obarray_mut().make_special(old_name);
    preflight_symbol_plist_put(eval, &Value::symbol(new_name), "variable-documentation")?;
    eval.run_variable_watchers(
        &previous_target,
        &Value::symbol(old_name),
        &Value::Nil,
        "defvaralias",
    )?;
    eval.watchers.clear_watchers(new_name);
    // GNU Emacs updates `variable-documentation` through plist machinery after
    // installing alias state, so malformed raw plists still raise
    // `(wrong-type-argument plistp ...)` with the alias edge retained.
    let docstring = args.get(2).cloned().unwrap_or(Value::Nil);
    builtin_put(
        eval,
        vec![
            Value::symbol(new_name),
            Value::symbol("variable-documentation"),
            docstring,
        ],
    )?;
    Ok(Value::symbol(old_name))
}

pub(crate) fn builtin_indirect_variable_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("indirect-variable", &args, 1)?;
    let Some(name) = args[0].as_symbol_name() else {
        return Ok(args[0]);
    };
    let resolved = resolve_variable_alias_name(eval, name)?;
    Ok(Value::symbol(resolved))
}

pub(crate) fn builtin_fboundp(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("fboundp", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    if eval.obarray().is_function_unbound(name) {
        return Ok(Value::Nil);
    }
    if let Some(function) = eval.obarray().symbol_function(name) {
        let result = !function.is_nil();
        return Ok(Value::bool(result));
    }
    let macro_bound = super::subr_info::is_evaluator_macro_name(name);
    let result = super::subr_info::is_special_form(name)
        || macro_bound
        || super::subr_info::is_evaluator_callable_name(name)
        || super::builtin_registry::is_dispatch_builtin_name(name)
        || name.parse::<PureBuiltinId>().is_ok();
    Ok(Value::bool(result))
}

pub(crate) fn builtin_symbol_value(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("symbol-value", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    // Check dynamic bindings first
    let resolved_id = intern(&resolved);
    for frame in eval.dynamic.iter().rev() {
        if let Some(value) = frame.get(&resolved_id) {
            return Ok(*value);
        }
    }
    // Check current buffer-local binding.
    if let Some(buf) = eval.buffers.current_buffer() {
        if let Some(value) = buf.get_buffer_local(&resolved) {
            return Ok(*value);
        }
    }
    match eval.obarray().symbol_value(&resolved).cloned() {
        Some(value) => Ok(value),
        None if resolved.starts_with(':') => Ok(Value::symbol(resolved)),
        None => Err(signal("void-variable", vec![Value::symbol(name)])),
    }
}

fn startup_virtual_autoload_function_cell(
    _eval: &super::eval::Evaluator,
    _name: &str,
) -> Option<Value> {
    None
}

pub(crate) fn builtin_symbol_function(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("symbol-function", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    if eval.obarray().is_function_unbound(name) {
        return Ok(Value::Nil);
    }

    if let Some(function) = eval.obarray().symbol_function(name) {
        // GNU Emacs exposes this symbol as autoload-shaped in startup state,
        // then subr-shaped after first invocation triggers autoload materialization.
        if name == "kmacro-name-last-macro"
            && matches!(function, Value::Subr(subr) if resolve_sym(*subr) == "kmacro-name-last-macro")
            && eval
                .obarray()
                .get_property("kmacro-name-last-macro", "neovm--kmacro-autoload-promoted")
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

    if let Some(function) = startup_virtual_autoload_function_cell(eval, name) {
        return Ok(function);
    }

    if let Some(function) = super::subr_info::fallback_macro_value(name) {
        return Ok(function);
    }

    if name == "inline" {
        return Ok(Value::symbol("inline"));
    }

    if let Some(alias_target) = pure_builtin_symbol_alias_target(name) {
        return Ok(Value::symbol(alias_target));
    }

    if super::subr_info::is_special_form(name)
        || super::subr_info::is_evaluator_callable_name(name)
        || super::builtin_registry::is_dispatch_builtin_name(name)
        || name.parse::<PureBuiltinId>().is_ok()
    {
        return Ok(Value::Subr(intern(name)));
    }

    Ok(Value::Nil)
}

pub(crate) fn builtin_func_arity_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("func-arity", &args, 1)?;

    if let Some(name) = args[0].as_symbol_name() {
        if let Some(function) = resolve_indirect_symbol(eval, name) {
            if function.is_nil() {
                return Err(signal("void-function", vec![Value::symbol(name)]));
            }
            maybe_materialize_thingatpt_word_symbol(eval, name, &function);
            maybe_mark_pcase_fallback_materialized(eval, name, &function);
            if super::subr_info::is_special_form(name) {
                return super::subr_info::builtin_func_arity(vec![Value::Subr(intern(name))]);
            }
            if let Some(arity) = dispatch_symbol_func_arity_override(eval, name, &function) {
                return Ok(arity);
            }
            return super::subr_info::builtin_func_arity(vec![function]);
        }
        return Err(signal("void-function", vec![Value::symbol(name)]));
    }

    super::subr_info::builtin_func_arity(vec![args[0]])
}

fn maybe_materialize_thingatpt_word_symbol(
    eval: &mut super::eval::Evaluator,
    name: &str,
    function: &Value,
) {
    if !super::autoload::is_autoload_value(function) {
        return;
    }
    if !matches!(
        name,
        "symbol-at-point" | "thing-at-point" | "bounds-of-thing-at-point"
    ) {
        return;
    }
    let obarray = eval.obarray();
    if obarray.fboundp("word-at-point") {
        return;
    }
    // Respect explicit user-level `fmakunbound` after materialization. Startup
    // masking keeps the symbol uninterned and should still allow first bootstrap.
    if obarray.is_function_unbound("word-at-point")
        && obarray.intern_soft("word-at-point").is_some()
    {
        return;
    }
    eval.set_function("word-at-point", Value::Subr(intern("word-at-point")));
}

fn maybe_mark_pcase_fallback_materialized(
    _eval: &mut super::eval::Evaluator,
    _name: &str,
    _function: &Value,
) {
}

fn has_startup_subr_wrapper(eval: &super::eval::Evaluator, name: &str) -> bool {
    let wrapper = format!("neovm--startup-subr-wrapper-{name}");
    matches!(
        eval.obarray().symbol_function(&wrapper),
        Some(Value::Subr(subr_id)) if resolve_sym(*subr_id) == name
    )
}

fn dispatch_symbol_func_arity_override(
    eval: &super::eval::Evaluator,
    name: &str,
    function: &Value,
) -> Option<Value> {
    if !super::builtin_registry::is_dispatch_builtin_name(name) {
        return None;
    }

    if super::autoload::is_autoload_value(function)
        || (matches!(function, Value::ByteCode(_)) && has_startup_subr_wrapper(eval, name))
    {
        return Some(super::subr_info::dispatch_subr_arity_value(name));
    }

    None
}

pub(crate) fn builtin_set(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("set", &args, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    if eval.obarray().is_constant(&resolved) {
        return Err(signal("setting-constant", vec![Value::symbol(name)]));
    }
    let value = args[1];
    eval.assign_with_watchers(&resolved, value, "set")
}

pub(crate) fn builtin_fset(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("fset", &args, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    if name == "nil" {
        return Err(signal("setting-constant", vec![Value::symbol("nil")]));
    }
    let def = args[1];
    if would_create_function_alias_cycle(eval, name, &def) {
        return Err(signal(
            "cyclic-function-indirection",
            vec![Value::symbol(name)],
        ));
    }
    eval.obarray_mut().set_symbol_function(name, def);
    Ok(def)
}

pub(crate) fn would_create_function_alias_cycle(
    eval: &super::eval::Evaluator,
    target_name: &str,
    def: &Value,
) -> bool {
    let mut current = match def.as_symbol_name() {
        Some(name) => name.to_string(),
        None => return false,
    };
    let mut seen = HashSet::new();

    loop {
        if current == target_name {
            return true;
        }
        if !seen.insert(current.clone()) {
            return true;
        }

        let next = match eval.obarray().symbol_function(&current) {
            Some(function) => {
                if let Some(name) = function.as_symbol_name() {
                    name.to_string()
                } else {
                    return false;
                }
            }
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
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    if eval.obarray().is_constant(&resolved) {
        return Err(signal("setting-constant", vec![Value::symbol(name)]));
    }
    eval.obarray_mut().makunbound(&resolved);
    eval.run_variable_watchers(&resolved, &Value::Nil, &Value::Nil, "makunbound")?;
    Ok(args[0])
}

pub(crate) fn builtin_fmakunbound(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("fmakunbound", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    eval.obarray_mut().fmakunbound(name);
    Ok(args[0])
}

pub(crate) fn builtin_get(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("get", &args, 2)?;
    let sym = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    if let Some(raw) = symbol_raw_plist_value(eval, sym) {
        return Ok(plist_lookup_value(&raw, &args[1]).unwrap_or(Value::Nil));
    }
    let prop = args[1].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[1]],
        )
    })?;
    if is_internal_symbol_plist_property(prop) {
        return Ok(Value::Nil);
    }
    Ok(eval
        .obarray()
        .get_property(sym, prop)
        .cloned()
        .unwrap_or(Value::Nil))
}

pub(crate) fn builtin_put(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("put", &args, 3)?;
    let sym = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let prop = args[1].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[1]],
        )
    })?;
    let value = args[2];
    if let Some(raw) = symbol_raw_plist_value(eval, sym) {
        let plist = builtin_plist_put(vec![raw, args[1], value])?;
        set_symbol_raw_plist(eval, sym, plist);
        return Ok(value);
    }
    eval.obarray_mut().put_property(sym, prop, value);
    Ok(value)
}

pub(crate) fn builtin_symbol_plist_fn(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("symbol-plist", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    if let Some(raw) = symbol_raw_plist_value(eval, name) {
        return Ok(raw);
    }
    let Some(sym) = eval.obarray().get(name) else {
        return Ok(Value::Nil);
    };
    let mut items = Vec::new();
    for (key, value) in &sym.plist {
        if is_internal_symbol_plist_property(resolve_sym(*key)) {
            continue;
        }
        items.push(Value::symbol(resolve_sym(*key)));
        items.push(*value);
    }
    if items.is_empty() {
        Ok(Value::Nil)
    } else {
        Ok(Value::list(items))
    }
}

pub(super) fn builtin_register_code_conversion_map_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() == 2 {
        preflight_symbol_plist_put(eval, &args[0], "code-conversion-map")?;
    }
    let map_id = super::ccl::builtin_register_code_conversion_map(args.clone())?;

    let _ = builtin_put(
        eval,
        vec![args[0], Value::symbol("code-conversion-map"), args[1]],
    )?;
    let _ = builtin_put(
        eval,
        vec![args[0], Value::symbol("code-conversion-map-id"), map_id],
    )?;

    Ok(map_id)
}

fn symbol_has_valid_ccl_program_idx(
    eval: &mut super::eval::Evaluator,
    symbol: &Value,
) -> Result<bool, Flow> {
    if !symbol.is_symbol() {
        return Ok(false);
    }
    let idx = builtin_get(eval, vec![*symbol, Value::symbol("ccl-program-idx")])?;
    Ok(idx.as_int().is_some_and(|n| n >= 0))
}

pub(super) fn builtin_ccl_program_p_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() == 1 && args[0].is_symbol() {
        return Ok(Value::bool(symbol_has_valid_ccl_program_idx(
            eval, &args[0],
        )?));
    }
    super::ccl::builtin_ccl_program_p(args)
}

pub(super) fn builtin_ccl_execute_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.first().is_some_and(Value::is_symbol)
        && !symbol_has_valid_ccl_program_idx(eval, &args[0])?
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
    if args.first().is_some_and(Value::is_symbol)
        && !symbol_has_valid_ccl_program_idx(eval, &args[0])?
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
    let was_registered = args
        .first()
        .and_then(Value::as_symbol_name)
        .is_some_and(super::ccl::is_registered_ccl_program);
    let program_id = super::ccl::builtin_register_ccl_program(args.clone())?;

    if was_registered {
        return Ok(program_id);
    }

    let publish = builtin_put(
        eval,
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
    let Some(name) = symbol.as_symbol_name() else {
        return Ok(());
    };
    let Some(raw) = symbol_raw_plist_value(eval, name) else {
        return Ok(());
    };
    let _ = builtin_plist_put(vec![raw, Value::symbol(property), Value::Nil])?;
    Ok(())
}

pub(crate) fn builtin_setplist_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("setplist", &args, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let plist = args[1];
    set_symbol_raw_plist(eval, name, plist);
    Ok(plist)
}

fn macroexpand_environment_binding(env: &Value, name: &str) -> Option<Value> {
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
                if entry_pair.car.as_symbol_name() == Some(name) {
                    return Some(entry_pair.cdr);
                }
            }
            _ => return None,
        }
    }
}

fn macroexpand_environment_callable(
    eval: &mut super::eval::Evaluator,
    binding: &Value,
) -> Result<Value, Flow> {
    if is_lambda_form_list(binding) {
        return eval.eval_value(binding);
    }
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
    eval: &mut super::eval::Evaluator,
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

            let length_sym = eval.next_pcase_macroexpand_temp_symbol();
            let mut elem_bindings = Vec::with_capacity(vars.len());
            let mut var_bindings = Vec::with_capacity(vars.len());
            for (idx, var) in vars.iter().enumerate() {
                let temp = eval.next_pcase_macroexpand_temp_symbol();
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
        let head = eval.next_pcase_macroexpand_temp_symbol();
        let tail = eval.next_pcase_macroexpand_temp_symbol();
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
    eval: &mut super::eval::Evaluator,
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
                return macroexpand_known_fallback_macro(eval, "pcase-let*", &star_args);
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

                if let Some(pattern) = unknown_pattern {
                    return Err(signal(
                        "error",
                        vec![Value::string(format!("Unknown x pattern: {pattern}"))],
                    ));
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
                        eval,
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

#[tracing::instrument(level = "trace", skip(eval, environment), fields(head))]
fn macroexpand_once_with_environment(
    eval: &mut super::eval::Evaluator,
    form: Value,
    environment: Option<&Value>,
) -> Result<(Value, bool), Flow> {
    let Value::Cons(form_cell) = form else {
        return Ok((form, false));
    };
    let form_pair = read_cons(form_cell);
    let head = form_pair.car;
    let tail = form_pair.cdr;
    let Some(head_name) = head.as_symbol_name() else {
        return Ok((form, false));
    };

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
        if let Some(binding) = macroexpand_environment_binding(env, head_name) {
            env_bound = true;
            if !binding.is_nil() {
                function = Some(macroexpand_environment_callable(eval, &binding)?);
            }
        }
    }
    if env_bound && function.is_none() {
        return Ok((form, false));
    }
    let mut resolved_name = head_name.to_string();
    let mut fallback_placeholder = false;
    if function.is_none() {
        if let Some((resolved, global)) = resolve_indirect_symbol_with_name(eval, head_name) {
            // Check for Value::Macro (native macros) AND cons-cell macros
            // `(macro . fn)` — matches real Emacs eval.c which checks
            // `EQ (XCAR (def), Qmacro)`.
            let is_macro = matches!(global, Value::Macro(_))
                || (global.is_cons() && global.cons_car().is_symbol_named("macro"));
            if is_macro {
                fallback_placeholder = super::subr_info::has_fallback_macro(&resolved)
                    && eval.obarray().symbol_function(&resolved).is_none();
                resolved_name = resolved;
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
                let _ = super::autoload::builtin_autoload_do_load(
                    eval,
                    vec![global, Value::symbol(head_name), Value::symbol("macro")],
                );
                // Re-check the function cell after loading
                if let Some((resolved2, global2)) =
                    resolve_indirect_symbol_with_name(eval, head_name)
                {
                    let is_macro2 = matches!(global2, Value::Macro(_))
                        || (global2.is_cons() && global2.cons_car().is_symbol_named("macro"));
                    if is_macro2 {
                        fallback_placeholder = super::subr_info::has_fallback_macro(&resolved2)
                            && eval.obarray().symbol_function(&resolved2).is_none();
                        resolved_name = resolved2;
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
        if let Some(expanded) = macroexpand_known_fallback_macro(eval, &resolved_name, &args)? {
            return Ok((expanded, true));
        }
        return Ok((form, false));
    }
    // Root function and args across eval.apply() — the macro
    // expander may trigger GC.
    let saved_roots = eval.save_temp_roots();
    eval.push_temp_root(form);
    eval.push_temp_root(function);
    for arg in &args {
        eval.push_temp_root(*arg);
    }
    let expanded = eval.apply(function, args)?;
    eval.restore_temp_roots(saved_roots);
    // Match real Emacs (eval.c line 1319): if the macro expander returned
    // the same form object (EQ), treat it as "no expansion occurred".
    let did_expand = !eq_value(&form, &expanded);
    Ok((expanded, did_expand))
}

pub(crate) fn builtin_macroexpand_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("macroexpand", &args, 1, 2)?;
    let mut form = args[0];
    let environment = args.get(1);
    loop {
        let (expanded, did_expand) = macroexpand_once_with_environment(eval, form, environment)?;
        if !did_expand {
            return Ok(expanded);
        }
        form = expanded;
    }
}

pub(crate) fn builtin_indirect_function(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("indirect-function", &args, 1)?;
    expect_max_args("indirect-function", &args, 2)?;
    let _noerror = args.get(1).is_some_and(|value| value.is_truthy());

    if let Some(name) = args[0].as_symbol_name() {
        if let Some(function) = startup_virtual_autoload_function_cell(eval, name) {
            return Ok(function);
        }
        if let Some(function) = resolve_indirect_symbol(eval, name) {
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

fn resolve_indirect_symbol_with_name(
    eval: &super::eval::Evaluator,
    name: &str,
) -> Option<(String, Value)> {
    let mut current = name.to_string();
    let mut seen = HashSet::new();

    loop {
        if !seen.insert(current.clone()) {
            return None;
        }

        if eval.obarray().is_function_unbound(&current) {
            return None;
        }

        if let Some(function) = eval.obarray().symbol_function(&current) {
            if let Some(next) = function.as_symbol_name() {
                if next == "nil" {
                    return Some(("nil".to_string(), Value::Nil));
                }
                current = next.to_string();
                continue;
            }
            return Some((current, *function));
        }

        if let Some(function) = super::subr_info::fallback_macro_value(&current) {
            return Some((current, function));
        }

        if let Some(alias_target) = pure_builtin_symbol_alias_target(&current) {
            current = alias_target.to_string();
            continue;
        }

        if super::subr_info::is_special_form(&current)
            || super::subr_info::is_evaluator_callable_name(&current)
            || super::builtin_registry::is_dispatch_builtin_name(&current)
            || current.parse::<PureBuiltinId>().is_ok()
        {
            return Some((current.clone(), Value::Subr(intern(&current))));
        }

        return None;
    }
}

pub(super) fn resolve_indirect_symbol(eval: &super::eval::Evaluator, name: &str) -> Option<Value> {
    resolve_indirect_symbol_with_name(eval, name).map(|(_, value)| value)
}

pub(crate) fn builtin_macrop_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("macrop", &args, 1)?;
    if let Some(name) = args[0].as_symbol_name() {
        if let Some(function) = startup_virtual_autoload_function_cell(eval, name) {
            return super::subr_info::builtin_macrop(vec![function]);
        }
        if let Some(function) = resolve_indirect_symbol(eval, name) {
            return super::subr_info::builtin_macrop(vec![function]);
        }
        return Ok(Value::Nil);
    }

    super::subr_info::builtin_macrop(args)
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
    eval.obarray_mut().intern(&name);
    Ok(Value::symbol(name))
}

pub(crate) fn builtin_intern_soft(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
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
    let name = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).clone()),
        Value::Nil => return Ok(Value::Nil),
        Value::True => return Ok(Value::True),
        Value::Keyword(_) => return Ok(args[0]),
        Value::Symbol(id) => {
            if eval.obarray().intern_soft(resolve_sym(*id)).is_some() {
                return Ok(args[0]);
            }
            return Ok(Value::Nil);
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    if eval.obarray().intern_soft(&name).is_some() {
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

pub(super) fn expect_obarray_vector_id(value: &Value) -> Result<ObjId, Flow> {
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
    expect_range_args("next-frame", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !matches!(frame, Value::Frame(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    super::window_cmds::builtin_selected_frame(eval, Vec::new())
}

pub(crate) fn builtin_previous_frame(args: Vec<Value>) -> EvalResult {
    expect_range_args("previous-frame", &args, 0, 2)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_previous_frame_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("previous-frame", &args, 0, 2)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() && !matches!(frame, Value::Frame(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    super::window_cmds::builtin_selected_frame(eval, Vec::new())
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

pub(crate) fn builtin_suspend_emacs(args: Vec<Value>) -> EvalResult {
    expect_range_args("suspend-emacs", &args, 0, 1)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_vertical_motion(args: Vec<Value>) -> EvalResult {
    expect_range_args("vertical-motion", &args, 1, 3)?;
    let lines = expect_fixnum(&args[0])?;
    if args.len() == 1 {
        return Ok(Value::Int(lines));
    }
    let window = &args[1];
    if !window.is_nil() && !matches!(window, Value::Window(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-live-p"), *window],
        ));
    }
    Ok(Value::Int(0))
}

pub(crate) fn builtin_rename_buffer(args: Vec<Value>) -> EvalResult {
    expect_range_args("rename-buffer", &args, 1, 2)?;
    let name = expect_strict_string(&args[0])?;
    Ok(Value::string(name))
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

pub(crate) fn builtin_map_keymap(args: Vec<Value>) -> EvalResult {
    expect_range_args("map-keymap", &args, 2, 3)?;
    if !is_lisp_keymap_object(&args[1]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("keymapp"), args[1]],
        ));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_map_keymap_internal(args: Vec<Value>) -> EvalResult {
    expect_args("map-keymap-internal", &args, 2)?;
    if !is_lisp_keymap_object(&args[1]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("keymapp"), args[1]],
        ));
    }
    Ok(Value::Nil)
}

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

/// `(match-data--translate N)` — translate match data by N positions.
///
/// Shifts all byte positions in the current match data by N (which can
/// be negative).  This is used by `replace-regexp-in-string` in subr.el
/// after `string-match` to adjust positions for a substring.
pub(crate) fn builtin_match_data_translate_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("match-data--translate", &args, 1)?;
    let n = expect_fixnum(&args[0])?;

    if let Some(ref mut md) = eval.match_data {
        for group in md.groups.iter_mut() {
            if let Some((start, end)) = group {
                *start = (*start as i64 + n).max(0) as usize;
                *end = (*end as i64 + n).max(0) as usize;
            }
        }
    }
    Ok(Value::Nil)
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
    expect_args("old-selected-frame", &args, 0)?;
    super::window_cmds::builtin_selected_frame(eval, Vec::new())
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
    expect_args("mouse-pixel-position", &args, 0)?;
    let frame = super::window_cmds::builtin_selected_frame(eval, Vec::new())?;
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
    expect_args("mouse-position", &args, 0)?;
    let frame = super::window_cmds::builtin_selected_frame(eval, Vec::new())?;
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

pub(crate) fn builtin_new_fontset(args: Vec<Value>) -> EvalResult {
    expect_args("new-fontset", &args, 2)?;
    let _ = expect_strict_string(&args[0])?;
    Err(signal(
        "error",
        vec![Value::string("Fontset name must be in XLFD format")],
    ))
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
    Ok(Value::Nil)
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
    Ok(Value::Nil)
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
    Ok(args.get(1).cloned().unwrap_or(Value::Nil))
}

pub(crate) fn builtin_set_window_new_pixel(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-window-new-pixel", &args, 2, 3)?;
    expect_window_valid_or_nil(&args[0])?;
    let _ = expect_int(&args[1])?;
    Ok(args[1])
}

pub(crate) fn builtin_set_window_new_total(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-window-new-total", &args, 2, 3)?;
    expect_window_valid_or_nil(&args[0])?;
    let _ = expect_fixnum(&args[1])?;
    Ok(args[1])
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

pub(crate) fn builtin_subst_char_in_region(args: Vec<Value>) -> EvalResult {
    expect_range_args("subst-char-in-region", &args, 4, 5)?;
    Ok(Value::Nil)
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

fn compare_value_lt(lhs: &Value, rhs: &Value) -> Result<std::cmp::Ordering, (Value, Value)> {
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
    expect_args("variable-binding-locus", &args, 1)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    if resolved == "nil" || resolved == "t" || resolved.starts_with(':') {
        return Ok(Value::Nil);
    }
    if let Some(buf) = eval.buffers.current_buffer() {
        if buf.get_buffer_local(&resolved).is_some() {
            return Ok(Value::Buffer(buf.id));
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_x_begin_drag(args: Vec<Value>) -> EvalResult {
    expect_range_args("x-begin-drag", &args, 1, 6)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_x_create_frame(args: Vec<Value>) -> EvalResult {
    expect_args("x-create-frame", &args, 1)?;
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
    let mut body_iter = body.iter();
    let first = match body_iter.next() {
        Some(super::expr::Expr::Str(_)) => body_iter.next(),
        other => other,
    };

    let super::expr::Expr::List(items) = first? else {
        return None;
    };
    let super::expr::Expr::Symbol(head_id) = items.first()? else {
        return None;
    };
    if resolve_sym(*head_id) != "interactive" {
        return None;
    }
    let mut interactive = vec![Value::symbol("interactive")];
    if let Some(spec) = items.get(1).map(super::eval::quote_to_value) {
        interactive.push(spec);
    }
    Some(Value::list(interactive))
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
        Value::Nil => Ok(Some(Value::list(vec![Value::symbol("interactive")]))),
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

pub(crate) fn builtin_interactive_form_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("interactive-form", &args, 1)?;
    if args[0].as_symbol_name() == Some("ignore") {
        return Ok(Value::list(vec![Value::symbol("interactive"), Value::Nil]));
    }

    let function = match &args[0] {
        Value::Symbol(id) => {
            let Some((resolved_name, function)) =
                resolve_indirect_symbol_with_name(eval, resolve_sym(*id))
            else {
                return Ok(Value::Nil);
            };
            if resolved_name == "ignore" {
                return Ok(Value::list(vec![Value::symbol("interactive"), Value::Nil]));
            }
            function
        }
        other => *other,
    };

    let interactive = match &function {
        Value::Lambda(id) | Value::Macro(id) => {
            let body = with_heap(|h| h.get_lambda(*id).body.clone());
            Ok(interactive_form_from_expr_body(&body))
        }
        Value::Cons(_) => interactive_form_from_quoted_lambda(&function),
        _ => Ok(None),
    }?;
    Ok(interactive.unwrap_or(Value::Nil))
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
    expect_range_args("local-variable-if-set-p", &args, 1, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    if resolved == "nil" || resolved == "t" || resolved.starts_with(':') {
        return Ok(Value::Nil);
    }
    Ok(Value::bool(eval.custom.is_auto_buffer_local(&resolved)))
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
    let _ = expect_character_code(&args[0])?;
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
    let (mut modifiers, base) = parse_event_symbol_prefixes(name);
    modifiers.reverse();

    let mut out = vec![Value::symbol(base)];
    out.extend(modifiers);
    Ok(Value::list(out))
}

pub(crate) fn builtin_internal_handle_focus_in(args: Vec<Value>) -> EvalResult {
    expect_args("internal-handle-focus-in", &args, 1)?;
    Err(signal(
        "error",
        vec![Value::string("invalid focus-in event")],
    ))
}

pub(crate) fn builtin_internal_make_var_non_special(args: Vec<Value>) -> EvalResult {
    expect_args("internal-make-var-non-special", &args, 1)?;
    if args[0].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    Ok(Value::Nil)
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

pub(crate) fn builtin_handler_bind_1(args: Vec<Value>) -> EvalResult {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("handler-bind-1"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    Ok(Value::Nil)
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
    Ok(Value::Nil)
}

pub(crate) fn builtin_kill_emacs(args: Vec<Value>) -> EvalResult {
    expect_range_args("kill-emacs", &args, 0, 2)?;
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
        decode_gnu_bytecode, parse_arglist_value, string_value_to_bytes,
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

    // 3b. Post-process constants: recursively convert nested bytecode vectors.
    // In .elc files, inner lambdas appear as vectors in the constants.
    // The parser produces (byte-code-literal VECTOR) for #[...], which gets
    // evaluated by sf_byte_code_literal. But when constants come directly as
    // Value::Vector entries, we need to check if they look like bytecode objects
    // and convert them.
    for i in 0..constants.len() {
        constants[i] = try_convert_nested_bytecode(constants[i]);
    }

    // 4. Decode GNU bytecodes
    let ops = decode_gnu_bytecode(&raw_bytes, &mut constants).map_err(|e| {
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

    // 6. Extract docstring
    let doc = match docstring {
        Some(v) if v.is_string() => v.as_str().map(str::to_string),
        _ => None,
    };

    // 7. Build ByteCodeFunction
    let bc = ByteCodeFunction {
        ops,
        constants,
        max_stack,
        params,
        env: None,
        docstring: doc,
        doc_form: None,
    };

    let _ = interactive; // Not used yet

    Ok(Value::make_bytecode(bc))
}

/// Try to convert a Value::Vector that looks like a bytecode object into Value::ByteCode.
/// A bytecode vector has >= 4 elements where element 1 is a string (bytecodes)
/// and element 2 is a vector (constants).
pub(crate) fn try_convert_nested_bytecode(val: Value) -> Value {
    let items = match val {
        Value::Vector(id) => {
            let v = with_heap(|h| h.get_vector(id).clone());
            if v.len() >= 4 {
                v
            } else {
                return val;
            }
        }
        _ => return val,
    };

    // Check if this looks like a bytecode vector:
    // [0] = arglist (int or list), [1] = bytecode string, [2] = constants vector, [3] = maxdepth
    if !items[1].is_string() {
        return val;
    }
    // items[2] should be a vector
    if !items[2].is_vector() && !items[2].is_nil() {
        return val;
    }

    match make_byte_code_from_parts(
        &items[0],
        &items[1],
        &items[2],
        &items[3],
        items.get(4),
        items.get(5),
    ) {
        Ok(bc) => bc,
        Err(_) => val, // If decoding fails, keep the original vector
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

pub(crate) fn builtin_make_indirect_buffer(args: Vec<Value>) -> EvalResult {
    expect_range_args("make-indirect-buffer", &args, 2, 4)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_make_interpreted_closure(args: Vec<Value>) -> EvalResult {
    expect_range_args("make-interpreted-closure", &args, 3, 5)?;

    // Arguments: (ARGS BODY ENV &optional DOCSTRING IFORM)
    let params_value = &args[0];
    let body_value = &args[1];
    let env_value = &args[2];
    let docstring_value = args.get(3).copied().unwrap_or(Value::Nil);
    let _iform = args.get(4).copied().unwrap_or(Value::Nil);

    // Parse parameter list from Value
    let params_expr = super::eval::value_to_expr(params_value);
    let params = parse_lambda_params_from_expr(&params_expr)?;

    // Parse body from Value (must be a list)
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

    // Parse env from Value — store directly as a cons alist Value.
    let env = if env_value.is_nil() {
        None // Dynamic scope
    } else if matches!(env_value, Value::True) {
        // t = empty lexical env marker
        Some(Value::Nil)
    } else {
        // Already a cons alist — store directly
        Some(*env_value)
    };

    // Parse docstring — can be a string, a symbol (oclosure type), or nil
    let (docstring, doc_form) = match &docstring_value {
        Value::Str(id) => (Some(with_heap(|h| h.get_string(*id).clone())), None),
        Value::Nil => (None, None),
        // Non-string, non-nil: store as doc_form (e.g., symbol for oclosure type)
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
