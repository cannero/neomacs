//! Keymap system — key binding lookup and command dispatch.
//!
//! Provides an Emacs-compatible keymap system with:
//! - Sparse and full keymaps
//! - Parent (inheritance) chain lookup
//! - Key description parsing (`kbd` style: "C-x C-f", "M-x", "RET", etc.)
//! - Global and local (buffer) keymap support

use std::collections::HashSet;

use super::builtins::{builtin_get_pos_property_impl, expect_integer_or_marker_in_buffers};
use super::chartable::{
    builtin_char_table_range, builtin_set_char_table_range, is_char_table, make_char_table_value,
};
use super::error::{EvalResult, Flow, signal};
use super::eval::Context;
use super::intern::resolve_sym;
use super::intern::{SymId, intern};
use super::keyboard::pure::{
    KEY_CHAR_ALT, KEY_CHAR_CODE_MASK, KEY_CHAR_CTRL, KEY_CHAR_HYPER, KEY_CHAR_META,
    KEY_CHAR_MOD_MASK, KEY_CHAR_SHIFT, KEY_CHAR_SUPER,
};
use super::symbol::Obarray;
use super::value::{
    OrderedRuntimeBindingMap, Value, ValueKind, VecLikeType, eq_value, list_to_vec,
};

// ---------------------------------------------------------------------------
// Key events
// ---------------------------------------------------------------------------

/// A key event — a single keystroke with optional modifiers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum KeyEvent {
    /// A regular character with modifiers.
    Char {
        code: char,
        ctrl: bool,
        meta: bool,
        shift: bool,
        super_: bool,
        hyper: bool,
        alt: bool,
    },
    /// A named function/special key (e.g. "return", "backspace", "f1").
    Function {
        name: String,
        ctrl: bool,
        meta: bool,
        shift: bool,
        super_: bool,
        hyper: bool,
        alt: bool,
    },
}

// ---------------------------------------------------------------------------
// Conversion from keyboard::KeyEvent → keymap::KeyEvent
// ---------------------------------------------------------------------------

impl From<crate::keyboard::KeyEvent> for KeyEvent {
    fn from(ke: crate::keyboard::KeyEvent) -> Self {
        use crate::keyboard::{Key, NamedKey};
        match ke.key {
            Key::Char(c) => KeyEvent::Char {
                code: c,
                ctrl: ke.modifiers.ctrl,
                meta: ke.modifiers.meta,
                shift: ke.modifiers.shift,
                super_: ke.modifiers.super_,
                hyper: ke.modifiers.hyper,
                alt: false,
            },
            Key::Named(named) => {
                if matches!(named, NamedKey::Escape) {
                    return KeyEvent::Char {
                        code: '\u{1b}',
                        ctrl: ke.modifiers.ctrl,
                        meta: ke.modifiers.meta,
                        shift: ke.modifiers.shift,
                        super_: ke.modifiers.super_,
                        hyper: ke.modifiers.hyper,
                        alt: false,
                    };
                }
                let name = match named {
                    NamedKey::Escape => {
                        unreachable!("escape is handled above as a literal ESC char")
                    }
                    NamedKey::Return => "return",
                    NamedKey::Tab => "tab",
                    NamedKey::Backspace => "backspace",
                    NamedKey::Delete => "delete",
                    NamedKey::Insert => "insert",
                    NamedKey::Home => "home",
                    NamedKey::End => "end",
                    NamedKey::PageUp => "prior",
                    NamedKey::PageDown => "next",
                    NamedKey::Left => "left",
                    NamedKey::Right => "right",
                    NamedKey::Up => "up",
                    NamedKey::Down => "down",
                    NamedKey::F(n) => {
                        return KeyEvent::Function {
                            name: format!("f{}", n),
                            ctrl: ke.modifiers.ctrl,
                            meta: ke.modifiers.meta,
                            shift: ke.modifiers.shift,
                            super_: ke.modifiers.super_,
                            hyper: ke.modifiers.hyper,
                            alt: false,
                        };
                    }
                };
                KeyEvent::Function {
                    name: name.to_string(),
                    ctrl: ke.modifiers.ctrl,
                    meta: ke.modifiers.meta,
                    shift: ke.modifiers.shift,
                    super_: ke.modifiers.super_,
                    hyper: ke.modifiers.hyper,
                    alt: false,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Key description parsing  ("kbd" style)
// ---------------------------------------------------------------------------

/// Parse a key description string into a sequence of `KeyEvent`s.
///
/// Supported syntax:
/// - `"C-x"` — Ctrl+x
/// - `"M-x"` — Meta(Alt)+x
/// - `"S-x"` — Shift+x
/// - `"s-x"` — Super+x
/// - `"C-M-x"` — Ctrl+Meta+x
/// - `"C-x C-f"` — sequence of Ctrl+x then Ctrl+f
/// - `"RET"`, `"TAB"`, `"SPC"`, `"ESC"`, `"DEL"`, `"BS"` — named keys
/// - `"f1"` .. `"f12"` — function keys
/// - `"a"`, `"b"`, `"1"`, `"!"` — plain characters
pub fn parse_key_description(desc: &str) -> Result<Vec<KeyEvent>, String> {
    let desc = desc.trim();
    if desc.is_empty() {
        return Err("empty key description".to_string());
    }

    let mut result = Vec::new();
    for part in desc.split_whitespace() {
        result.push(parse_single_key(part)?);
    }
    Ok(result)
}

/// Parse a single key token (e.g. "C-x", "M-RET", "a", "f1").
pub fn parse_single_key(token: &str) -> Result<KeyEvent, String> {
    let mut ctrl = false;
    let mut meta = false;
    let mut shift = false;
    let mut super_ = false;
    let mut hyper = false;
    let mut alt = false;

    let mut remainder = token;

    // Parse modifier prefixes: "C-", "M-", "S-", "s-", "H-", "A-"
    loop {
        if let Some(rest) = remainder.strip_prefix("C-") {
            ctrl = true;
            remainder = rest;
        } else if let Some(rest) = remainder.strip_prefix("M-") {
            meta = true;
            remainder = rest;
        } else if remainder.starts_with("S-") && remainder.len() > 2 {
            let rest = &remainder[2..];
            shift = true;
            remainder = rest;
        } else if remainder.starts_with("s-") && remainder.len() > 2 {
            let rest = &remainder[2..];
            super_ = true;
            remainder = rest;
        } else if remainder.starts_with("H-") && remainder.len() > 2 {
            let rest = &remainder[2..];
            hyper = true;
            remainder = rest;
        } else if remainder.starts_with("A-") && remainder.len() > 2 {
            let rest = &remainder[2..];
            alt = true;
            remainder = rest;
        } else {
            break;
        }
    }

    if remainder.is_empty() {
        return Err(format!("incomplete key description: {}", token));
    }

    // Helper to build a Function event with current modifiers
    let mk_func = |name: &str| -> KeyEvent {
        KeyEvent::Function {
            name: name.to_string(),
            ctrl,
            meta,
            shift,
            super_,
            hyper,
            alt,
        }
    };

    // Check for named special keys
    match remainder {
        "RET" | "return" => Ok(mk_func("return")),
        "TAB" | "tab" => Ok(mk_func("tab")),
        "SPC" | "space" => Ok(KeyEvent::Char {
            code: ' ',
            ctrl,
            meta,
            shift,
            super_,
            hyper,
            alt,
        }),
        "ESC" | "escape" => Ok(KeyEvent::Char {
            code: '\u{1b}',
            ctrl,
            meta,
            shift,
            super_,
            hyper,
            alt,
        }),
        "DEL" | "delete" => Ok(mk_func("delete")),
        "BS" | "backspace" => Ok(mk_func("backspace")),
        "up" => Ok(mk_func("up")),
        "down" => Ok(mk_func("down")),
        "left" => Ok(mk_func("left")),
        "right" => Ok(mk_func("right")),
        "home" => Ok(mk_func("home")),
        "end" => Ok(mk_func("end")),
        "prior" | "page-up" => Ok(mk_func("prior")),
        "next" | "page-down" => Ok(mk_func("next")),
        "insert" => Ok(mk_func("insert")),
        other => {
            // Check for function keys: f1 .. f20
            if let Some(stripped) = other.strip_prefix('f') {
                if let Ok(n) = stripped.parse::<u32>() {
                    if (1..=20).contains(&n) {
                        return Ok(mk_func(&format!("f{}", n)));
                    }
                }
            }

            // Single character
            let mut chars = other.chars();
            let ch = chars
                .next()
                .ok_or_else(|| format!("empty key after modifiers: {}", token))?;
            if chars.next().is_some() {
                return Err(format!("unknown key name: {}", other));
            }
            Ok(KeyEvent::Char {
                code: ch,
                ctrl,
                meta,
                shift,
                super_,
                hyper,
                alt,
            })
        }
    }
}

/// Format a key event back to a human-readable description string.
pub fn format_key_event(event: &KeyEvent) -> String {
    let mut parts = String::new();
    let (ctrl, meta, shift, super_, hyper, alt) = match event {
        KeyEvent::Char {
            ctrl,
            meta,
            shift,
            super_,
            hyper,
            alt,
            ..
        } => (*ctrl, *meta, *shift, *super_, *hyper, *alt),
        KeyEvent::Function {
            ctrl,
            meta,
            shift,
            super_,
            hyper,
            alt,
            ..
        } => (*ctrl, *meta, *shift, *super_, *hyper, *alt),
    };
    if alt {
        parts.push_str("A-");
    }
    if ctrl {
        parts.push_str("C-");
    }
    if hyper {
        parts.push_str("H-");
    }
    if meta {
        parts.push_str("M-");
    }
    if shift {
        parts.push_str("S-");
    }
    if super_ {
        parts.push_str("s-");
    }
    match event {
        KeyEvent::Char { code: ' ', .. } => {
            parts.push_str("SPC");
        }
        KeyEvent::Char { code: '\r', .. } => {
            parts.push_str("RET");
        }
        KeyEvent::Char { code: '\t', .. } => {
            parts.push_str("TAB");
        }
        KeyEvent::Char { code: '\u{7f}', .. } => {
            parts.push_str("DEL");
        }
        KeyEvent::Char { code: '\u{1b}', .. } => {
            parts.push_str("ESC");
        }
        KeyEvent::Char { code, .. } => {
            parts.push(*code);
        }
        KeyEvent::Function { name, .. } => match name.as_str() {
            "return" => parts.push_str("RET"),
            "tab" => parts.push_str("TAB"),
            "escape" => parts.push_str("ESC"),
            "delete" => parts.push_str("DEL"),
            "backspace" => parts.push_str("BS"),
            other => parts.push_str(other),
        },
    }
    parts
}

/// Format a full key sequence.
pub fn format_key_sequence(events: &[KeyEvent]) -> String {
    events
        .iter()
        .map(format_key_event)
        .collect::<Vec<_>>()
        .join(" ")
}

// ===========================================================================
// Emacs-compatible list keymaps
// ===========================================================================
//
// Official Emacs keymap format:
//   Full keymap:   (keymap CHAR-TABLE (EVENT . DEF) (EVENT . DEF) ...)
//   Sparse keymap: (keymap (EVENT . DEF) (EVENT . DEF) ...)
//   With parent:   (keymap (EVENT . DEF) ... . PARENT-KEYMAP)
//
// - `keymapp` checks `(consp x) && (car x) == 'keymap`
// - Char-table stores character bindings (0-MAX_CHAR)
// - Alist stores non-character bindings (function keys, mouse, remap, modified chars)
// - Events: integers (char code with modifier bits) or symbols (function keys)
// - Parent keymap: last CDR in the list, itself a `(keymap ...)` list

/// Create a full list keymap: `(keymap CHAR-TABLE)`
pub fn make_list_keymap() -> Value {
    let char_table = make_char_table_value(Value::NIL, Value::NIL);
    Value::list(vec![Value::symbol("keymap"), char_table])
}

/// Create a sparse list keymap: `(keymap)` — a single-element list.
pub fn make_sparse_list_keymap() -> Value {
    Value::list(vec![Value::symbol("keymap")])
}

/// Check if a value is a keymap: `(consp x) && (car x) == 'keymap`.
pub fn is_list_keymap(v: &Value) -> bool {
    match v.kind() {
        ValueKind::Cons => v.cons_car().as_symbol_name() == Some("keymap"),
        _ => false,
    }
}

fn keymap_symbol_id(value: &Value) -> Option<SymId> {
    match value.kind() {
        ValueKind::Nil => Some(intern("nil")),
        ValueKind::T => Some(intern("t")),
        ValueKind::Symbol(id) => Some(id),
        _ => None,
    }
}

fn resolve_indirect_function_by_id_in_obarray(
    obarray: &Obarray,
    symbol: SymId,
) -> Option<(SymId, Value)> {
    let mut current = symbol;
    let mut seen = HashSet::new();

    loop {
        if !seen.insert(current) {
            return None;
        }
        let function = obarray.get_by_id(current)?.function?;
        if let Some(next_symbol) = keymap_symbol_id(&function) {
            current = next_symbol;
            continue;
        }
        return Some((current, function));
    }
}

pub(crate) fn is_keymap_autoload_form(value: &Value) -> bool {
    if !crate::emacs_core::autoload::is_autoload_value(value) {
        return false;
    }
    list_to_vec(value)
        .and_then(|items| items.get(4).copied())
        .is_some_and(|kind| kind.as_symbol_name() == Some("keymap"))
}

pub(crate) fn get_keymap_in_obarray(
    obarray: &Obarray,
    value: &Value,
    error_if_not_keymap: bool,
) -> Result<Value, Flow> {
    if value.is_nil() {
        return if error_if_not_keymap {
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("keymapp"), *value],
            ))
        } else {
            Ok(Value::NIL)
        };
    }

    if is_list_keymap(value) {
        return Ok(*value);
    }

    if let Some(symbol) = keymap_symbol_id(value)
        && let Some((_, function)) = resolve_indirect_function_by_id_in_obarray(obarray, symbol)
    {
        if is_list_keymap(&function) {
            return Ok(function);
        }
        if is_keymap_autoload_form(&function) && !error_if_not_keymap {
            return Ok(*value);
        }
    }

    if error_if_not_keymap {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("keymapp"), *value],
        ))
    } else {
        Ok(Value::NIL)
    }
}

pub(crate) fn maybe_keymap_in_obarray(obarray: &Obarray, value: &Value) -> Option<Value> {
    get_keymap_in_obarray(obarray, value, false)
        .ok()
        .filter(is_list_keymap)
}

pub(crate) fn get_keymap_in_runtime(
    eval: &mut Context,
    value: &Value,
    error_if_not_keymap: bool,
    autoload: bool,
) -> EvalResult {
    let original = *value;
    let mut current = original;

    loop {
        if current.is_nil() {
            break;
        }
        if is_list_keymap(&current) {
            return Ok(current);
        }

        let Some(symbol) = keymap_symbol_id(&current) else {
            break;
        };
        let Some((_, function)) =
            resolve_indirect_function_by_id_in_obarray(eval.obarray(), symbol)
        else {
            break;
        };

        if is_list_keymap(&function) {
            return Ok(function);
        }

        if is_keymap_autoload_form(&function) {
            if autoload {
                current = crate::emacs_core::autoload::builtin_autoload_do_load(
                    eval,
                    vec![function, original, Value::NIL],
                )?;
                continue;
            }
            if !error_if_not_keymap {
                return Ok(original);
            }
        }

        break;
    }

    if error_if_not_keymap {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("keymapp"), original],
        ))
    } else {
        Ok(Value::NIL)
    }
}

pub(crate) fn maybe_keymap_in_runtime(
    eval: &mut Context,
    value: &Value,
    autoload: bool,
) -> EvalResult {
    let resolved = get_keymap_in_runtime(eval, value, false, autoload)?;
    if is_list_keymap(&resolved) {
        Ok(resolved)
    } else {
        Ok(Value::NIL)
    }
}

/// Strip menu-item wrappers from a keymap binding, mirroring `get_keyelt`
/// in official Emacs `keymap.c`.
///
/// - `(STRING . DEFN)` → `DEFN`  (menu label)
/// - `(STRING . (STRING . DEFN))` → `DEFN`  (menu label + help string)
/// - `(menu-item NAME DEFN ...)` → `DEFN`  (extended menu item)
/// - anything else → returned as-is
fn get_keyelt(binding: Value) -> Value {
    let mut obj = binding;
    loop {
        if !obj.is_cons() {
            return obj;
        };
        let pair_car = obj.cons_car();
        let pair_cdr = obj.cons_cdr();
        if pair_car.is_string() {
            // (STRING . REST) — strip the menu label
            obj = pair_cdr;
            // Also strip a second string (help string)
            if obj.is_cons() {
                let p2_car = obj.cons_car();
                let p2_cdr = obj.cons_cdr();
                if p2_car.is_string() {
                    obj = p2_cdr;
                }
            }
            continue;
        }
        if pair_car.is_symbol_named("menu-item") {
            // (menu-item NAME DEFN . PROPS) — extract DEFN (third element)
            if pair_cdr.is_cons() {
                let p1_cdr = pair_cdr.cons_cdr();
                if p1_cdr.is_cons() {
                    let p2_car = p1_cdr.cons_car();
                    return p2_car;
                }
            }
            return Value::NIL;
        }
        return obj;
    }
}

/// Look up a single event in a keymap, following the parent chain.
///
/// This mirrors GNU Emacs `access_keymap` with `noinherit=false, t_ok=false`.
/// When a prefix keymap is found, it is composed with parent prefix
/// keymaps to create a merged keymap that includes all bindings from
/// the entire inheritance chain.
///
/// Returns the binding or `Value::NIL` if not found.
pub fn list_keymap_lookup_one(keymap: &Value, event: &Value) -> Value {
    list_keymap_access(keymap, event, false, false)
}

/// Look up a single event in a keymap, following the parent chain,
/// accepting `(t . COMMAND)` default bindings.
///
/// This mirrors GNU Emacs `access_keymap` with `noinherit=false, t_ok=true`.
pub fn list_keymap_lookup_one_t_ok(keymap: &Value, event: &Value) -> Value {
    list_keymap_access(keymap, event, false, true)
}

/// Look up a single event in a keymap without following the parent chain.
///
/// This mirrors GNU Emacs `access_keymap` with `noinherit=true`.
/// Used by `define-key` to only check the current keymap level.
pub fn list_keymap_lookup_one_noinherit(keymap: &Value, event: &Value) -> Value {
    list_keymap_access(keymap, event, true, false)
}

/// Look up a single event in one level of a keymap (no parent following).
///
/// Helper: scans only the entries in the given keymap (not parents).
/// Returns `Some(binding)` if found (even if binding is nil), or
/// `None` if not found. This distinction is critical: an explicit
/// nil binding shadows parent bindings, while "not found" falls through.
///
/// When `t_ok` is true, a `(t . COMMAND)` entry is accepted as a
/// default binding, matching GNU `access_keymap_1`'s `t_ok` parameter.
fn lookup_in_keymap_level(keymap: &Value, event: &Value, t_ok: bool) -> Option<Value> {
    if !keymap.is_cons() {
        return None;
    };
    let pair_car = keymap.cons_car();
    let pair_cdr = keymap.cons_cdr();
    if pair_car.as_symbol_name() != Some("keymap") {
        return None;
    }

    let mut cursor = pair_cdr;
    let mut entries = 0;
    let mut t_binding: Option<Value> = None;
    while cursor.is_cons() {
        if is_list_keymap(&cursor) {
            break; // parent boundary
        }
        entries += 1;
        if entries > 100_000 {
            break;
        }
        let entry_car = cursor.cons_car();
        let entry_cdr = cursor.cons_cdr();

        // Char-table: only look up characters WITHOUT modifier bits.
        // GNU keymap.c:441-450: nil in char-table means unbound;
        // Qt means explicitly nil binding.
        if is_char_table(&entry_car) {
            if let Some(code) = event.as_fixnum() {
                if (code & KEY_CHAR_MOD_MASK) == 0 {
                    let base = code & KEY_CHAR_CODE_MASK;
                    if base >= 0 && base <= 0x3FFFFF {
                        let result =
                            builtin_char_table_range(vec![entry_car, *event]).unwrap_or(Value::NIL);
                        if !result.is_nil() {
                            // Qt in char-table means explicitly nil binding
                            // (shadows parent), matching GNU keymap.c:455-459
                            let val = if result == Value::T {
                                Value::NIL
                            } else {
                                result
                            };
                            return Some(get_keyelt(val));
                        }
                        // nil in char-table means unbound — fall through
                    }
                }
            }
            cursor = entry_cdr;
            continue;
        }

        // Vector element in keymap spine: maps char codes 0..len to
        // bindings by index. Matches GNU keymap.c:431-434.
        if entry_car.is_vector() {
            if let Some(code) = event.as_fixnum() {
                if code >= 0 {
                    let idx = code as usize;
                    let items = entry_car.as_vector_data().unwrap();
                    if idx < items.len() {
                        let val = items[idx];
                        if !val.is_nil() {
                            return Some(get_keyelt(val));
                        }
                    }
                }
            }
            cursor = entry_cdr;
            continue;
        }

        // Alist entry: (EVENT . DEF)
        if entry_car.is_cons() {
            let binding_car = entry_car.cons_car();
            let binding_cdr = entry_car.cons_cdr();
            if events_match(&binding_car, event) {
                return Some(get_keyelt(binding_cdr));
            }
            // Check for (t . COMMAND) default binding.
            // GNU keymap.c:425-429: when t_ok, record the first t binding
            // but keep scanning for a specific match.
            if t_ok && t_binding.is_none() && binding_car == Value::T {
                t_binding = Some(get_keyelt(binding_cdr));
            }
        }

        cursor = entry_cdr;
    }

    // If no specific binding found but we have a t default binding, use it.
    // Matches GNU keymap.c:486-487.
    t_binding
}

/// Get the parent keymap from a keymap (the tail after all alist entries).
fn get_keymap_tail_parent(keymap: &Value) -> Value {
    if !keymap.is_cons() {
        return Value::NIL;
    };
    let pair_car = keymap.cons_car();
    let pair_cdr = keymap.cons_cdr();
    if pair_car.as_symbol_name() != Some("keymap") {
        return Value::NIL;
    }
    let mut cursor = pair_cdr;
    while cursor.is_cons() {
        if is_list_keymap(&cursor) {
            return cursor;
        }
        cursor = cursor.cons_cdr();
    }
    Value::NIL
}

/// Core event lookup in a keymap, optionally following the parent chain.
///
/// Mirrors GNU Emacs `access_keymap`:
/// - Walks the keymap list scanning bindings (char-tables, alist entries)
/// - When `noinherit` is false: follows parent keymap chain; if a prefix
///   keymap is found, it composes it with prefix keymaps from parent
///   levels to create a proper inheritance chain
/// - When `noinherit` is true: stops at the first parent boundary
/// - When `t_ok` is true: accepts `(t . COMMAND)` default bindings
///
/// An explicit nil binding (e.g. from `define-key m [?b] nil`) shadows
/// parent bindings, matching GNU Emacs behavior where nil != unbound.
fn list_keymap_access(keymap: &Value, event: &Value, noinherit: bool, t_ok: bool) -> Value {
    let mut current = *keymap;
    let mut depth = 0;
    const MAX_KEYMAP_DEPTH: usize = 50;

    loop {
        depth += 1;
        if depth > MAX_KEYMAP_DEPTH {
            tracing::warn!("list_keymap_access: depth limit reached, possible cycle");
            return Value::NIL;
        }

        // Look up the event in the current keymap level only.
        // Some(val) means "found" (val may be nil for explicit nil binding).
        // None means "not found at this level".
        match lookup_in_keymap_level(&current, event, t_ok) {
            Some(binding) => {
                if !noinherit && is_list_keymap(&binding) {
                    // Found a prefix keymap at this level. Check if parent
                    // also has a prefix keymap for the same event. If so,
                    // create a composed keymap: (keymap child-sub . parent-sub)
                    let parent = get_keymap_tail_parent(&current);
                    if !parent.is_nil() {
                        let parent_binding = list_keymap_access(&parent, event, false, t_ok);
                        if is_list_keymap(&parent_binding) {
                            return compose_keymaps(&binding, &parent_binding);
                        }
                    }
                }
                // Return the found binding (even if nil — nil shadows parents)
                return binding;
            }
            None => {
                // No binding at this level. Follow parent chain if allowed.
                if noinherit {
                    return Value::NIL;
                }
                let parent = get_keymap_tail_parent(&current);
                if parent.is_nil() {
                    return Value::NIL;
                }
                current = parent;
            }
        }
    }
}

/// Create a composed keymap: a shallow copy of `child` with `parent` set
/// as its parent keymap. This does NOT mutate either input keymap.
///
/// Result: `(keymap <child entries>... . parent)`
fn compose_keymaps(child: &Value, parent: &Value) -> Value {
    if !child.is_cons() {
        return *parent;
    };
    let pair_car = child.cons_car();
    let pair_cdr = child.cons_cdr();
    if pair_car.as_symbol_name() != Some("keymap") {
        return *parent;
    }

    // Collect child's own entries (excluding its existing parent)
    let mut elements = vec![Value::symbol("keymap")];
    let mut cursor = pair_cdr;
    while cursor.is_cons() {
        if is_list_keymap(&cursor) {
            // Don't include child's existing parent; we'll set a new one
            break;
        }
        let entry_car = cursor.cons_car();
        let entry_cdr = cursor.cons_cdr();
        elements.push(entry_car);
        cursor = entry_cdr;
    }

    // Build: (keymap entries... . parent)
    let mut result = *parent;
    for elem in elements.into_iter().rev() {
        result = Value::cons(elem, result);
    }
    result
}

/// Check if two event values match for keymap lookup purposes.
fn events_match(a: &Value, b: &Value) -> bool {
    let normalize = |value: &Value| {
        if value.is_cons() {
            value.cons_car()
        } else {
            *value
        }
    };
    let a = normalize(a);
    let b = normalize(b);

    match (a.kind(), b.kind()) {
        (ValueKind::Fixnum(x), ValueKind::Fixnum(y)) => x == y,
        (ValueKind::Symbol(x), ValueKind::Symbol(y)) => x == y,
        _ => false,
    }
}

pub(crate) fn expand_meta_prefix_char_events_in_obarray(
    obarray: &Obarray,
    events: &[Value],
) -> Option<Vec<Value>> {
    let meta_prefix = match obarray
        .symbol_value("meta-prefix-char")
        .and_then(|v| v.as_fixnum())
    {
        Some(code) => code,
        None => return None,
    };

    let mut changed = false;
    let mut expanded = Vec::with_capacity(events.len() + 1);
    for event in events {
        match event.kind() {
            ValueKind::Fixnum(code) if (code & KEY_CHAR_META) != 0 => {
                changed = true;
                expanded.push(Value::fixnum(meta_prefix));
                expanded.push(Value::fixnum(code & !KEY_CHAR_META));
            }
            _ => expanded.push(*event),
        }
    }

    changed.then_some(expanded)
}

pub(crate) fn resolve_prefix_keymap_binding_in_obarray(
    obarray: &Obarray,
    binding: &Value,
) -> Option<Value> {
    if is_list_keymap(binding) {
        return Some(*binding);
    }
    maybe_keymap_in_obarray(obarray, binding)
}

pub(crate) fn lookup_key_in_obarray(
    obarray: &Obarray,
    keymap: &Value,
    events: &[Value],
    t_ok: bool,
) -> Value {
    if events.is_empty() {
        return *keymap;
    }

    let mut current_map = *keymap;
    for (i, event) in events.iter().enumerate() {
        let binding = if t_ok {
            list_keymap_lookup_one_t_ok(&current_map, event)
        } else {
            list_keymap_lookup_one(&current_map, event)
        };
        let is_last = i == events.len() - 1;

        if is_last {
            return binding;
        }

        if binding.is_nil() {
            return Value::fixnum((i + 1) as i64);
        }

        if let Some(prefix_keymap) = resolve_prefix_keymap_binding_in_obarray(obarray, &binding) {
            current_map = prefix_keymap;
            continue;
        }

        return Value::fixnum((i + 1) as i64);
    }

    Value::NIL
}

pub(crate) fn lookup_key_in_keymaps_in_obarray(
    obarray: &Obarray,
    keymaps: &[Value],
    events: &[Value],
    t_ok: bool,
) -> Value {
    if events.is_empty() {
        return keymaps.first().copied().unwrap_or(Value::NIL);
    }

    let mut best = Value::NIL;
    for keymap in keymaps {
        let direct = lookup_key_in_obarray(obarray, keymap, events, t_ok);
        if !direct.is_nil() && !direct.is_fixnum() {
            return direct;
        }

        if let Some(expanded) = expand_meta_prefix_char_events_in_obarray(obarray, events) {
            let expanded_result = lookup_key_in_obarray(obarray, keymap, &expanded, t_ok);
            if !expanded_result.is_nil() && !expanded_result.is_fixnum() {
                return expanded_result;
            }
        }

        if best.is_nil() {
            best = direct;
        }
    }

    best
}

/// Define a binding in a keymap.
///
/// For integer events without modifier bits in full keymaps: stores in char-table.
/// Otherwise: updates existing alist entry in-place or prepends `(event . def)`.
///
/// When `remove` is true, removes the binding entry from the alist (or
/// stores nil in char-table), matching GNU keymap.c `store_in_keymap` with
/// `remove=true`.
pub fn list_keymap_define(keymap: Value, event: Value, def: Value) {
    store_in_keymap(keymap, event, def, false);
}

/// Remove a binding from a keymap, matching GNU `define-key` with REMOVE arg.
pub fn list_keymap_remove(keymap: Value, event: Value) {
    store_in_keymap(keymap, event, Value::NIL, true);
}

/// Core store/remove implementation matching GNU `store_in_keymap`.
fn store_in_keymap(keymap: Value, event: Value, def: Value, remove: bool) {
    if !keymap.is_cons() {
        return;
    };
    let root_car = keymap.cons_car();
    let root_cdr = keymap.cons_cdr();
    if root_car.as_symbol_name() != Some("keymap") {
        return;
    }

    // Scan the keymap for existing bindings, tracking insertion point.
    // GNU keymap.c: insertion_point starts at keymap; if a char-table or
    // vector is found, insertion_point is updated to point after it.
    let mut insertion_point = keymap;
    let mut cursor = root_cdr;
    while cursor.is_cons() {
        if is_list_keymap(&cursor) {
            // Hit a parent keymap boundary — stop scanning
            break;
        }
        let entry_car = cursor.cons_car();
        let entry_cdr = cursor.cons_cdr();

        // Char-table: handle plain character events (no modifier bits).
        // GNU keymap.c:805-829
        if is_char_table(&entry_car) {
            if let Some(code) = event.as_fixnum() {
                let mods = code & KEY_CHAR_MOD_MASK;
                if mods == 0 {
                    let base = code & KEY_CHAR_CODE_MASK;
                    if base >= 0 && base <= 0x3FFFFF {
                        let store_val = if remove {
                            Value::NIL
                        } else if def.is_nil() {
                            // nil has special meaning for char-tables (unbound),
                            // so use Qt (Value::T) for explicitly nil binding.
                            // GNU keymap.c:813-814
                            Value::T
                        } else {
                            def
                        };
                        let _ = builtin_set_char_table_range(vec![entry_car, event, store_val]);
                        return;
                    }
                }
            }
            insertion_point = cursor;
            cursor = entry_cdr;
            continue;
        }

        // Vector element: check for matching index.
        // GNU keymap.c:783-803
        if entry_car.is_vector() {
            if let Some(code) = event.as_fixnum() {
                let idx = code as usize;
                let updated = entry_car
                    .with_vector_data_mut(|vec_data| {
                        if idx < vec_data.len() {
                            vec_data[idx] = def;
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap();
                if updated {
                    return;
                }
            }
            insertion_point = cursor;
            cursor = entry_cdr;
            continue;
        }

        // Alist entry: (EVENT . DEF) — check for existing binding to update in-place.
        // GNU keymap.c:842-849
        if entry_car.is_cons() {
            let binding_car = entry_car.cons_car();
            if events_match(&binding_car, &event) {
                if remove {
                    // Remove the entry: splice it out of the list.
                    // Set insertion_point's cdr to skip this entry.
                    insertion_point.set_cdr(entry_cdr);
                } else {
                    // Update in-place: set the cdr of the binding cons.
                    entry_car.set_cdr(def);
                }
                return;
            }
        }

        // Check for 'keymap symbol in spine (start of inherited keymap)
        // GNU keymap.c:871-876
        if entry_car.is_symbol_named("keymap") {
            break;
        }

        insertion_point = cursor;
        cursor = entry_cdr;
    }

    // No existing binding found. Append new entry after insertion_point.
    if !remove {
        let binding = Value::cons(event, def);
        let old_cdr = match insertion_point.kind() {
            ValueKind::Cons => insertion_point.cons_cdr(),
            _ => Value::NIL,
        };
        let new_cdr = Value::cons(binding, old_cdr);
        insertion_point.set_cdr(new_cdr);
    }
}

/// Get the parent keymap (last CDR that is itself a keymap).
pub fn list_keymap_parent(keymap: &Value) -> Value {
    if !keymap.is_cons() {
        return Value::NIL;
    };
    let pair_car = keymap.cons_car();
    let pair_cdr = keymap.cons_cdr();
    if pair_car.as_symbol_name() != Some("keymap") {
        return Value::NIL;
    }

    let mut cursor = pair_cdr;
    while cursor.is_cons() {
        // Check if cursor itself is a parent keymap before treating as alist entry
        if is_list_keymap(&cursor) {
            return cursor;
        }
        let entry_cdr = cursor.cons_cdr();
        if entry_cdr.is_nil() {
            return Value::NIL;
        }
        cursor = entry_cdr;
    }
    Value::NIL
}

/// Set the parent keymap: walk to the last alist cons cell, set its CDR.
pub fn list_keymap_set_parent(keymap: Value, parent: Value) {
    if !keymap.is_cons() {
        return;
    };
    let root_car = keymap.cons_car();
    let root_cdr = keymap.cons_cdr();
    if root_car.as_symbol_name() != Some("keymap") {
        return;
    }

    // Find the last cons cell in the keymap list
    let mut prev_cell_value = keymap;
    let mut cursor = root_cdr;
    loop {
        if is_list_keymap(&cursor) || cursor.is_nil() {
            prev_cell_value.set_cdr(parent);
            return;
        }
        match cursor.kind() {
            ValueKind::Cons => {
                let entry_cdr = cursor.cons_cdr();
                // If cdr is a keymap (existing parent) or nil, we replace it
                if is_list_keymap(&entry_cdr) || entry_cdr.is_nil() {
                    cursor.set_cdr(parent);
                    return;
                }
                prev_cell_value = cursor;
                cursor = entry_cdr;
            }
            _ => {
                // cursor is either nil or an existing parent keymap
                // Set previous cell's cdr to the new parent
                prev_cell_value.set_cdr(parent);
                return;
            }
        }
    }
}

/// Check whether `target` appears in `keymap`'s parent chain.
pub fn list_keymap_inherits_from(keymap: &Value, target: &Value) -> bool {
    let mut current = *keymap;
    while is_list_keymap(&current) {
        // Use pointer identity (eq), not structural equality (equal),
        // to detect cycles. Two keymaps with the same content are NOT
        // the same keymap.
        if eq_value(&current, target) {
            return true;
        }
        current = list_keymap_parent(&current);
    }
    false
}

pub(crate) fn ensure_global_keymap_in_obarray(obarray: &mut Obarray) -> Value {
    if let Some(val) = obarray.symbol_value("global-map").copied()
        && is_list_keymap(&val)
    {
        return val;
    }
    let keymap = make_list_keymap();
    obarray.set_symbol_value("global-map", keymap);
    keymap
}

fn dynamic_or_global_symbol_value_in_state(
    obarray: &Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    name: &str,
) -> Option<Value> {
    obarray.symbol_value(name).cloned()
}

fn dynamic_buffer_or_global_symbol_value_in_state(
    obarray: &Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    buffer_id: Option<crate::buffer::BufferId>,
    name: &str,
) -> Option<Value> {
    if let Some(buffer_id) = buffer_id
        && let Some(buf) = buffers.get(buffer_id)
        && let Some(value) = buf.get_buffer_local(name)
    {
        return Some(*value);
    }
    obarray.symbol_value(name).cloned()
}

pub(crate) fn minor_mode_map_entry(entry: &Value) -> Option<(String, Value)> {
    if !entry.is_cons() {
        return None;
    };

    let (mode, cdr) = {
        let pair_car = entry.cons_car();
        let pair_cdr = entry.cons_cdr();
        (pair_car, pair_cdr)
    };
    let mode_name = mode.as_symbol_name()?.to_string();
    if cdr == Value::NIL {
        return None;
    }
    Some((mode_name, cdr))
}

pub(crate) fn key_binding_lookup_in_keymap_in_obarray(
    obarray: &Obarray,
    keymap: &Value,
    events: &[Value],
) -> Option<Value> {
    if !is_list_keymap(keymap) || events.is_empty() {
        return None;
    }

    let mut current_map = *keymap;
    for (index, event) in events.iter().enumerate() {
        let binding = list_keymap_lookup_one(&current_map, event);
        if binding.is_nil() {
            return None;
        }
        if index == events.len() - 1 {
            return Some(binding);
        }
        current_map = resolve_prefix_keymap_binding_in_obarray(obarray, &binding)?;
    }

    None
}

fn collect_maps_from_alist_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    buffer_id: Option<crate::buffer::BufferId>,
    alist: &Value,
    skip_if_in: Option<&Value>,
    maps: &mut Vec<Value>,
) {
    let Some(entries) = list_to_vec(alist) else {
        return;
    };
    for entry in entries {
        if !entry.is_cons() {
            continue;
        };
        let (mode_var, keymap_val) = {
            let pair_car = entry.cons_car();
            let pair_cdr = entry.cons_cdr();
            (pair_car, pair_cdr)
        };
        let Some(mode_name) = mode_var.as_symbol_name() else {
            continue;
        };

        if let Some(skip_alist) = skip_if_in
            && assq_in_alist(skip_alist, &mode_var)
        {
            continue;
        }

        let mode_active = dynamic_buffer_or_global_symbol_value_in_state(
            obarray, dynamic, buffers, buffer_id, mode_name,
        )
        .map(|value| value.is_truthy())
        .unwrap_or(false);
        if !mode_active {
            continue;
        }

        if let Some(resolved) = maybe_keymap_in_obarray(obarray, &keymap_val) {
            maps.push(resolved);
        }
    }
}

fn assq_in_alist(alist: &Value, key: &Value) -> bool {
    let Some(entries) = list_to_vec(alist) else {
        return false;
    };

    for entry in entries {
        if !entry.is_cons() {
            continue;
        };
        let pair_car = entry.cons_car();
        let pair_cdr = entry.cons_cdr();
        if pair_car == *key {
            return true;
        }
    }

    false
}

pub(crate) fn collect_minor_mode_maps_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    buffer_id: Option<crate::buffer::BufferId>,
) -> Vec<Value> {
    let mut maps = Vec::new();

    if let Some(emulation_raw) = dynamic_buffer_or_global_symbol_value_in_state(
        obarray,
        dynamic,
        buffers,
        buffer_id,
        "emulation-mode-map-alists",
    ) {
        if let Some(emulation_entries) = list_to_vec(&emulation_raw) {
            for entry in emulation_entries {
                let alist_value = match entry.as_symbol_name() {
                    Some(name) => dynamic_buffer_or_global_symbol_value_in_state(
                        obarray, dynamic, buffers, buffer_id, name,
                    )
                    .unwrap_or(Value::NIL),
                    None => entry,
                };
                collect_maps_from_alist_in_state(
                    obarray,
                    dynamic,
                    buffers,
                    buffer_id,
                    &alist_value,
                    None,
                    &mut maps,
                );
            }
        }
    }

    let overriding = dynamic_buffer_or_global_symbol_value_in_state(
        obarray,
        dynamic,
        buffers,
        buffer_id,
        "minor-mode-overriding-map-alist",
    );
    if let Some(ref overriding_maps) = overriding {
        collect_maps_from_alist_in_state(
            obarray,
            dynamic,
            buffers,
            buffer_id,
            overriding_maps,
            None,
            &mut maps,
        );
    }

    if let Some(regular) = dynamic_buffer_or_global_symbol_value_in_state(
        obarray,
        dynamic,
        buffers,
        buffer_id,
        "minor-mode-map-alist",
    ) {
        collect_maps_from_alist_in_state(
            obarray,
            dynamic,
            buffers,
            buffer_id,
            &regular,
            overriding.as_ref(),
            &mut maps,
        );
    }

    maps
}

#[derive(Clone, Copy, Debug)]
struct ActiveMapPosition {
    buffer_id: crate::buffer::BufferId,
    buffer_object: Value,
    buffer_local_map: Value,
    char_pos: Option<i64>,
}

fn active_map_position(
    frames: &crate::window::FrameManager,
    buffers: &crate::buffer::BufferManager,
    position: Option<&Value>,
) -> Result<Option<ActiveMapPosition>, Flow> {
    let Some(buffer) = buffers.current_buffer() else {
        return Ok(None);
    };

    let default_position = ActiveMapPosition {
        buffer_id: buffer.id,
        buffer_object: Value::make_buffer(buffer.id),
        buffer_local_map: buffer.local_map(),
        char_pos: Some(buffer.point_char() as i64 + 1),
    };

    let Some(position) = position else {
        return Ok(Some(default_position));
    };

    if position.is_window() {
        let window_id = crate::window::WindowId(position.as_window_id().unwrap());
        for frame_id in frames.frame_list() {
            let Some(frame) = frames.get(frame_id) else {
                continue;
            };
            let Some(window) = frame.find_window(window_id) else {
                continue;
            };
            let Some(buffer_id) = window.buffer_id() else {
                break;
            };
            let Some(target_buffer) = buffers.get(buffer_id) else {
                break;
            };

            return Ok(Some(ActiveMapPosition {
                buffer_id,
                buffer_object: Value::make_buffer(buffer_id),
                buffer_local_map: target_buffer.local_map(),
                char_pos: Some(target_buffer.point_char() as i64 + 1),
            }));
        }

        return Ok(Some(default_position));
    }

    if position.is_fixnum() || position.is_char() || position.is_marker() {
        let char_pos = expect_integer_or_marker_in_buffers(buffers, position)?;
        let point_min = buffer.point_min_char() as i64 + 1;
        let point_max = buffer.point_max_char() as i64 + 1;
        if char_pos < point_min || char_pos > point_max {
            return Err(signal(
                "args-out-of-range",
                vec![Value::make_buffer(buffer.id), *position],
            ));
        }

        return Ok(Some(ActiveMapPosition {
            buffer_id: buffer.id,
            buffer_object: Value::make_buffer(buffer.id),
            buffer_local_map: buffer.local_map(),
            char_pos: Some(char_pos),
        }));
    }

    let Some(slots) = list_to_vec(position) else {
        return Ok(Some(default_position));
    };
    if slots.len() < 6 {
        return Ok(Some(default_position));
    }

    let window_id = match slots[0].as_window_id() {
        Some(id) => crate::window::WindowId(id),
        None => return Ok(Some(default_position)),
    };
    let char_pos = slots[5].as_int().or_else(|| slots[1].as_int());

    for frame_id in frames.frame_list() {
        let Some(frame) = frames.get(frame_id) else {
            continue;
        };
        let Some(window) = frame.find_window(window_id) else {
            continue;
        };
        let Some(buffer_id) = window.buffer_id() else {
            continue;
        };
        let Some(target_buffer) = buffers.get(buffer_id) else {
            continue;
        };
        if let Some(char_pos) = char_pos {
            let point_min = target_buffer.point_min_char() as i64 + 1;
            let point_max = target_buffer.point_max_char() as i64 + 1;
            if char_pos < point_min || char_pos > point_max {
                return Err(signal(
                    "args-out-of-range",
                    vec![Value::make_buffer(buffer_id), *position],
                ));
            }
        }

        return Ok(Some(ActiveMapPosition {
            buffer_id,
            buffer_object: Value::make_buffer(buffer_id),
            buffer_local_map: target_buffer.local_map(),
            char_pos,
        }));
    }

    Ok(Some(default_position))
}

fn keymap_property_at_position(
    obarray: &Obarray,
    buffers: &crate::buffer::BufferManager,
    buffer_object: Value,
    char_pos: i64,
    prop_name: &str,
) -> Result<Value, Flow> {
    let prop_symbol = Value::symbol(prop_name);
    let char_property = super::builtins::textprop::builtin_get_char_property_in_state(
        obarray,
        buffers,
        vec![Value::fixnum(char_pos), prop_symbol, buffer_object],
    )?;
    if !char_property.is_nil() {
        return Ok(char_property);
    }

    builtin_get_pos_property_impl(
        obarray,
        &[],
        buffers,
        vec![Value::fixnum(char_pos), prop_symbol, buffer_object],
    )
}

fn current_local_map_for_position(
    obarray: &Obarray,
    frames: &crate::window::FrameManager,
    buffers: &crate::buffer::BufferManager,
    fallback_local_map: Value,
    position: Option<&Value>,
) -> Result<Value, Flow> {
    let Some(active_position) = active_map_position(frames, buffers, position)? else {
        return Ok(fallback_local_map);
    };

    if let Some(char_pos) = active_position.char_pos {
        let property = keymap_property_at_position(
            obarray,
            buffers,
            active_position.buffer_object,
            char_pos,
            "local-map",
        )?;
        return Ok(
            maybe_keymap_in_obarray(obarray, &property).unwrap_or(active_position.buffer_local_map)
        );
    }

    Ok(active_position.buffer_local_map)
}

fn position_keymap(
    obarray: &Obarray,
    frames: &crate::window::FrameManager,
    buffers: &crate::buffer::BufferManager,
    position: Option<&Value>,
) -> Result<Value, Flow> {
    let Some(active_position) = active_map_position(frames, buffers, position)? else {
        return Ok(Value::NIL);
    };

    let Some(char_pos) = active_position.char_pos else {
        return Ok(Value::NIL);
    };

    let property = keymap_property_at_position(
        obarray,
        buffers,
        active_position.buffer_object,
        char_pos,
        "keymap",
    )?;
    Ok(maybe_keymap_in_obarray(obarray, &property).unwrap_or(Value::NIL))
}

fn current_active_maps_from_parts(
    obarray: &Obarray,
    frames: &crate::window::FrameManager,
    buffers: &crate::buffer::BufferManager,
    current_local_map: Value,
    global_map: Value,
    minor_maps: Vec<Value>,
    overriding_local_map: Option<Value>,
    overriding_terminal_local_map: Option<Value>,
    obey_overriding_local_maps: bool,
    position: Option<&Value>,
) -> Result<Vec<Value>, Flow> {
    let active_position = active_map_position(frames, buffers, position)?;
    let current_buffer_id = active_position.map(|pos| pos.buffer_id);

    if obey_overriding_local_maps
        && overriding_terminal_local_map.is_none()
        && let Some(overriding_local_map) = overriding_local_map
    {
        return Ok(vec![overriding_local_map, global_map]);
    }

    let mut maps = Vec::new();

    if obey_overriding_local_maps
        && let Some(overriding_terminal_local_map) = overriding_terminal_local_map
    {
        maps.push(overriding_terminal_local_map);
    }

    let property_keymap = position_keymap(obarray, frames, buffers, position)?;
    if !property_keymap.is_nil() {
        maps.push(property_keymap);
    }

    if minor_maps.is_empty() {
        maps.extend(collect_minor_mode_maps_in_state(
            obarray,
            &[],
            buffers,
            current_buffer_id,
        ));
    } else {
        maps.extend(minor_maps);
    }

    let local_map =
        current_local_map_for_position(obarray, frames, buffers, current_local_map, position)?;
    if !local_map.is_nil() {
        maps.push(local_map);
    }

    maps.push(global_map);
    Ok(maps)
}

pub(crate) fn current_active_maps_for_position(
    ctx: &mut Context,
    obey_overriding_local_maps: bool,
    position: Option<&Value>,
) -> Result<Vec<Value>, Flow> {
    let global_map = ensure_global_keymap_in_obarray(&mut ctx.obarray);
    let overriding_local_map =
        dynamic_or_global_symbol_value_in_state(&ctx.obarray, &[], "overriding-local-map")
            .and_then(|value| maybe_keymap_in_obarray(&ctx.obarray, &value));
    let overriding_terminal_local_map =
        dynamic_or_global_symbol_value_in_state(&ctx.obarray, &[], "overriding-terminal-local-map")
            .and_then(|value| maybe_keymap_in_obarray(&ctx.obarray, &value));

    current_active_maps_from_parts(
        &ctx.obarray,
        &ctx.frames,
        &ctx.buffers,
        ctx.buffers.current_local_map(),
        global_map,
        Vec::new(),
        overriding_local_map,
        overriding_terminal_local_map,
        obey_overriding_local_maps,
        position,
    )
}

pub(crate) fn current_active_maps_for_position_read_only(
    ctx: &Context,
    obey_overriding_local_maps: bool,
    position: Option<&Value>,
) -> Result<Vec<Value>, Flow> {
    let global_map = ctx
        .obarray
        .symbol_value("global-map")
        .copied()
        .filter(is_list_keymap)
        .unwrap_or_else(make_list_keymap);
    let overriding_local_map =
        dynamic_or_global_symbol_value_in_state(&ctx.obarray, &[], "overriding-local-map")
            .and_then(|value| maybe_keymap_in_obarray(&ctx.obarray, &value));
    let overriding_terminal_local_map =
        dynamic_or_global_symbol_value_in_state(&ctx.obarray, &[], "overriding-terminal-local-map")
            .and_then(|value| maybe_keymap_in_obarray(&ctx.obarray, &value));

    current_active_maps_from_parts(
        &ctx.obarray,
        &ctx.frames,
        &ctx.buffers,
        ctx.buffers.current_local_map(),
        global_map,
        Vec::new(),
        overriding_local_map,
        overriding_terminal_local_map,
        obey_overriding_local_maps,
        position,
    )
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActiveKeyBindingResolution {
    pub lookup: Value,
    pub binding: Value,
}

pub(crate) fn is_plain_printable_emacs_event(event: &Value) -> bool {
    let Some(ch) = (match event.kind() {
        ValueKind::Fixnum(code) if (code & KEY_CHAR_MOD_MASK) == 0 => char::from_u32(code as u32),
        _ => None,
    }) else {
        return false;
    };

    !ch.is_control() && ch != '\u{7f}'
}

pub(crate) fn resolve_active_key_binding(
    ctx: &mut Context,
    events: &[Value],
    accept_default: bool,
    no_remap: bool,
    position: Option<&Value>,
) -> Result<ActiveKeyBindingResolution, Flow> {
    let active_maps = current_active_maps_for_position(ctx, true, position)?;
    let lookup =
        lookup_key_in_keymaps_in_obarray(&ctx.obarray, &active_maps, events, accept_default);
    let binding = if !lookup.is_nil() && !lookup.is_fixnum() {
        key_binding_apply_remap_in_active_maps(&active_maps, lookup, no_remap)
    } else if events.len() == 1 && is_plain_printable_emacs_event(&events[0]) {
        Value::symbol("self-insert-command")
    } else {
        Value::NIL
    };

    Ok(ActiveKeyBindingResolution { lookup, binding })
}

fn lookup_minor_mode_binding_in_alist_in_obarray(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buffers: &crate::buffer::BufferManager,
    buffer_id: Option<crate::buffer::BufferId>,
    events: &[Value],
    alist_value: &Value,
) -> Result<Option<(String, Value)>, Flow> {
    let Some(entries) = list_to_vec(alist_value) else {
        return Ok(None);
    };

    for entry in entries {
        let Some((mode_name, map_value)) = minor_mode_map_entry(&entry) else {
            continue;
        };
        if !dynamic_buffer_or_global_symbol_value_in_state(
            obarray, dynamic, buffers, buffer_id, &mode_name,
        )
        .is_some_and(|v| v.is_truthy())
        {
            continue;
        }

        let keymap = if is_list_keymap(&map_value) {
            map_value
        } else if map_value.as_symbol_name().is_some() {
            match map_value
                .as_symbol_name()
                .and_then(|name| obarray.symbol_value(name).copied())
            {
                Some(value) if is_list_keymap(&value) => value,
                _ => match obarray.symbol_function_of_value(&map_value).copied() {
                    Some(value) if is_list_keymap(&value) => value,
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("keymapp"), map_value],
                        ));
                    }
                },
            }
        } else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("keymapp"), map_value],
            ));
        };

        let binding = lookup_keymap_with_partial(&keymap, events);
        if binding.is_nil() {
            continue;
        }

        return Ok(Some((mode_name, binding)));
    }

    Ok(None)
}

pub(crate) fn minor_mode_key_binding_in_context(
    ctx: &Context,
    events: &[Value],
) -> Result<Value, Flow> {
    let current_buffer_id = ctx.buffers.current_buffer_id();
    if let Some(emulation_raw) = dynamic_buffer_or_global_symbol_value_in_state(
        &ctx.obarray,
        &[],
        &ctx.buffers,
        current_buffer_id,
        "emulation-mode-map-alists",
    ) && let Some(emulation_entries) = list_to_vec(&emulation_raw)
    {
        for emulation_entry in emulation_entries {
            let alist_value = match emulation_entry.as_symbol_name() {
                Some(name) => dynamic_buffer_or_global_symbol_value_in_state(
                    &ctx.obarray,
                    &[],
                    &ctx.buffers,
                    current_buffer_id,
                    name,
                )
                .unwrap_or(Value::NIL),
                None => emulation_entry,
            };
            if let Some((mode_name, binding)) = lookup_minor_mode_binding_in_alist_in_obarray(
                &ctx.obarray,
                &[],
                &ctx.buffers,
                current_buffer_id,
                events,
                &alist_value,
            )? {
                return Ok(Value::list(vec![Value::cons(
                    Value::symbol(mode_name),
                    binding,
                )]));
            }
        }
    }

    for alist_name in ["minor-mode-overriding-map-alist", "minor-mode-map-alist"] {
        let Some(alist_value) = dynamic_buffer_or_global_symbol_value_in_state(
            &ctx.obarray,
            &[],
            &ctx.buffers,
            current_buffer_id,
            alist_name,
        ) else {
            continue;
        };
        if let Some((mode_name, binding)) = lookup_minor_mode_binding_in_alist_in_obarray(
            &ctx.obarray,
            &[],
            &ctx.buffers,
            current_buffer_id,
            events,
            &alist_value,
        )? {
            return Ok(Value::list(vec![Value::cons(
                Value::symbol(mode_name),
                binding,
            )]));
        }
    }

    Ok(Value::NIL)
}

fn where_is_expect_keymap_in_obarray(obarray: &Obarray, value: &Value) -> Result<Value, Flow> {
    get_keymap_in_obarray(obarray, value, true)
}

fn where_is_explicit_keymaps_in_context(ctx: &Context, value: &Value) -> Result<Vec<Value>, Flow> {
    if is_list_keymap(value) {
        let keymap = where_is_expect_keymap_in_obarray(&ctx.obarray, value)?;
        let mut keymaps = vec![keymap];
        let global_map = ctx
            .obarray
            .symbol_value("global-map")
            .copied()
            .unwrap_or(Value::NIL);
        if is_list_keymap(&global_map) && global_map != keymap {
            keymaps.push(global_map);
        }
        return Ok(keymaps);
    }

    if let Some(items) = list_to_vec(value) {
        let mut keymaps = Vec::with_capacity(items.len());
        for item in items {
            keymaps.push(where_is_expect_keymap_in_obarray(&ctx.obarray, &item)?);
        }
        return Ok(keymaps);
    }

    let keymap = where_is_expect_keymap_in_obarray(&ctx.obarray, value)?;
    let mut keymaps = vec![keymap];
    let global_map = ctx
        .obarray
        .symbol_value("global-map")
        .copied()
        .unwrap_or(Value::NIL);
    if is_list_keymap(&global_map) && global_map != keymap {
        keymaps.push(global_map);
    }
    Ok(keymaps)
}

pub(crate) fn where_is_keymaps_in_context(
    ctx: &mut Context,
    value: Option<&Value>,
) -> Result<Vec<Value>, Flow> {
    match value {
        Some(keymap_arg) if keymap_arg.is_nil() => {
            Ok(current_active_maps_for_position(ctx, true, None).unwrap_or_default())
        }
        Some(keymap_arg) => where_is_explicit_keymaps_in_context(ctx, keymap_arg),
        None => Ok(current_active_maps_for_position(ctx, true, None).unwrap_or_default()),
    }
}

fn command_remapping_list_tail(value: &Value, n: usize) -> Option<Value> {
    let mut cursor = *value;
    for _ in 0..n {
        match cursor.kind() {
            ValueKind::Cons => {
                cursor = cursor.cons_cdr();
            }
            _ => return None,
        }
    }
    Some(cursor)
}

fn command_remapping_nth_list_element(value: &Value, index: usize) -> Option<Value> {
    let tail = command_remapping_list_tail(value, index)?;
    match tail.kind() {
        ValueKind::Cons => Some(tail.cons_car()),
        _ => None,
    }
}

fn command_remapping_lookup_in_lisp_remap_entry(
    entry: &Value,
    command_name: &str,
) -> Option<Value> {
    if command_remapping_nth_list_element(entry, 0)?.as_symbol_name() != Some("remap") {
        return None;
    }
    if command_remapping_nth_list_element(entry, 1)?.as_symbol_name() != Some("keymap") {
        return None;
    }

    let mut bindings = command_remapping_list_tail(entry, 2)?;
    while bindings.is_cons() {
        let (binding_entry, rest) = {
            let pair_car = bindings.cons_car();
            let pair_cdr = bindings.cons_cdr();
            (pair_car, pair_cdr)
        };
        if binding_entry.is_cons() {
            let (binding_key, binding_target) = {
                let pair_car = binding_entry.cons_car();
                let pair_cdr = binding_entry.cons_cdr();
                (pair_car, pair_cdr)
            };
            if binding_key.as_symbol_name() == Some(command_name) {
                return Some(binding_target);
            }
        }
        bindings = rest;
    }
    None
}

pub(crate) fn command_remapping_lookup_in_lisp_keymap(
    keymap: &Value,
    command_name: &str,
) -> Option<Value> {
    if !is_list_keymap(keymap) {
        return None;
    }

    let mut cursor = if keymap.is_cons() {
        keymap.cons_cdr()
    } else {
        Value::NIL
    };

    while cursor.is_cons() {
        if is_list_keymap(&cursor) {
            if let Some(parent) = command_remapping_lookup_in_lisp_keymap(&cursor, command_name) {
                return Some(parent);
            }
            break;
        }

        let car = cursor.cons_car();
        let cdr = cursor.cons_cdr();
        if let Some(remap) = command_remapping_lookup_in_lisp_remap_entry(&car, command_name) {
            return Some(remap);
        }
        cursor = cdr;
    }

    None
}

fn command_remapping_menu_item_target(value: &Value) -> Option<Value> {
    if !value.is_cons() {
        return None;
    };
    let pair_car = value.cons_car();
    let pair_cdr = value.cons_cdr();
    if pair_car.as_symbol_name() != Some("menu-item") {
        return None;
    }

    let tail = pair_cdr;
    let title = command_remapping_nth_list_element(&tail, 0)?;
    if title.is_nil() {
        return None;
    }
    command_remapping_nth_list_element(&tail, 1)
}

pub(crate) fn command_remapping_normalize_target(raw: Value) -> Value {
    if let Some(menu_target) = command_remapping_menu_item_target(&raw) {
        return if menu_target.is_integer() {
            Value::NIL
        } else {
            menu_target
        };
    }
    if raw == Value::T || raw.is_fixnum() {
        return Value::NIL;
    }
    raw
}

fn command_remapping_lookup_in_keymap_value(keymap: &Value, command_name: &str) -> Option<Value> {
    command_remapping_lookup_in_lisp_keymap(keymap, command_name)
        .map(command_remapping_normalize_target)
}

pub(crate) fn command_remapping_lookup_in_keymaps(
    keymaps: &[Value],
    command_name: &str,
) -> Option<Value> {
    for keymap in keymaps {
        if !is_list_keymap(keymap) {
            continue;
        }
        if let Some(value) = command_remapping_lookup_in_keymap_value(keymap, command_name) {
            return Some(value);
        }
    }
    None
}

pub(crate) fn command_remapping_command_name(command: &Value) -> Option<String> {
    if command.is_nil() {
        Some("nil".to_string())
    } else if *command == Value::T {
        Some("t".to_string())
    } else if let Some(name) = command.as_symbol_name() {
        Some(name.to_owned())
    } else {
        None
    }
}

pub(crate) fn key_binding_apply_remap_in_active_maps(
    active_maps: &[Value],
    binding: Value,
    no_remap: bool,
) -> Value {
    if no_remap {
        return binding;
    }
    let Some(command_name) = binding.as_symbol_name().map(ToString::to_string) else {
        return binding;
    };
    match command_remapping_lookup_in_keymaps(active_maps, &command_name) {
        Some(remapped) if !remapped.is_nil() => remapped,
        _ => binding,
    }
}

/// Convert a `KeyEvent` to an Emacs event value (integer with modifier bits, or symbol).
///
/// For Ctrl + ASCII letter, produce the control character code (1-26)
/// instead of using the CTRL modifier bit.  This matches GNU Emacs
/// `MAKE_CTRL_CHAR` normalization: C-a=1, C-b=2, ..., C-z=26,
/// C-@=0, C-[=27, C-\=28, C-]=29, C-^=30, C-_=31.
pub fn key_event_to_emacs_event(event: &KeyEvent) -> Value {
    match event {
        KeyEvent::Char {
            code,
            ctrl,
            meta,
            shift,
            super_,
            hyper,
            alt,
        } => {
            let mut bits: i64;
            if *ctrl {
                let c = *code as u32;
                // GNU Emacs MAKE_CTRL_CHAR normalization: for characters
                // that have a natural control character, fold into 0-31
                // without the CTRL modifier bit.
                let ctrl_char = match c {
                    // a-z → 1-26
                    0x61..=0x7A => Some(c - 0x60),
                    // A-Z → 1-26  (same as lowercase)
                    0x41..=0x5A => Some(c - 0x40),
                    // @ → 0 (NUL)
                    0x40 => Some(0),
                    // [ → 27 (ESC)
                    0x5B => Some(27),
                    // \ → 28
                    0x5C => Some(28),
                    // ] → 29
                    0x5D => Some(29),
                    // ^ → 30
                    0x5E => Some(30),
                    // _ → 31
                    0x5F => Some(31),
                    // Space/? → 0 (NUL) — Emacs convention
                    0x20 => Some(0),
                    _ => None,
                };
                if let Some(cc) = ctrl_char {
                    bits = cc as i64;
                } else {
                    bits = *code as i64;
                    bits |= KEY_CHAR_CTRL;
                }
            } else {
                bits = *code as i64;
            }
            if *meta {
                bits |= KEY_CHAR_META;
            }
            if *shift {
                bits |= KEY_CHAR_SHIFT;
            }
            if *super_ {
                bits |= KEY_CHAR_SUPER;
            }
            if *hyper {
                bits |= KEY_CHAR_HYPER;
            }
            if *alt {
                bits |= KEY_CHAR_ALT;
            }
            Value::fixnum(bits)
        }
        KeyEvent::Function {
            name,
            ctrl,
            meta,
            shift,
            super_,
            hyper,
            alt,
        } => {
            if let Some(base) = match name.as_str() {
                "return" => Some('\r' as i64),
                "tab" => Some('\t' as i64),
                _ => None,
            } {
                let mut bits = base;
                if *ctrl {
                    bits |= KEY_CHAR_CTRL;
                }
                if *meta {
                    bits |= KEY_CHAR_META;
                }
                if *shift {
                    bits |= KEY_CHAR_SHIFT;
                }
                if *super_ {
                    bits |= KEY_CHAR_SUPER;
                }
                if *hyper {
                    bits |= KEY_CHAR_HYPER;
                }
                if *alt {
                    bits |= KEY_CHAR_ALT;
                }
                return Value::fixnum(bits);
            }
            let mut prefix = String::new();
            if *alt {
                prefix.push_str("A-");
            }
            if *ctrl {
                prefix.push_str("C-");
            }
            if *hyper {
                prefix.push_str("H-");
            }
            if *meta {
                prefix.push_str("M-");
            }
            if *shift {
                prefix.push_str("S-");
            }
            if *super_ {
                prefix.push_str("s-");
            }
            Value::symbol(format!("{}{}", prefix, name))
        }
    }
}

/// Convert an Emacs event value to a `KeyEvent`.
///
/// Recognizes control characters (0-31) and decomposes them into
/// the corresponding letter with ctrl=true.
pub fn emacs_event_to_key_event(event: &Value) -> Option<KeyEvent> {
    match event.kind() {
        ValueKind::Fixnum(code) => {
            let base = code & KEY_CHAR_CODE_MASK;
            let has_ctrl_bit = (code & KEY_CHAR_CTRL) != 0;
            let meta = (code & KEY_CHAR_META) != 0;
            let shift = (code & KEY_CHAR_SHIFT) != 0;
            let super_ = (code & KEY_CHAR_SUPER) != 0;
            let hyper = (code & KEY_CHAR_HYPER) != 0;
            let alt = (code & KEY_CHAR_ALT) != 0;

            // Decompose control characters (0-31) back to letter + ctrl
            if !has_ctrl_bit && (0..=31).contains(&base) {
                let (ch, ctrl) = match base {
                    0 => ('@', true), // NUL → C-@
                    1..=26 => {
                        // 1-26 → C-a through C-z
                        let c = char::from_u32((base + 0x60) as u32)?;
                        (c, true)
                    }
                    27 => ('\u{1b}', false), // ESC → literal escape prefix char
                    28 => ('\\', true),      // C-\
                    29 => (']', true),       // C-]
                    30 => ('^', true),       // C-^
                    31 => ('_', true),       // C-_
                    _ => unreachable!(),
                };
                Some(KeyEvent::Char {
                    code: ch,
                    ctrl,
                    meta,
                    shift,
                    super_,
                    hyper,
                    alt,
                })
            } else {
                let ch = char::from_u32(base as u32)?;
                Some(KeyEvent::Char {
                    code: ch,
                    ctrl: has_ctrl_bit,
                    meta,
                    shift,
                    super_,
                    hyper,
                    alt,
                })
            }
        }
        ValueKind::Symbol(id) => {
            let name = resolve_sym(id);
            // Parse modifier prefixes
            let mut rest = name;
            let mut ctrl = false;
            let mut meta = false;
            let mut shift = false;
            let mut super_ = false;
            let mut hyper = false;
            let mut alt = false;
            loop {
                if let Some(r) = rest.strip_prefix("C-") {
                    ctrl = true;
                    rest = r;
                    continue;
                }
                if let Some(r) = rest.strip_prefix("M-") {
                    meta = true;
                    rest = r;
                    continue;
                }
                if let Some(r) = rest.strip_prefix("S-") {
                    shift = true;
                    rest = r;
                    continue;
                }
                if let Some(r) = rest.strip_prefix("s-") {
                    super_ = true;
                    rest = r;
                    continue;
                }
                if let Some(r) = rest.strip_prefix("H-") {
                    hyper = true;
                    rest = r;
                    continue;
                }
                if let Some(r) = rest.strip_prefix("A-") {
                    alt = true;
                    rest = r;
                    continue;
                }
                break;
            }
            // If single char, return Char event
            let mut chars = rest.chars();
            if let Some(ch) = chars.next() {
                if chars.next().is_none() {
                    return Some(KeyEvent::Char {
                        code: ch,
                        ctrl,
                        meta,
                        shift,
                        super_,
                        hyper,
                        alt,
                    });
                }
            }
            // Otherwise it's a function key
            Some(KeyEvent::Function {
                name: rest.to_string(),
                ctrl,
                meta,
                shift,
                super_,
                hyper,
                alt,
            })
        }
        _ => None,
    }
}

/// Look up a key sequence in a keymap, following prefix keymaps and parent chains.
/// Returns the binding Value, or the number of keys matched (as `Value::Int`)
/// when the sequence resolves through a non-keymap binding.
pub fn list_keymap_lookup_seq(keymap: &Value, events: &[Value]) -> Value {
    if events.is_empty() {
        return *keymap;
    }

    let mut current_map = *keymap;
    for (i, event) in events.iter().enumerate() {
        let binding = list_keymap_lookup_one(&current_map, event);
        let is_last = i == events.len() - 1;
        if is_last {
            // GNU: for the last key, return binding directly (even nil)
            return binding;
        }
        if binding.is_nil() {
            // No binding for a non-last event → return the number of keys
            // consumed (matching GNU which returns make_fixnum(idx) where
            // idx is already post-incremented).
            return Value::fixnum((i + 1) as i64);
        }
        // Must be a prefix keymap to continue
        if is_list_keymap(&binding) {
            current_map = binding;
        } else {
            // Check if it's a symbol whose function cell is a keymap
            if let Some(sym_name) = binding.as_symbol_name() {
                // We can't resolve symbol function cells from keymap.rs —
                // caller must handle this case. For now treat as non-prefix.
                let _ = sym_name;
            }
            return Value::fixnum((i + 1) as i64);
        }
    }
    Value::NIL
}

pub(crate) fn lookup_keymap_with_partial(keymap: &Value, emacs_events: &[Value]) -> Value {
    if emacs_events.is_empty() {
        return *keymap;
    }
    list_keymap_lookup_seq(keymap, emacs_events)
}

/// Define a key in a keymap, auto-creating prefix maps for multi-key sequences.
///
/// Returns `Err` if an intermediate key is already bound to a non-prefix
/// command (matching GNU Emacs behavior which signals an error).
pub fn list_keymap_define_seq(keymap: Value, events: &[Value], def: Value) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }
    if events.len() == 1 {
        list_keymap_define(keymap, events[0], def);
        return Ok(());
    }

    let mut current_map = keymap;
    for (i, event) in events.iter().enumerate() {
        if i == events.len() - 1 {
            list_keymap_define(current_map, *event, def);
            return Ok(());
        }
        // Use noinherit: only look in current keymap level for prefix,
        // matching GNU Emacs define-key which uses access_keymap(noinherit=1)
        let binding = list_keymap_lookup_one_noinherit(&current_map, event);
        if is_list_keymap(&binding) {
            current_map = binding;
        } else if binding.is_nil() {
            // No binding at this level, create a new prefix keymap
            let prefix_map = make_sparse_list_keymap();
            list_keymap_define(current_map, *event, prefix_map);
            current_map = prefix_map;
        } else {
            // Non-prefix binding found — error (matching GNU Emacs)
            return Err(format!("Key sequence starts with non-prefix key"));
        }
    }
    Ok(())
}

/// Define a key in a keymap, resolving symbol prefix bindings through the
/// obarray before auto-creating nested prefix maps.
///
/// Uses noinherit lookup for prefix keys, matching GNU Emacs `Fdefine_key`
/// which calls `access_keymap(noinherit=1)`.
///
/// Returns `Err` with a descriptive message if an intermediate key is already
/// bound to a non-prefix command (matching GNU Emacs behavior which signals
/// an error like "Key sequence <f1> a starts with non-prefix key <f1>").
pub fn list_keymap_define_seq_in_obarray(
    obarray: &Obarray,
    keymap: Value,
    events: &[Value],
    def: Value,
) -> Result<(), String> {
    list_keymap_define_seq_in_obarray_ex(obarray, keymap, events, def, false)
}

/// Extended version of define-seq that supports the REMOVE flag.
pub fn list_keymap_define_seq_in_obarray_ex(
    obarray: &Obarray,
    keymap: Value,
    events: &[Value],
    def: Value,
    remove: bool,
) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }
    if events.len() == 1 {
        if remove {
            list_keymap_remove(keymap, events[0]);
        } else {
            list_keymap_define(keymap, events[0], def);
        }
        return Ok(());
    }

    let mut current_map = keymap;
    for (i, event) in events.iter().enumerate() {
        if i == events.len() - 1 {
            if remove {
                list_keymap_remove(current_map, *event);
            } else {
                list_keymap_define(current_map, *event, def);
            }
            return Ok(());
        }
        // Use noinherit: only look in current keymap level for prefix,
        // matching GNU Emacs define-key which uses access_keymap(noinherit=1)
        let binding = list_keymap_lookup_one_noinherit(&current_map, event);
        if let Some(prefix_map) = resolve_prefix_keymap_binding_in_obarray(obarray, &binding) {
            current_map = prefix_map;
        } else if binding.is_nil() {
            // No binding, create a new prefix keymap.
            // Matches GNU `define_as_prefix` (keymap.c:1446-1452).
            let prefix_map = make_sparse_list_keymap();
            list_keymap_define(current_map, *event, prefix_map);
            current_map = prefix_map;
        } else {
            // Non-prefix binding found. GNU Emacs `define_as_prefix` creates
            // a new prefix keymap even when a non-keymap binding exists at
            // this intermediate position. Match that behavior (gap #10).
            let prefix_map = make_sparse_list_keymap();
            list_keymap_define(current_map, *event, prefix_map);
            current_map = prefix_map;
        }
    }
    Ok(())
}

/// Generate a human-readable description of an event sequence for error messages.
/// Uses the same format as GNU Emacs `key-description`: function keys use
/// angle brackets (e.g., `<f1>`), characters use their standard description.
fn describe_event_sequence(events: &[Value]) -> String {
    use super::keyboard::pure::describe_single_key_value;
    events
        .iter()
        .map(|e| {
            describe_single_key_value(e, false).unwrap_or_else(|_| {
                if let Some(name) = e.as_symbol_name() {
                    format!("<{}>", name)
                } else {
                    format!("{:?}", e)
                }
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Deep-copy a keymap cons-list structure.
///
/// Mirrors GNU Emacs `copy_keymap_1`:
/// - Copies the cons-list structure
/// - Deep-copies char-tables (via vector clone + recursive entry copy)
/// - Recursively copies sub-keymaps (prefix key maps)
/// - Copies alist bindings whose values are keymaps
/// - Preserves parent keymap as shared (not recursively copied)
pub fn list_keymap_copy(keymap: &Value) -> Value {
    list_keymap_copy_impl(keymap, 0)
}

fn list_keymap_copy_impl(keymap: &Value, depth: usize) -> Value {
    if depth > 100 {
        tracing::warn!("list_keymap_copy: recursion depth limit, possible infinite loop");
        return *keymap;
    }

    if !keymap.is_cons() {
        return *keymap;
    };
    let pair_car = keymap.cons_car();
    let pair_cdr = keymap.cons_cdr();
    if pair_car.as_symbol_name() != Some("keymap") {
        return *keymap;
    }

    let mut elements = vec![Value::symbol("keymap")];
    let mut cursor = pair_cdr;
    let mut tail_parent = Value::NIL;

    while cursor.is_cons() {
        if is_list_keymap(&cursor) {
            // Parent keymap: keep shared (don't recursively copy parent chain)
            tail_parent = cursor;
            break;
        }
        let entry_car = cursor.cons_car();
        let entry_cdr = cursor.cons_cdr();

        if is_char_table(&entry_car) {
            // Deep-copy char-table: clone the vector, then recursively copy
            // any keymap entries within it.
            elements.push(copy_char_table_for_keymap(&entry_car, depth));
        } else if is_list_keymap(&entry_car) {
            // Nested keymap element — recursively copy
            elements.push(list_keymap_copy_impl(&entry_car, depth + 1));
        } else if entry_car.is_cons() {
            // Alist entry (EVENT . DEF) — copy the cons, recurse if DEF is a keymap
            let binding_car = entry_car.cons_car();
            let binding_cdr = entry_car.cons_cdr();
            let copied_def = copy_keymap_item(&binding_cdr, depth);
            elements.push(Value::cons(binding_car, copied_def));
        } else {
            elements.push(entry_car);
        }

        cursor = entry_cdr;
    }

    // Build the new list
    let mut result = tail_parent;
    for elem in elements.into_iter().rev() {
        result = Value::cons(elem, result);
    }
    result
}

/// Copy a keymap item (the DEF part of an alist entry).
/// If it's a keymap, recursively copy it. Otherwise return as-is.
/// Mirrors GNU `copy_keymap_item`.
fn copy_keymap_item(item: &Value, depth: usize) -> Value {
    if is_list_keymap(item) {
        return list_keymap_copy_impl(item, depth + 1);
    }
    // Handle menu items etc. — for now, just return as-is for non-keymaps
    *item
}

/// Deep-copy a char-table used in a keymap.
/// Clones the underlying vector and recursively copies any keymap entries.
fn copy_char_table_for_keymap(ct: &Value, depth: usize) -> Value {
    if !ct.is_vector() {
        return *ct;
    };
    let old_vec = ct.as_vector_data().unwrap();
    let mut new_vec = old_vec.clone();

    // Walk the data pairs and recursively copy any keymap values.
    // Char-table layout: [tag, default, parent, subtype, extra_count, ...extras..., ...data-pairs...]
    // Data pairs start after extra slots (CT_EXTRA_START + n_extras),
    // stored as consecutive (char-code, value) pairs.
    let ct_extra_start = 5; // matches chartable.rs CT_EXTRA_START
    let n_extras = new_vec.get(4).and_then(|v| v.as_fixnum()).unwrap_or(0) as usize;
    let data_start = ct_extra_start + n_extras;
    let mut i = data_start;
    while i + 1 < new_vec.len() {
        // i is the char-code, i+1 is the value
        let val = new_vec[i + 1];
        if is_list_keymap(&val) {
            new_vec[i + 1] = list_keymap_copy_impl(&val, depth + 1);
        }
        i += 2;
    }

    Value::vector(new_vec)
}

/// Collect all accessible sub-keymaps with their key prefixes.
pub fn list_keymap_accessible(
    keymap: &Value,
    prefix: &mut Vec<Value>,
    out: &mut Vec<Value>,
    seen: &mut Vec<Value>,
) {
    // Detect cycles: check if we've seen this exact keymap object
    for s in seen.iter() {
        if keymap_value_eq(s, keymap) {
            return;
        }
    }
    seen.push(*keymap);

    // Add current keymap
    out.push(Value::cons(Value::vector(prefix.clone()), *keymap));

    if !keymap.is_cons() {
        return;
    };
    let pair_car = keymap.cons_car();
    let pair_cdr = keymap.cons_cdr();
    if pair_car.as_symbol_name() != Some("keymap") {
        return;
    }

    // Scan alist entries for prefix keymaps
    let mut cursor = pair_cdr;
    while cursor.is_cons() {
        if is_list_keymap(&cursor) {
            break;
        }
        let entry_car = cursor.cons_car();
        let entry_cdr = cursor.cons_cdr();

        if entry_car.is_cons() {
            let binding_car = entry_car.cons_car();
            let binding_cdr = entry_car.cons_cdr();
            if is_list_keymap(&binding_cdr) {
                prefix.push(binding_car);
                list_keymap_accessible(&binding_cdr, prefix, out, seen);
                prefix.pop();
            }
        }

        if is_list_keymap(&entry_cdr) {
            break; // parent keymap, don't descend
        }
        cursor = entry_cdr;
    }

    seen.pop();
}

/// Check if two keymap values are the same object (by cons cell identity).
fn keymap_value_eq(a: &Value, b: &Value) -> bool {
    match (a.kind(), b.kind()) {
        (ValueKind::Cons, ValueKind::Cons) => *a == *b,
        _ => false,
    }
}

/// Iterate over all bindings in a keymap (not following parent).
/// Calls `f(event, def)` for each binding.
pub fn list_keymap_for_each_binding<F>(keymap: &Value, mut f: F)
where
    F: FnMut(Value, Value),
{
    if !keymap.is_cons() {
        return;
    };
    let pair_car = keymap.cons_car();
    let pair_cdr = keymap.cons_cdr();
    if pair_car.as_symbol_name() != Some("keymap") {
        return;
    }

    let mut cursor = pair_cdr;
    while cursor.is_cons() {
        if is_list_keymap(&cursor) {
            break;
        }
        let entry_car = cursor.cons_car();
        let entry_cdr = cursor.cons_cdr();

        if super::chartable::is_char_table(&entry_car) {
            super::chartable::for_each_non_nil_char_table_run(&entry_car, &mut f);
        }

        if entry_car.is_cons() {
            let binding_car = entry_car.cons_car();
            let binding_cdr = entry_car.cons_cdr();
            f(binding_car, binding_cdr);
        }

        if is_list_keymap(&entry_cdr) {
            break;
        }
        cursor = entry_cdr;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "keymap_test.rs"]
mod tests;
