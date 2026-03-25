use super::*;
use crate::emacs_core::symbol::Obarray;

// ===========================================================================
// Keymap builtins
// ===========================================================================
use super::keymap::{
    KeyEvent, expand_meta_prefix_char_events_in_obarray, is_list_keymap, key_event_to_emacs_event,
    list_keymap_accessible, list_keymap_copy, list_keymap_define_seq_in_obarray,
    list_keymap_lookup_one, list_keymap_parent, list_keymap_set_parent, make_list_keymap,
    make_sparse_list_keymap,
};

/// Validate that a value is a keymap, returning it if so.
/// Accepts:
/// - Cons cells starting with 'keymap
/// - Symbols whose function definition is a keymap
pub(crate) fn expect_keymap_in_obarray(obarray: &Obarray, value: &Value) -> Result<Value, Flow> {
    if is_list_keymap(value) {
        return Ok(*value);
    }
    // Check if it's a symbol whose function cell is a keymap
    if let Some(sym_name) = value.as_symbol_name() {
        if let Some(func) = obarray.symbol_function(sym_name).copied() {
            if is_list_keymap(&func) {
                return Ok(func);
            }
        }
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("keymapp"), *value],
    ))
}

fn expect_keymap(eval: &super::eval::Context, value: &Value) -> Result<Value, Flow> {
    expect_keymap_in_obarray(eval.obarray(), value)
}

/// Get the global keymap from obarray, creating one if needed.
pub(crate) fn ensure_global_keymap_in_obarray(obarray: &mut Obarray) -> Value {
    if let Some(val) = obarray.symbol_value("global-map").copied() {
        if is_list_keymap(&val) {
            return val;
        }
    }
    let km = make_list_keymap();
    obarray.set_symbol_value("global-map", km);
    km
}

/// Get the global keymap from obarray, creating one if needed.
fn ensure_global_keymap(eval: &mut super::eval::Context) -> Value {
    ensure_global_keymap_in_obarray(&mut eval.obarray)
}

/// Parse a key description from a Value, returning emacs event values.
///
/// For vectors, integer and symbol elements are used directly as emacs event
/// codes (preserving all modifier bits including Alt and Hyper).  For strings,
/// each character is treated as a raw key event.
pub(crate) fn expect_key_events(value: &Value) -> Result<Vec<Value>, Flow> {
    use super::value::with_heap;

    match value {
        // Vectors: use elements directly — integers are already emacs event codes,
        // symbols are already event symbols.
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(*v).clone());
            let mut events = Vec::with_capacity(items.len());
            for item in &items {
                match item {
                    // Integer event codes (character + modifier bits)
                    Value::Int(_) => events.push(*item),
                    // Char values: convert to Int for keymap consistency
                    Value::Char(c) => events.push(Value::Int(*c as i64)),
                    // Symbol events (function keys, remap, etc.)
                    Value::Symbol(_) => events.push(*item),
                    // nil and t can appear as events in vectors
                    Value::Nil => events.push(Value::symbol("nil")),
                    Value::True => events.push(Value::symbol("t")),
                    // Event modifier list: (control meta ?a) etc.
                    Value::Cons(_) => {
                        match super::kbd::key_events_from_designator(&Value::vector(vec![*item])) {
                            Ok(ke) => {
                                for e in &ke {
                                    events.push(key_event_to_emacs_event(e));
                                }
                            }
                            Err(super::kbd::KeyDesignatorError::Parse(msg)) => {
                                return Err(signal("error", vec![Value::string(msg)]));
                            }
                            Err(super::kbd::KeyDesignatorError::WrongType(other)) => {
                                return Err(signal(
                                    "wrong-type-argument",
                                    vec![Value::symbol("arrayp"), other],
                                ));
                            }
                        }
                    }
                    other => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("arrayp"), *other],
                        ));
                    }
                }
            }
            Ok(events)
        }
        // Strings and other forms: go through KeyEvent roundtrip
        _ => {
            let key_events = expect_key_description(value)?;
            Ok(key_events.iter().map(key_event_to_emacs_event).collect())
        }
    }
}

/// Parse a key description from a Value (must be a string or vector).
fn expect_key_description(value: &Value) -> Result<Vec<KeyEvent>, Flow> {
    match super::kbd::key_events_from_designator(value) {
        Ok(events) => Ok(events),
        Err(super::kbd::KeyDesignatorError::WrongType(other)) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("arrayp"), other],
        )),
        Err(super::kbd::KeyDesignatorError::Parse(msg)) => {
            Err(signal("error", vec![Value::string(msg)]))
        }
    }
}

/// `(accessible-keymaps KEYMAP &optional PREFIXES)` -> list of accessible keymaps.
pub(super) fn builtin_accessible_keymaps(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_accessible_keymaps_in_obarray(eval.obarray(), &args)
}

pub(crate) fn builtin_accessible_keymaps_in_obarray(
    obarray: &Obarray,
    args: &[Value],
) -> EvalResult {
    use super::value::with_heap;

    expect_min_args("accessible-keymaps", &args, 1)?;
    expect_max_args("accessible-keymaps", &args, 2)?;
    let keymap = expect_keymap_in_obarray(obarray, &args[0])?;

    // Collect all accessible keymaps
    let mut all_out = Vec::new();
    let mut prefix = Vec::new();
    let mut seen = Vec::new();
    list_keymap_accessible(&keymap, &mut prefix, &mut all_out, &mut seen);

    // If prefix argument is provided, filter results
    if let Some(prefix_arg) = args.get(1) {
        if !prefix_arg.is_nil() {
            // Must be a sequence (string or vector), not a list or non-sequence
            let prefix_events: Vec<Value> = match prefix_arg {
                Value::Str(_) => {
                    // String prefix — convert to events
                    expect_key_events(prefix_arg)?
                }
                Value::Vector(id) => {
                    // Vector prefix — elements are events directly
                    with_heap(|h| h.get_vector(*id).clone())
                }
                Value::Cons(_) => {
                    // Lists are not valid as key sequences for prefix
                    return Err(super::error::signal(
                        "wrong-type-argument",
                        vec![Value::symbol("arrayp"), *prefix_arg],
                    ));
                }
                _ => {
                    return Err(super::error::signal(
                        "wrong-type-argument",
                        vec![Value::symbol("sequencep"), *prefix_arg],
                    ));
                }
            };

            // Filter: only keep entries whose prefix starts with the given prefix
            let filtered: Vec<Value> = all_out
                .into_iter()
                .filter(|entry| {
                    if let Value::Cons(cell) = entry {
                        let pair = read_cons(*cell);
                        // pair.car is the prefix vector
                        if let Value::Vector(vid) = pair.car {
                            let entry_prefix = with_heap(|h| h.get_vector(vid).clone());
                            if entry_prefix.len() >= prefix_events.len() {
                                return entry_prefix[..prefix_events.len()] == prefix_events[..];
                            }
                        }
                    }
                    false
                })
                .collect();

            if filtered.is_empty() {
                return Ok(Value::Nil);
            }
            return Ok(Value::list(filtered));
        }
    }

    Ok(Value::list(all_out))
}

/// (make-keymap) -> keymap
pub(super) fn builtin_make_keymap(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_make_keymap_pure(&args)
}

pub(crate) fn builtin_make_keymap_pure(args: &[Value]) -> EvalResult {
    expect_max_args("make-keymap", &args, 1)?;
    Ok(make_list_keymap())
}

/// (make-sparse-keymap &optional NAME) -> keymap
pub(super) fn builtin_make_sparse_keymap(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("make-sparse-keymap", &args, 1)?;
    // GNU keymap.c: (make-sparse-keymap "prompt") → (keymap "prompt")
    if let Some(prompt) = args.first() {
        if prompt.is_string() {
            return Ok(Value::cons(
                Value::symbol("keymap"),
                Value::cons(*prompt, Value::Nil),
            ));
        }
    }
    Ok(make_sparse_list_keymap())
}

/// `(copy-keymap KEYMAP)` -> keymap copy.
pub(super) fn builtin_copy_keymap(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_copy_keymap_in_obarray(eval.obarray(), &args)
}

pub(crate) fn builtin_copy_keymap_in_obarray(obarray: &Obarray, args: &[Value]) -> EvalResult {
    expect_args("copy-keymap", &args, 1)?;
    let keymap = expect_keymap_in_obarray(obarray, &args[0])?;
    Ok(list_keymap_copy(&keymap))
}

/// (define-key KEYMAP KEY DEF &optional REMOVE) -> DEF
pub(super) fn builtin_define_key(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("define-key", &args, 3)?;
    expect_max_args("define-key", &args, 4)?;
    let keymap = expect_keymap(eval, &args[0])?;
    let mut events = expect_key_events(&args[1])?;
    let def = args[2];
    // Expand meta-prefixed events to ESC + base, matching GNU Emacs
    // Fdefine_key's metized handling.
    if let Some(expanded) = expand_meta_prefix_char_events_in_obarray(eval.obarray(), &events) {
        events = expanded;
    }
    if let Err(msg) = list_keymap_define_seq_in_obarray(eval.obarray(), keymap, &events, def) {
        return Err(signal("error", vec![Value::string(msg)]));
    }
    Ok(def)
}

/// (lookup-key KEYMAP KEY) -> binding or nil
pub(super) fn builtin_lookup_key(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_lookup_key_in_obarray(eval.obarray(), &args)
}

pub(crate) fn builtin_lookup_key_in_obarray(obarray: &Obarray, args: &[Value]) -> EvalResult {
    expect_min_args("lookup-key", &args, 2)?;
    expect_max_args("lookup-key", &args, 3)?;
    // Optional 3rd arg ACCEPT-DEFAULTS is accepted but ignored.
    let keymap = expect_keymap_in_obarray(obarray, &args[0])?;
    let events = expect_key_events(&args[1])?;

    if events.is_empty() {
        return Ok(keymap);
    }

    let direct = lookup_key_in_obarray(obarray, &keymap, &events);
    if direct.is_nil() || matches!(direct, Value::Int(_)) {
        if let Some(expanded) = expand_meta_prefix_char_events_in_obarray(obarray, &events) {
            let expanded_result = lookup_key_in_obarray(obarray, &keymap, &expanded);
            // Only use the expanded result if it found an actual binding
            // (not nil and not a "too long" integer).
            if !expanded_result.is_nil() && !matches!(expanded_result, Value::Int(_)) {
                return Ok(expanded_result);
            }
        }
    }

    Ok(direct)
}

fn lookup_key_in_obarray(obarray: &Obarray, keymap: &Value, events: &[Value]) -> Value {
    if events.is_empty() {
        return *keymap;
    }

    let mut current_map = *keymap;
    for (i, event) in events.iter().enumerate() {
        let binding = list_keymap_lookup_one(&current_map, event);
        let is_last = i == events.len() - 1;

        // For the last key in the sequence, return the binding directly
        // (even if nil), matching GNU Emacs lookup_key_1 behavior.
        if is_last {
            return binding;
        }

        // For non-last keys, the binding must be a prefix keymap.
        // If nil/unbound, the key sequence is invalid — return
        // the number of keys consumed so far (matching GNU which
        // returns make_fixnum(idx) where idx is already incremented).
        if binding.is_nil() {
            return Value::Int((i + 1) as i64);
        }

        // Try to resolve to a keymap for the next level of lookup.
        if is_list_keymap(&binding) {
            current_map = binding;
            continue;
        }
        if let Some(sym_name) = binding.as_symbol_name() {
            if let Some(func) = obarray.symbol_function(sym_name).copied() {
                if is_list_keymap(&func) {
                    current_map = func;
                    continue;
                }
            }
        }
        // Non-prefix binding found before all keys consumed: "too long"
        return Value::Int((i + 1) as i64);
    }

    Value::Nil
}

/// (global-set-key KEY COMMAND)
pub(super) fn builtin_global_set_key(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("global-set-key", &args, 2)?;
    let global = ensure_global_keymap(eval);
    let events = expect_key_events(&args[0])?;
    let def = args[1];
    if let Err(msg) = list_keymap_define_seq_in_obarray(eval.obarray(), global, &events, def) {
        return Err(signal("error", vec![Value::string(msg)]));
    }
    Ok(def)
}

/// (local-set-key KEY COMMAND)
pub(super) fn builtin_local_set_key(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("local-set-key", &args, 2)?;
    let local = if eval.buffers.current_local_map().is_nil() {
        let km = make_sparse_list_keymap();
        let _ = eval.buffers.set_current_local_map(km);
        km
    } else {
        eval.buffers.current_local_map()
    };
    let events = expect_key_events(&args[0])?;
    let def = args[1];
    if let Err(msg) = list_keymap_define_seq_in_obarray(eval.obarray(), local, &events, def) {
        return Err(signal("error", vec![Value::string(msg)]));
    }
    Ok(def)
}

/// (use-local-map KEYMAP)
pub(super) fn builtin_use_local_map(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("use-local-map", &args, 1)?;
    let keymap = if args[0].is_nil() {
        Value::Nil
    } else {
        expect_keymap(eval, &args[0])?
    };
    let _ = eval.buffers.set_current_local_map(keymap);
    Ok(Value::Nil)
}


/// (use-global-map KEYMAP)
pub(super) fn builtin_use_global_map(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_use_global_map_in_obarray(&mut eval.obarray, &args)
}

pub(crate) fn builtin_use_global_map_in_obarray(
    obarray: &mut Obarray,
    args: &[Value],
) -> EvalResult {
    expect_args("use-global-map", args, 1)?;
    let keymap = expect_keymap_in_obarray(obarray, &args[0])?;
    obarray.set_symbol_value("global-map", keymap);
    Ok(Value::Nil)
}

/// (current-local-map) -> keymap or nil
pub(super) fn builtin_current_local_map(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_current_local_map_in_state(eval.buffers.current_local_map(), &args)
}

pub(crate) fn builtin_current_local_map_in_state(
    current_local_map: Value,
    args: &[Value],
) -> EvalResult {
    expect_args("current-local-map", args, 0)?;
    Ok(current_local_map)
}

/// (current-global-map) -> keymap
pub(super) fn builtin_current_global_map(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-global-map", &args, 0)?;
    Ok(ensure_global_keymap(eval))
}

/// `(current-active-maps &optional OLP POSITION)` -> list of active keymaps.
///
/// Returns list of currently active keymaps in priority order.
/// GNU Emacs order: minor-mode maps > local-map > global-map.
pub(super) fn builtin_current_active_maps(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_current_active_maps_in_state(
        &mut eval.obarray,
        eval.dynamic.as_slice(),
        eval.buffers.current_local_map(),
        &args,
    )
}

pub(crate) fn builtin_current_active_maps_in_state(
    obarray: &mut Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    current_local_map: Value,
    args: &[Value],
) -> EvalResult {
    expect_max_args("current-active-maps", &args, 2)?;

    let mut maps = Vec::new();

    // Collect minor mode keymaps (highest precedence).
    let minor_maps = collect_minor_mode_maps_in_state(obarray, dynamic);
    maps.extend(minor_maps);

    // Local map.
    if !current_local_map.is_nil() {
        maps.push(current_local_map);
    }

    // Global map (lowest precedence).
    maps.push(ensure_global_keymap_in_obarray(obarray));
    Ok(Value::list(maps))
}

/// `(current-minor-mode-maps)` -> list of active minor mode keymaps.
pub(super) fn builtin_current_minor_mode_maps(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_current_minor_mode_maps_in_state(eval.obarray(), eval.dynamic.as_slice(), &args)
}

pub(crate) fn builtin_current_minor_mode_maps_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    args: &[Value],
) -> EvalResult {
    expect_args("current-minor-mode-maps", &args, 0)?;
    let maps = collect_minor_mode_maps_in_state(obarray, dynamic);
    if maps.is_empty() {
        Ok(Value::Nil)
    } else {
        Ok(Value::list(maps))
    }
}

/// Collect all active minor mode keymaps in precedence order.
///
/// Mirrors GNU Emacs `current_minor_maps()` in keymap.c:
/// 1. `emulation-mode-map-alists` (highest precedence)
/// 2. `minor-mode-overriding-map-alist`
/// 3. `minor-mode-map-alist` (entries already in overriding alist are skipped)
fn collect_minor_mode_maps(eval: &super::eval::Context) -> Vec<Value> {
    collect_minor_mode_maps_in_state(eval.obarray(), eval.dynamic.as_slice())
}

fn collect_minor_mode_maps_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
) -> Vec<Value> {
    let mut maps = Vec::new();

    // 1. Emulation mode map alists (highest precedence).
    if let Some(emulation_raw) = super::misc_eval::dynamic_or_global_symbol_value_in_state(
        obarray,
        dynamic,
        "emulation-mode-map-alists",
    ) {
        if let Some(emulation_entries) = list_to_vec(&emulation_raw) {
            for entry in emulation_entries {
                // Each entry is either a symbol (whose value is an alist) or an alist directly.
                let alist_value = match entry.as_symbol_name() {
                    Some(name) => super::misc_eval::dynamic_or_global_symbol_value_in_state(
                        obarray, dynamic, name,
                    )
                    .unwrap_or(Value::Nil),
                    None => entry,
                };
                collect_maps_from_alist_in_state(obarray, dynamic, &alist_value, None, &mut maps);
            }
        }
    }

    // 2. minor-mode-overriding-map-alist.
    let overriding = super::misc_eval::dynamic_or_global_symbol_value_in_state(
        obarray,
        dynamic,
        "minor-mode-overriding-map-alist",
    );
    if let Some(ref ov) = overriding {
        collect_maps_from_alist_in_state(obarray, dynamic, ov, None, &mut maps);
    }

    // 3. minor-mode-map-alist (skip entries already in overriding alist).
    if let Some(regular) = super::misc_eval::dynamic_or_global_symbol_value_in_state(
        obarray,
        dynamic,
        "minor-mode-map-alist",
    ) {
        collect_maps_from_alist_in_state(
            obarray,
            dynamic,
            &regular,
            overriding.as_ref(),
            &mut maps,
        );
    }

    maps
}

/// Collect keymaps from a minor mode alist `((MODE-VAR . KEYMAP) ...)`.
///
/// For each entry where MODE-VAR is bound and non-nil, add KEYMAP to `maps`.
/// If `skip_if_in` is provided, skip entries whose MODE-VAR appears in that alist
/// (used to avoid duplicates between overriding and regular alists).
fn collect_maps_from_alist(
    eval: &super::eval::Context,
    alist: &Value,
    skip_if_in: Option<&Value>,
    maps: &mut Vec<Value>,
) {
    collect_maps_from_alist_in_state(
        eval.obarray(),
        eval.dynamic.as_slice(),
        alist,
        skip_if_in,
        maps,
    )
}

fn collect_maps_from_alist_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    alist: &Value,
    skip_if_in: Option<&Value>,
    maps: &mut Vec<Value>,
) {
    let Some(entries) = list_to_vec(alist) else {
        return;
    };
    for entry in entries {
        let Value::Cons(cell) = entry else {
            continue;
        };
        let (mode_var, keymap_val) = {
            let pair = read_cons(cell);
            (pair.car, pair.cdr)
        };
        let Some(mode_name) = mode_var.as_symbol_name() else {
            continue;
        };

        // Skip if this mode variable appears in the overriding alist.
        if let Some(skip_alist) = skip_if_in {
            if assq_in_alist(skip_alist, &mode_var) {
                continue;
            }
        }

        // Check if mode variable is bound and non-nil.
        let mode_active =
            super::misc_eval::dynamic_or_global_symbol_value_in_state(obarray, dynamic, mode_name)
                .is_some_and(|v| v.is_truthy());
        if !mode_active {
            continue;
        }

        // Resolve indirect keymaps (symbol → its function definition).
        let resolved = if is_list_keymap(&keymap_val) {
            keymap_val
        } else if let Some(sym_name) = keymap_val.as_symbol_name() {
            obarray
                .symbol_function(sym_name)
                .cloned()
                .filter(|v| is_list_keymap(v))
                .unwrap_or(Value::Nil)
        } else {
            Value::Nil
        };

        if !resolved.is_nil() && is_list_keymap(&resolved) {
            maps.push(resolved);
        }
    }
}

/// Check if a symbol appears as a car in an alist.
fn assq_in_alist(alist: &Value, key: &Value) -> bool {
    let Some(entries) = list_to_vec(alist) else {
        return false;
    };
    for entry in entries {
        if let Value::Cons(cell) = entry {
            let pair = read_cons(cell);
            if pair.car == *key {
                return true;
            }
        }
    }
    false
}

pub(crate) struct KeymapIterationPlan {
    pub(crate) bindings: Vec<(Value, Value)>,
    pub(crate) parent: Value,
}

pub(crate) fn plan_keymap_iteration(keymap: Value) -> KeymapIterationPlan {
    let Some(entries) = list_to_vec(&keymap) else {
        return KeymapIterationPlan {
            bindings: Vec::new(),
            parent: Value::Nil,
        };
    };

    let mut bindings = Vec::new();
    let mut parent = Value::Nil;

    for (i, entry) in entries.iter().enumerate() {
        if i == 0 && entry.is_symbol_named("keymap") {
            continue;
        }

        if is_list_keymap(entry) {
            parent = *entry;
            break;
        }

        match entry {
            Value::Cons(cell) => {
                let pair = read_cons(*cell);
                if !pair.cdr.is_nil() {
                    bindings.push((pair.car, pair.cdr));
                }
            }
            Value::Vector(obj_id) => {
                let items = with_heap(|h| h.get_vector(*obj_id).clone());
                for (idx, binding) in items.iter().enumerate() {
                    if !binding.is_nil() {
                        bindings.push((Value::Int(idx as i64), *binding));
                    }
                }
            }
            _ => {}
        }
    }

    KeymapIterationPlan { bindings, parent }
}

pub(crate) fn execute_keymap_iteration_callbacks(
    eval: &mut super::eval::Context,
    function: Value,
    bindings: &[(Value, Value)],
) -> Result<(), Flow> {
    for (event, binding) in bindings {
        eval.apply(function, vec![*event, *binding])?;
    }
    Ok(())
}

/// `(map-keymap FUNCTION KEYMAP &optional SORT-FIRST)` -> nil.
///
/// Call FUNCTION for each binding in KEYMAP and its parents.
/// FUNCTION receives two arguments: the event and the binding definition.
pub(super) fn builtin_map_keymap(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("map-keymap", &args, 2)?;
    expect_max_args("map-keymap", &args, 3)?;
    let function = args[0];
    let mut keymap = expect_keymap(eval, &args[1])?;

    // Traverse this keymap and all parents.
    loop {
        keymap = map_keymap_internal_impl(eval, function, keymap)?;
        if keymap.is_nil() {
            break;
        }
        // keymap is the parent; continue if it's a valid keymap.
        if !is_list_keymap(&keymap) {
            break;
        }
    }
    Ok(Value::Nil)
}

/// `(map-keymap-internal FUNCTION KEYMAP)` -> parent keymap or nil.
///
/// Call FUNCTION for each binding in KEYMAP (not its parents).
/// Returns the parent keymap if it has one.
pub(super) fn builtin_map_keymap_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("map-keymap-internal", &args, 2)?;
    let function = args[0];
    let keymap = expect_keymap(eval, &args[1])?;
    map_keymap_internal_impl(eval, function, keymap)
}

/// Core implementation: iterate over one level of keymap entries,
/// calling `function(event, binding)` for each. Returns the parent
/// keymap (or nil if none).
fn map_keymap_internal_impl(
    eval: &mut super::eval::Context,
    function: Value,
    keymap: Value,
) -> EvalResult {
    let plan = plan_keymap_iteration(keymap);
    execute_keymap_iteration_callbacks(eval, function, &plan.bindings)?;
    Ok(plan.parent)
}

/// (keymap-parent KEYMAP) -> keymap or nil
pub(super) fn builtin_keymap_parent(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_keymap_parent_in_obarray(eval.obarray(), &args)
}

pub(crate) fn builtin_keymap_parent_in_obarray(obarray: &Obarray, args: &[Value]) -> EvalResult {
    expect_args("keymap-parent", &args, 1)?;
    let keymap = expect_keymap_in_obarray(obarray, &args[0])?;
    Ok(list_keymap_parent(&keymap))
}

/// (set-keymap-parent KEYMAP PARENT) -> PARENT
pub(super) fn builtin_set_keymap_parent(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_set_keymap_parent_in_obarray(eval.obarray(), &args)
}

pub(crate) fn builtin_set_keymap_parent_in_obarray(
    obarray: &Obarray,
    args: &[Value],
) -> EvalResult {
    expect_args("set-keymap-parent", &args, 2)?;
    let keymap = expect_keymap_in_obarray(obarray, &args[0])?;
    let parent = if args[1].is_nil() {
        Value::Nil
    } else {
        expect_keymap_in_obarray(obarray, &args[1])?
    };
    list_keymap_set_parent(keymap, parent);
    Ok(args[1])
}

pub(super) fn is_lisp_keymap_object(value: &Value) -> bool {
    is_list_keymap(value)
}

/// (keymapp OBJ) -> t or nil
pub(super) fn builtin_keymapp(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_keymapp_in_obarray(eval.obarray(), &args)
}

pub(crate) fn builtin_keymapp_in_obarray(obarray: &Obarray, args: &[Value]) -> EvalResult {
    expect_args("keymapp", &args, 1)?;
    if is_list_keymap(&args[0]) {
        return Ok(Value::True);
    }
    // Check if it's a symbol whose function cell is a keymap
    if let Some(sym_name) = args[0].as_symbol_name() {
        if let Some(func) = obarray.symbol_function(sym_name) {
            if is_list_keymap(&func) {
                return Ok(Value::True);
            }
        }
    }
    Ok(Value::Nil)
}

/// `(event-convert-list EVENT-DESC)` -> event object or nil
pub(crate) fn builtin_event_convert_list(args: Vec<Value>) -> EvalResult {
    expect_args("event-convert-list", &args, 1)?;
    let Some(items) = list_to_vec(&args[0]) else {
        return Ok(Value::Nil);
    };
    if items.is_empty() {
        return Ok(Value::Nil);
    }
    if items.len() == 1 {
        return Ok(items[0]);
    }

    let mut mod_bits = 0i64;
    let mut base: Option<Value> = None;
    for item in items {
        if base.is_none() {
            if let Some(sym) = item.as_symbol_name() {
                if let Some(bit) = event_modifier_bit(sym) {
                    mod_bits |= bit;
                    continue;
                }
            }
            base = Some(item);
        } else {
            return Err(signal(
                "error",
                vec![Value::string("Invalid event description")],
            ));
        }
    }

    let Some(base) = base else {
        return Ok(Value::Nil);
    };

    match base {
        Value::Int(_) | Value::Char(_) => {
            let mut code = match base {
                Value::Int(i) => i,
                Value::Char(c) => c as i64,
                _ => unreachable!(),
            };

            let ctrl = (mod_bits & KEY_CHAR_CTRL) != 0;
            let shift = (mod_bits & KEY_CHAR_SHIFT) != 0;

            if shift && !ctrl && (97..=122).contains(&code) {
                code -= 32;
                mod_bits &= !KEY_CHAR_SHIFT;
            }
            if ctrl && code <= 31 {
                mod_bits &= !KEY_CHAR_CTRL;
            }
            if ctrl && code != 32 && code != 63 {
                if let Some(resolved) = resolve_control_code(code) {
                    if (65..=90).contains(&code) {
                        mod_bits |= KEY_CHAR_SHIFT;
                    }
                    code = resolved;
                    mod_bits &= !KEY_CHAR_CTRL;
                }
            }
            Ok(Value::Int(code | mod_bits))
        }
        Value::Symbol(id) => {
            let name = resolve_sym(id);
            if mod_bits == 0 {
                Ok(Value::symbol(name))
            } else {
                Ok(Value::symbol(format!(
                    "{}{}",
                    event_modifier_prefix(mod_bits),
                    name
                )))
            }
        }
        Value::Nil | Value::True => {
            if mod_bits == 0 {
                Ok(base)
            } else {
                Err(signal(
                    "error",
                    vec![Value::string("Invalid event description")],
                ))
            }
        }
        _ => Err(signal(
            "error",
            vec![Value::string("Invalid event description")],
        )),
    }
}

/// `(text-char-description CHARACTER)` -> printable text description.
pub(super) fn builtin_text_char_description(args: Vec<Value>) -> EvalResult {
    expect_args("text-char-description", &args, 1)?;
    let code = match &args[0] {
        Value::Int(n) if (0..=KEY_CHAR_CODE_MASK).contains(n) => *n,
        Value::Char(c) => *c as i64,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), args[0]],
            ));
        }
    };
    if (code & !KEY_CHAR_CODE_MASK) != 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), args[0]],
        ));
    }

    let rendered = match code {
        0 => "^@".to_string(),
        1..=26 => format!("^{}", char::from_u32((code as u32) + 64).unwrap_or('?')),
        27 => "^[".to_string(),
        28 => "^\\\\".to_string(),
        29 => "^]".to_string(),
        30 => "^^".to_string(),
        31 => "^_".to_string(),
        127 => "^?".to_string(),
        _ => match char::from_u32(code as u32) {
            Some(ch) => ch.to_string(),
            None => {
                if let Some(encoded) = encode_nonunicode_char_for_storage(code as u32) {
                    encoded
                } else {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("characterp"), args[0]],
                    ));
                }
            }
        },
    };
    Ok(Value::string(rendered))
}

pub(super) fn parse_event_symbol_prefixes(mut name: &str) -> (Vec<Value>, &str) {
    let mut mods = Vec::new();
    loop {
        if let Some(rest) = name.strip_prefix("C-") {
            mods.push(Value::symbol("control"));
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("M-") {
            mods.push(Value::symbol("meta"));
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("S-") {
            mods.push(Value::symbol("shift"));
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("s-") {
            mods.push(Value::symbol("super"));
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("H-") {
            mods.push(Value::symbol("hyper"));
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("A-") {
            mods.push(Value::symbol("alt"));
            name = rest;
            continue;
        }
        break;
    }
    (mods, name)
}

/// `(single-key-description KEY &optional NO-ANGLES)` -> string
pub(super) fn builtin_single_key_description(args: Vec<Value>) -> EvalResult {
    expect_range_args("single-key-description", &args, 1, 2)?;
    let no_angles = args.get(1).is_some_and(Value::is_truthy);
    Ok(Value::string(describe_single_key_value(
        &args[0], no_angles,
    )?))
}

/// `(key-description KEYS &optional PREFIX)` -> string
pub(crate) fn builtin_key_description(args: Vec<Value>) -> EvalResult {
    expect_range_args("key-description", &args, 1, 2)?;
    let mut events = if let Some(prefix) = args.get(1) {
        key_sequence_values(prefix)?
    } else {
        vec![]
    };
    events.extend(key_sequence_values(&args[0])?);
    let rendered: Result<Vec<String>, Flow> = events
        .iter()
        .map(|event| describe_single_key_value(event, false))
        .collect();
    Ok(Value::string(rendered?.join(" ")))
}

/// `(recent-keys &optional INCLUDE-CMDS)` -> vector of recent input events.
pub(crate) fn builtin_recent_keys(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_recent_keys_in_state(eval.recent_input_events(), args)
}

pub(crate) fn builtin_recent_keys_in_state(
    recent_input_events: &[Value],
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("recent-keys", &args, 1)?;
    Ok(Value::vector(recent_input_events.to_vec()))
}
