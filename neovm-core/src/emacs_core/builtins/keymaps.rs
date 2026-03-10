use super::*;
use crate::emacs_core::symbol::Obarray;

// ===========================================================================
// Keymap builtins
// ===========================================================================
use super::keymap::{
    KeyEvent, is_list_keymap, key_event_to_emacs_event, list_keymap_accessible, list_keymap_copy,
    list_keymap_define_seq, list_keymap_lookup_seq, list_keymap_parent, list_keymap_set_parent,
    make_list_keymap, make_sparse_list_keymap,
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

fn expect_keymap(eval: &super::eval::Evaluator, value: &Value) -> Result<Value, Flow> {
    expect_keymap_in_obarray(eval.obarray(), value)
}

/// Get the global keymap from obarray, creating one if needed.
fn ensure_global_keymap(eval: &mut super::eval::Evaluator) -> Value {
    if let Some(val) = eval.obarray.symbol_value("global-map").copied() {
        if is_list_keymap(&val) {
            return val;
        }
    }
    let km = make_list_keymap();
    eval.obarray.set_symbol_value("global-map", km);
    km
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    use super::value::with_heap;

    expect_min_args("accessible-keymaps", &args, 1)?;
    expect_max_args("accessible-keymaps", &args, 2)?;
    let keymap = expect_keymap(eval, &args[0])?;

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
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("make-keymap", &args, 1)?;
    Ok(make_list_keymap())
}

/// (make-sparse-keymap &optional NAME) -> keymap
pub(super) fn builtin_make_sparse_keymap(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("make-sparse-keymap", &args, 1)?;
    // Name argument is accepted but not stored in list keymap format
    // (official Emacs doesn't store it in the list either)
    Ok(make_sparse_list_keymap())
}

/// `(copy-keymap KEYMAP)` -> keymap copy.
pub(super) fn builtin_copy_keymap(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("copy-keymap", &args, 1)?;
    let keymap = expect_keymap(eval, &args[0])?;
    Ok(list_keymap_copy(&keymap))
}

/// (define-key KEYMAP KEY DEF &optional REMOVE) -> DEF
pub(super) fn builtin_define_key(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("define-key", &args, 3)?;
    expect_max_args("define-key", &args, 4)?;
    let keymap = expect_keymap(eval, &args[0])?;
    let events = expect_key_events(&args[1])?;
    let def = args[2];
    list_keymap_define_seq(keymap, &events, def);
    Ok(def)
}

/// (lookup-key KEYMAP KEY) -> binding or nil
pub(super) fn builtin_lookup_key(
    eval: &mut super::eval::Evaluator,
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

    Ok(list_keymap_lookup_seq(&keymap, &events))
}

/// (global-set-key KEY COMMAND)
pub(super) fn builtin_global_set_key(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("global-set-key", &args, 2)?;
    let global = ensure_global_keymap(eval);
    let events = expect_key_events(&args[0])?;
    let def = args[1];
    list_keymap_define_seq(global, &events, def);
    Ok(def)
}

/// (local-set-key KEY COMMAND)
pub(super) fn builtin_local_set_key(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("local-set-key", &args, 2)?;
    let local = if eval.current_local_map.is_nil() {
        let km = make_sparse_list_keymap();
        eval.current_local_map = km;
        km
    } else {
        eval.current_local_map
    };
    let events = expect_key_events(&args[0])?;
    let def = args[1];
    list_keymap_define_seq(local, &events, def);
    Ok(def)
}

/// (use-local-map KEYMAP)
pub(super) fn builtin_use_local_map(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("use-local-map", &args, 1)?;
    if args[0].is_nil() {
        eval.current_local_map = Value::Nil;
    } else {
        let keymap = expect_keymap(eval, &args[0])?;
        eval.current_local_map = keymap;
    }
    Ok(Value::Nil)
}

/// (use-global-map KEYMAP)
pub(super) fn builtin_use_global_map(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("use-global-map", &args, 1)?;
    let keymap = expect_keymap(eval, &args[0])?;
    eval.obarray.set_symbol_value("global-map", keymap);
    Ok(Value::Nil)
}

/// (current-local-map) -> keymap or nil
pub(super) fn builtin_current_local_map(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-local-map", &args, 0)?;
    Ok(eval.current_local_map)
}

/// (current-global-map) -> keymap
pub(super) fn builtin_current_global_map(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-global-map", &args, 0)?;
    Ok(ensure_global_keymap(eval))
}

/// `(current-active-maps &optional OLP POSITION)` -> list of active keymaps.
pub(super) fn builtin_current_active_maps(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("current-active-maps", &args, 2)?;

    let mut maps = Vec::new();
    if !eval.current_local_map.is_nil() {
        maps.push(eval.current_local_map);
    }
    maps.push(ensure_global_keymap(eval));
    Ok(Value::list(maps))
}

pub(super) fn builtin_current_minor_mode_maps(args: Vec<Value>) -> EvalResult {
    expect_args("current-minor-mode-maps", &args, 0)?;
    Ok(Value::Nil)
}

/// (keymap-parent KEYMAP) -> keymap or nil
pub(super) fn builtin_keymap_parent(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("keymap-parent", &args, 1)?;
    let keymap = expect_keymap(eval, &args[0])?;
    Ok(list_keymap_parent(&keymap))
}

/// (set-keymap-parent KEYMAP PARENT) -> PARENT
pub(super) fn builtin_set_keymap_parent(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-keymap-parent", &args, 2)?;
    let keymap = expect_keymap(eval, &args[0])?;
    let parent = if args[1].is_nil() {
        Value::Nil
    } else {
        expect_keymap(eval, &args[1])?
    };
    list_keymap_set_parent(keymap, parent);
    Ok(args[1])
}

pub(super) fn is_lisp_keymap_object(value: &Value) -> bool {
    is_list_keymap(value)
}

/// (keymapp OBJ) -> t or nil
pub(super) fn builtin_keymapp(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
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

/// (kbd STRING) -> string-or-vector
/// Parses key description text and returns Emacs-style event encoding.
pub(super) fn builtin_kbd(args: Vec<Value>) -> EvalResult {
    expect_args("kbd", &args, 1)?;
    let desc = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).clone()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    super::kbd::parse_kbd_string(&desc).map_err(|msg| signal("error", vec![Value::string(msg)]))
}

/// `(event-convert-list EVENT-DESC)` -> event object or nil
pub(super) fn builtin_event_convert_list(args: Vec<Value>) -> EvalResult {
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
pub(super) fn builtin_key_description(args: Vec<Value>) -> EvalResult {
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
pub(super) fn builtin_recent_keys(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("recent-keys", &args, 1)?;
    Ok(Value::vector(eval.recent_input_events().to_vec()))
}
