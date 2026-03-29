//! Keymap system — key binding lookup and command dispatch.
//!
//! Provides an Emacs-compatible keymap system with:
//! - Sparse and full keymaps
//! - Parent (inheritance) chain lookup
//! - Key description parsing (`kbd` style: "C-x C-f", "M-x", "RET", etc.)
//! - Global and local (buffer) keymap support

use super::chartable::{
    builtin_char_table_range, builtin_set_char_table_range, is_char_table, make_char_table_value,
};
use super::intern::resolve_sym;
use super::keyboard::pure::{
    KEY_CHAR_ALT, KEY_CHAR_CODE_MASK, KEY_CHAR_CTRL, KEY_CHAR_HYPER, KEY_CHAR_META,
    KEY_CHAR_MOD_MASK, KEY_CHAR_SHIFT, KEY_CHAR_SUPER,
};
use super::symbol::Obarray;
use super::value::{Value, read_cons, with_heap, with_heap_mut};

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
                        shift: false,
                        super_: ke.modifiers.super_,
                        hyper: false,
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
    let char_table = make_char_table_value(Value::Nil, Value::Nil);
    Value::list(vec![Value::symbol("keymap"), char_table])
}

/// Create a sparse list keymap: `(keymap)` — a single-element list.
pub fn make_sparse_list_keymap() -> Value {
    Value::list(vec![Value::symbol("keymap")])
}

/// Check if a value is a keymap: `(consp x) && (car x) == 'keymap`.
pub fn is_list_keymap(v: &Value) -> bool {
    match v {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            pair.car.as_symbol_name() == Some("keymap")
        }
        _ => false,
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
        let Value::Cons(cell) = obj else {
            return obj;
        };
        let pair = read_cons(cell);
        if pair.car.is_string() {
            // (STRING . REST) — strip the menu label
            obj = pair.cdr;
            // Also strip a second string (help string)
            if let Value::Cons(c2) = obj {
                let p2 = read_cons(c2);
                if p2.car.is_string() {
                    obj = p2.cdr;
                }
            }
            continue;
        }
        if pair.car.is_symbol_named("menu-item") {
            // (menu-item NAME DEFN . PROPS) — extract DEFN (third element)
            if let Value::Cons(c1) = pair.cdr {
                let p1 = read_cons(c1); // NAME
                if let Value::Cons(c2) = p1.cdr {
                    let p2 = read_cons(c2); // DEFN
                    return p2.car;
                }
            }
            return Value::Nil;
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
/// Returns the binding or `Value::Nil` if not found.
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
    let Value::Cons(cell) = keymap else {
        return None;
    };
    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("keymap") {
        return None;
    }

    let mut cursor = pair.cdr;
    let mut entries = 0;
    let mut t_binding: Option<Value> = None;
    while let Value::Cons(entry_cell) = cursor {
        if is_list_keymap(&cursor) {
            break; // parent boundary
        }
        entries += 1;
        if entries > 100_000 {
            break;
        }
        let entry = read_cons(entry_cell);

        // Char-table: only look up characters WITHOUT modifier bits.
        // GNU keymap.c:441-450: nil in char-table means unbound;
        // Qt means explicitly nil binding.
        if is_char_table(&entry.car) {
            if let Value::Int(code) = event {
                if (*code & KEY_CHAR_MOD_MASK) == 0 {
                    let base = *code & KEY_CHAR_CODE_MASK;
                    if base >= 0 && base <= 0x3FFFFF {
                        let result =
                            builtin_char_table_range(vec![entry.car, *event]).unwrap_or(Value::Nil);
                        if !result.is_nil() {
                            // Qt in char-table means explicitly nil binding
                            // (shadows parent), matching GNU keymap.c:455-459
                            let val = if result == Value::True {
                                Value::Nil
                            } else {
                                result
                            };
                            return Some(get_keyelt(val));
                        }
                        // nil in char-table means unbound — fall through
                    }
                }
            }
            cursor = entry.cdr;
            continue;
        }

        // Vector element in keymap spine: maps char codes 0..len to
        // bindings by index. Matches GNU keymap.c:431-434.
        if let Value::Vector(vec_id) = entry.car {
            if let Value::Int(code) = *event {
                if code >= 0 {
                    let idx = code as usize;
                    let items = with_heap(|h| h.get_vector(vec_id).clone());
                    if idx < items.len() {
                        let val = items[idx];
                        if !val.is_nil() {
                            return Some(get_keyelt(val));
                        }
                    }
                }
            }
            cursor = entry.cdr;
            continue;
        }

        // Alist entry: (EVENT . DEF)
        if let Value::Cons(binding_cell) = entry.car {
            let binding = read_cons(binding_cell);
            if events_match(&binding.car, event) {
                return Some(get_keyelt(binding.cdr));
            }
            // Check for (t . COMMAND) default binding.
            // GNU keymap.c:425-429: when t_ok, record the first t binding
            // but keep scanning for a specific match.
            if t_ok && t_binding.is_none() && binding.car == Value::True {
                t_binding = Some(get_keyelt(binding.cdr));
            }
        }

        cursor = entry.cdr;
    }

    // If no specific binding found but we have a t default binding, use it.
    // Matches GNU keymap.c:486-487.
    t_binding
}

/// Get the parent keymap from a keymap (the tail after all alist entries).
fn get_keymap_tail_parent(keymap: &Value) -> Value {
    let Value::Cons(cell) = keymap else {
        return Value::Nil;
    };
    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("keymap") {
        return Value::Nil;
    }
    let mut cursor = pair.cdr;
    while let Value::Cons(entry_cell) = cursor {
        if is_list_keymap(&cursor) {
            return cursor;
        }
        let entry = read_cons(entry_cell);
        cursor = entry.cdr;
    }
    Value::Nil
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
            return Value::Nil;
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
                    return Value::Nil;
                }
                let parent = get_keymap_tail_parent(&current);
                if parent.is_nil() {
                    return Value::Nil;
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
    let Value::Cons(cell) = child else {
        return *parent;
    };
    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("keymap") {
        return *parent;
    }

    // Collect child's own entries (excluding its existing parent)
    let mut elements = vec![Value::symbol("keymap")];
    let mut cursor = pair.cdr;
    while let Value::Cons(entry_cell) = cursor {
        if is_list_keymap(&cursor) {
            // Don't include child's existing parent; we'll set a new one
            break;
        }
        let entry = read_cons(entry_cell);
        elements.push(entry.car);
        cursor = entry.cdr;
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
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::Int(x), Value::Char(y)) => *x == *y as i64,
        (Value::Char(x), Value::Int(y)) => *x as i64 == *y,
        (Value::Symbol(x), Value::Symbol(y)) => x == y,
        _ => false,
    }
}

pub(crate) fn expand_meta_prefix_char_events_in_obarray(
    obarray: &Obarray,
    events: &[Value],
) -> Option<Vec<Value>> {
    let meta_prefix = match obarray.symbol_value("meta-prefix-char").copied() {
        Some(Value::Int(code)) => code,
        _ => return None,
    };

    let mut changed = false;
    let mut expanded = Vec::with_capacity(events.len() + 1);
    for event in events {
        match event {
            Value::Int(code) if (*code & KEY_CHAR_META) != 0 => {
                changed = true;
                expanded.push(Value::Int(meta_prefix));
                expanded.push(Value::Int(*code & !KEY_CHAR_META));
            }
            _ => expanded.push(*event),
        }
    }

    changed.then_some(expanded)
}

fn resolve_prefix_keymap_binding_in_obarray(obarray: &Obarray, binding: &Value) -> Option<Value> {
    if is_list_keymap(binding) {
        return Some(*binding);
    }
    // Use symbol_function_of_value to handle uninterned symbols correctly.
    binding.as_symbol_name()?; // verify it's a symbol
    obarray
        .symbol_function_of_value(binding)
        .copied()
        .filter(is_list_keymap)
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
    store_in_keymap(keymap, event, Value::Nil, true);
}

/// Core store/remove implementation matching GNU `store_in_keymap`.
fn store_in_keymap(keymap: Value, event: Value, def: Value, remove: bool) {
    let Value::Cons(root_cell) = keymap else {
        return;
    };
    let root = read_cons(root_cell);
    if root.car.as_symbol_name() != Some("keymap") {
        return;
    }

    // Scan the keymap for existing bindings, tracking insertion point.
    // GNU keymap.c: insertion_point starts at keymap; if a char-table or
    // vector is found, insertion_point is updated to point after it.
    let mut insertion_point = Value::Cons(root_cell);
    let mut cursor = root.cdr;
    while let Value::Cons(entry_cell) = cursor {
        if is_list_keymap(&cursor) {
            // Hit a parent keymap boundary — stop scanning
            break;
        }
        let entry = read_cons(entry_cell);

        // Char-table: handle plain character events (no modifier bits).
        // GNU keymap.c:805-829
        if is_char_table(&entry.car) {
            if let Value::Int(code) = event {
                let mods = code & KEY_CHAR_MOD_MASK;
                if mods == 0 {
                    let base = code & KEY_CHAR_CODE_MASK;
                    if base >= 0 && base <= 0x3FFFFF {
                        let store_val = if remove {
                            Value::Nil
                        } else if def.is_nil() {
                            // nil has special meaning for char-tables (unbound),
                            // so use Qt (Value::True) for explicitly nil binding.
                            // GNU keymap.c:813-814
                            Value::True
                        } else {
                            def
                        };
                        let _ = builtin_set_char_table_range(vec![entry.car, event, store_val]);
                        return;
                    }
                }
            }
            insertion_point = Value::Cons(entry_cell);
            cursor = entry.cdr;
            continue;
        }

        // Vector element: check for matching index.
        // GNU keymap.c:783-803
        if let Value::Vector(vec_id) = entry.car {
            if let Value::Int(code) = event {
                let idx = code as usize;
                let len = with_heap(|h| h.get_vector(vec_id).len());
                if idx < len {
                    with_heap_mut(|h| {
                        h.get_vector_mut(vec_id)[idx] = def;
                    });
                    return;
                }
            }
            insertion_point = Value::Cons(entry_cell);
            cursor = entry.cdr;
            continue;
        }

        // Alist entry: (EVENT . DEF) — check for existing binding to update in-place.
        // GNU keymap.c:842-849
        if let Value::Cons(binding_cell) = entry.car {
            let binding = read_cons(binding_cell);
            if events_match(&binding.car, &event) {
                if remove {
                    // Remove the entry: splice it out of the list.
                    // Set insertion_point's cdr to skip this entry.
                    insertion_point.set_cdr(entry.cdr);
                } else {
                    // Update in-place: set the cdr of the binding cons.
                    Value::Cons(binding_cell).set_cdr(def);
                }
                return;
            }
        }

        // Check for 'keymap symbol in spine (start of inherited keymap)
        // GNU keymap.c:871-876
        if entry.car.is_symbol_named("keymap") {
            break;
        }

        insertion_point = Value::Cons(entry_cell);
        cursor = entry.cdr;
    }

    // No existing binding found. Append new entry after insertion_point.
    if !remove {
        let binding = Value::cons(event, def);
        let old_cdr = match insertion_point {
            Value::Cons(cell) => read_cons(cell).cdr,
            _ => Value::Nil,
        };
        let new_cdr = Value::cons(binding, old_cdr);
        insertion_point.set_cdr(new_cdr);
    }
}

/// Get the parent keymap (last CDR that is itself a keymap).
pub fn list_keymap_parent(keymap: &Value) -> Value {
    let Value::Cons(cell) = keymap else {
        return Value::Nil;
    };
    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("keymap") {
        return Value::Nil;
    }

    let mut cursor = pair.cdr;
    while let Value::Cons(entry_cell) = cursor {
        // Check if cursor itself is a parent keymap before treating as alist entry
        if is_list_keymap(&cursor) {
            return cursor;
        }
        let entry = read_cons(entry_cell);
        if entry.cdr.is_nil() {
            return Value::Nil;
        }
        cursor = entry.cdr;
    }
    Value::Nil
}

/// Set the parent keymap: walk to the last alist cons cell, set its CDR.
pub fn list_keymap_set_parent(keymap: Value, parent: Value) {
    let Value::Cons(root_cell) = keymap else {
        return;
    };
    let root = read_cons(root_cell);
    if root.car.as_symbol_name() != Some("keymap") {
        return;
    }

    // Find the last cons cell in the keymap list
    let mut prev_cell_value = Value::Cons(root_cell);
    let mut cursor = root.cdr;
    loop {
        if is_list_keymap(&cursor) || cursor.is_nil() {
            prev_cell_value.set_cdr(parent);
            return;
        }
        match cursor {
            Value::Cons(cell) => {
                let entry = read_cons(cell);
                // If cdr is a keymap (existing parent) or nil, we replace it
                if is_list_keymap(&entry.cdr) || entry.cdr.is_nil() {
                    Value::Cons(cell).set_cdr(parent);
                    return;
                }
                prev_cell_value = Value::Cons(cell);
                cursor = entry.cdr;
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
            Value::Int(bits)
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
                return Value::Int(bits);
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
    match event {
        Value::Int(code) => {
            let base = *code & KEY_CHAR_CODE_MASK;
            let has_ctrl_bit = (*code & KEY_CHAR_CTRL) != 0;
            let meta = (*code & KEY_CHAR_META) != 0;
            let shift = (*code & KEY_CHAR_SHIFT) != 0;
            let super_ = (*code & KEY_CHAR_SUPER) != 0;
            let hyper = (*code & KEY_CHAR_HYPER) != 0;
            let alt = (*code & KEY_CHAR_ALT) != 0;

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
        Value::Char(c) => Some(KeyEvent::Char {
            code: *c,
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }),
        Value::Symbol(id) => {
            let name = resolve_sym(*id);
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
            return Value::Int((i + 1) as i64);
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
            return Value::Int((i + 1) as i64);
        }
    }
    Value::Nil
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

    let Value::Cons(cell) = keymap else {
        return *keymap;
    };
    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("keymap") {
        return *keymap;
    }

    let mut elements = vec![Value::symbol("keymap")];
    let mut cursor = pair.cdr;
    let mut tail_parent = Value::Nil;

    while let Value::Cons(entry_cell) = cursor {
        if is_list_keymap(&cursor) {
            // Parent keymap: keep shared (don't recursively copy parent chain)
            tail_parent = cursor;
            break;
        }
        let entry = read_cons(entry_cell);

        if is_char_table(&entry.car) {
            // Deep-copy char-table: clone the vector, then recursively copy
            // any keymap entries within it.
            elements.push(copy_char_table_for_keymap(&entry.car, depth));
        } else if is_list_keymap(&entry.car) {
            // Nested keymap element — recursively copy
            elements.push(list_keymap_copy_impl(&entry.car, depth + 1));
        } else if let Value::Cons(binding_cell) = entry.car {
            // Alist entry (EVENT . DEF) — copy the cons, recurse if DEF is a keymap
            let binding = read_cons(binding_cell);
            let copied_def = copy_keymap_item(&binding.cdr, depth);
            elements.push(Value::cons(binding.car, copied_def));
        } else {
            elements.push(entry.car);
        }

        cursor = entry.cdr;
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
    let Value::Vector(arc) = ct else {
        return *ct;
    };
    let old_vec = with_heap(|h| h.get_vector(*arc).clone());
    let mut new_vec = old_vec.clone();

    // Walk the data pairs and recursively copy any keymap values.
    // Char-table layout: [tag, default, parent, subtype, extra_count, ...extras..., ...data-pairs...]
    // Data pairs start after extra slots (CT_EXTRA_START + n_extras),
    // stored as consecutive (char-code, value) pairs.
    let ct_extra_start = 5; // matches chartable.rs CT_EXTRA_START
    let n_extras = match new_vec.get(4) {
        Some(Value::Int(n)) => *n as usize,
        _ => 0,
    };
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

    let Value::Cons(cell) = keymap else {
        return;
    };
    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("keymap") {
        return;
    }

    // Scan alist entries for prefix keymaps
    let mut cursor = pair.cdr;
    while let Value::Cons(entry_cell) = cursor {
        if is_list_keymap(&cursor) {
            break;
        }
        let entry = read_cons(entry_cell);

        if let Value::Cons(binding_cell) = entry.car {
            let binding = read_cons(binding_cell);
            if is_list_keymap(&binding.cdr) {
                prefix.push(binding.car);
                list_keymap_accessible(&binding.cdr, prefix, out, seen);
                prefix.pop();
            }
        }

        if is_list_keymap(&entry.cdr) {
            break; // parent keymap, don't descend
        }
        cursor = entry.cdr;
    }

    seen.pop();
}

/// Check if two keymap values are the same object (by cons cell identity).
fn keymap_value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Cons(x), Value::Cons(y)) => x == y,
        _ => false,
    }
}

/// Iterate over all bindings in a keymap (not following parent).
/// Calls `f(event, def)` for each binding.
pub fn list_keymap_for_each_binding<F>(keymap: &Value, mut f: F)
where
    F: FnMut(Value, Value),
{
    let Value::Cons(cell) = keymap else {
        return;
    };
    let pair = read_cons(*cell);
    if pair.car.as_symbol_name() != Some("keymap") {
        return;
    }

    let mut cursor = pair.cdr;
    while let Value::Cons(entry_cell) = cursor {
        if is_list_keymap(&cursor) {
            break;
        }
        let entry = read_cons(entry_cell);

        if super::chartable::is_char_table(&entry.car) {
            super::chartable::for_each_non_nil_char_table_run(&entry.car, &mut f);
        }

        if let Value::Cons(binding_cell) = entry.car {
            let binding = read_cons(binding_cell);
            f(binding.car, binding.cdr);
        }

        if is_list_keymap(&entry.cdr) {
            break;
        }
        cursor = entry.cdr;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "keymap_test.rs"]
mod tests;
