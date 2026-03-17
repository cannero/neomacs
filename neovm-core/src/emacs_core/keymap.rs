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
    KEY_CHAR_CODE_MASK, KEY_CHAR_CTRL, KEY_CHAR_META, KEY_CHAR_SHIFT, KEY_CHAR_SUPER,
};
use super::symbol::Obarray;
use super::value::{Value, read_cons};

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
    },
    /// A named function/special key (e.g. "return", "backspace", "f1").
    Function {
        name: String,
        ctrl: bool,
        meta: bool,
        shift: bool,
        super_: bool,
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
            },
            Key::Named(named) => {
                if matches!(named, NamedKey::Escape) {
                    return KeyEvent::Char {
                        code: '\u{1b}',
                        ctrl: ke.modifiers.ctrl,
                        meta: ke.modifiers.meta,
                        shift: false,
                        super_: ke.modifiers.super_,
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
                        };
                    }
                };
                KeyEvent::Function {
                    name: name.to_string(),
                    ctrl: ke.modifiers.ctrl,
                    meta: ke.modifiers.meta,
                    shift: ke.modifiers.shift,
                    super_: ke.modifiers.super_,
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

    let mut remainder = token;

    // Parse modifier prefixes: "C-", "M-", "S-", "s-"
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
        } else {
            break;
        }
    }

    if remainder.is_empty() {
        return Err(format!("incomplete key description: {}", token));
    }

    // Check for named special keys
    match remainder {
        "RET" | "return" => Ok(KeyEvent::Function {
            name: "return".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "TAB" | "tab" => Ok(KeyEvent::Function {
            name: "tab".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "SPC" | "space" => Ok(KeyEvent::Char {
            code: ' ',
            ctrl,
            meta,
            shift,
            super_,
        }),
        "ESC" | "escape" => Ok(KeyEvent::Char {
            code: '\u{1b}',
            ctrl,
            meta,
            shift,
            super_,
        }),
        "DEL" | "delete" => Ok(KeyEvent::Function {
            name: "delete".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "BS" | "backspace" => Ok(KeyEvent::Function {
            name: "backspace".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "up" => Ok(KeyEvent::Function {
            name: "up".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "down" => Ok(KeyEvent::Function {
            name: "down".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "left" => Ok(KeyEvent::Function {
            name: "left".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "right" => Ok(KeyEvent::Function {
            name: "right".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "home" => Ok(KeyEvent::Function {
            name: "home".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "end" => Ok(KeyEvent::Function {
            name: "end".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "prior" | "page-up" => Ok(KeyEvent::Function {
            name: "prior".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "next" | "page-down" => Ok(KeyEvent::Function {
            name: "next".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        "insert" => Ok(KeyEvent::Function {
            name: "insert".to_string(),
            ctrl,
            meta,
            shift,
            super_,
        }),
        other => {
            // Check for function keys: f1 .. f20
            if let Some(stripped) = other.strip_prefix('f') {
                if let Ok(n) = stripped.parse::<u32>() {
                    if (1..=20).contains(&n) {
                        return Ok(KeyEvent::Function {
                            name: format!("f{}", n),
                            ctrl,
                            meta,
                            shift,
                            super_,
                        });
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
            })
        }
    }
}

/// Format a key event back to a human-readable description string.
pub fn format_key_event(event: &KeyEvent) -> String {
    let mut parts = String::new();
    let (ctrl, meta, shift, super_) = match event {
        KeyEvent::Char {
            ctrl,
            meta,
            shift,
            super_,
            ..
        } => (*ctrl, *meta, *shift, *super_),
        KeyEvent::Function {
            ctrl,
            meta,
            shift,
            super_,
            ..
        } => (*ctrl, *meta, *shift, *super_),
    };
    if ctrl {
        parts.push_str("C-");
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
/// Returns the binding or `Value::Nil` if not found.
pub fn list_keymap_lookup_one(keymap: &Value, event: &Value) -> Value {
    let mut current = *keymap;

    loop {
        let Value::Cons(cell) = current else {
            return Value::Nil;
        };
        let pair = read_cons(cell);
        // First element must be 'keymap
        if pair.car.as_symbol_name() != Some("keymap") {
            return Value::Nil;
        }

        // Walk the CDR chain scanning for the binding
        let mut cursor = pair.cdr;
        while let Value::Cons(entry_cell) = cursor {
            let entry = read_cons(entry_cell);

            // Check if this element is a char-table
            if is_char_table(&entry.car) {
                // For integer events in range, look up in char-table
                if let Value::Int(code) = event {
                    let base = *code & KEY_CHAR_CODE_MASK;
                    if base >= 0 && (base <= 0x3FFFFF) {
                        let result =
                            builtin_char_table_range(vec![entry.car, *event]).unwrap_or(Value::Nil);
                        if !result.is_nil() {
                            return get_keyelt(result);
                        }
                    }
                }
                cursor = entry.cdr;
                continue;
            }

            // Check if this element is an alist entry: (EVENT . DEF)
            if let Value::Cons(binding_cell) = entry.car {
                let binding = read_cons(binding_cell);
                if events_match(&binding.car, event) {
                    return get_keyelt(binding.cdr);
                }
            }

            cursor = entry.cdr;
        }

        // If last CDR is itself a keymap, follow as parent
        if is_list_keymap(&cursor) {
            current = cursor;
        } else {
            return Value::Nil;
        }
    }
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
    let sym_name = binding.as_symbol_name()?;
    obarray
        .symbol_function(sym_name)
        .copied()
        .filter(is_list_keymap)
}

/// Define a binding in a keymap.
///
/// For integer events without modifier bits in full keymaps: stores in char-table.
/// Otherwise: prepends `(event . def)` to the alist portion.
pub fn list_keymap_define(keymap: Value, event: Value, def: Value) {
    let Value::Cons(root_cell) = keymap else {
        return;
    };
    let root = read_cons(root_cell);
    if root.car.as_symbol_name() != Some("keymap") {
        return;
    }

    // Check if full keymap (second element is char-table) and event is plain char
    let cdr = root.cdr;
    if let Value::Cons(second_cell) = cdr {
        let second = read_cons(second_cell);
        if is_char_table(&second.car) {
            // For plain character events (no modifier bits), use char-table
            if let Value::Int(code) = event {
                let base = code & KEY_CHAR_CODE_MASK;
                let mods = code & !KEY_CHAR_CODE_MASK;
                if mods == 0 && base >= 0 && base <= 0x3FFFFF {
                    let _ = builtin_set_char_table_range(vec![second.car, event, def]);
                    return;
                }
            }
            // For non-char events: prepend after char-table
            let binding = Value::cons(event, def);
            let new_cdr = Value::cons(binding, second.cdr);
            Value::Cons(second_cell).set_cdr(new_cdr);
            return;
        }
    }

    // Sparse keymap: prepend (event . def) right after 'keymap symbol
    let binding = Value::cons(event, def);
    let new_cdr = Value::cons(binding, cdr);
    Value::Cons(root_cell).set_cdr(new_cdr);
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
            Value::Int(bits)
        }
        KeyEvent::Function {
            name,
            ctrl,
            meta,
            shift,
            super_,
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
                return Value::Int(bits);
            }
            let mut prefix = String::new();
            if *ctrl {
                prefix.push_str("C-");
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
                })
            } else {
                let ch = char::from_u32(base as u32)?;
                Some(KeyEvent::Char {
                    code: ch,
                    ctrl: has_ctrl_bit,
                    meta,
                    shift,
                    super_,
                })
            }
        }
        Value::Char(c) => Some(KeyEvent::Char {
            code: *c,
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
        }),
        Value::Symbol(id) => {
            let name = resolve_sym(*id);
            // Parse modifier prefixes
            let mut rest = name;
            let mut ctrl = false;
            let mut meta = false;
            let mut shift = false;
            let mut super_ = false;
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
        if binding.is_nil() {
            // No binding for this event.  If we haven't consumed any
            // prefix keys yet (i == 0), the whole key is undefined → nil.
            // Otherwise we consumed `i` prefix events before failing →
            // return that count as an integer (official Emacs semantics).
            return if i == 0 {
                Value::Nil
            } else {
                Value::Int(i as i64)
            };
        }
        if i == events.len() - 1 {
            return binding;
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
pub fn list_keymap_define_seq(keymap: Value, events: &[Value], def: Value) {
    if events.is_empty() {
        return;
    }
    if events.len() == 1 {
        list_keymap_define(keymap, events[0], def);
        return;
    }

    let mut current_map = keymap;
    for (i, event) in events.iter().enumerate() {
        if i == events.len() - 1 {
            list_keymap_define(current_map, *event, def);
            return;
        }
        let binding = list_keymap_lookup_one(&current_map, event);
        if is_list_keymap(&binding) {
            current_map = binding;
        } else {
            // Create a new prefix keymap
            let prefix_map = make_sparse_list_keymap();
            list_keymap_define(current_map, *event, prefix_map);
            current_map = prefix_map;
        }
    }
}

/// Define a key in a keymap, resolving symbol prefix bindings through the
/// obarray before auto-creating nested prefix maps.
pub fn list_keymap_define_seq_in_obarray(
    obarray: &Obarray,
    keymap: Value,
    events: &[Value],
    def: Value,
) {
    if events.is_empty() {
        return;
    }
    if events.len() == 1 {
        list_keymap_define(keymap, events[0], def);
        return;
    }

    let mut current_map = keymap;
    for (i, event) in events.iter().enumerate() {
        if i == events.len() - 1 {
            list_keymap_define(current_map, *event, def);
            return;
        }
        let binding = list_keymap_lookup_one(&current_map, event);
        if let Some(prefix_map) = resolve_prefix_keymap_binding_in_obarray(obarray, &binding) {
            current_map = prefix_map;
        } else {
            let prefix_map = make_sparse_list_keymap();
            list_keymap_define(current_map, *event, prefix_map);
            current_map = prefix_map;
        }
    }
}

/// Deep-copy a keymap cons-list structure.
pub fn list_keymap_copy(keymap: &Value) -> Value {
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
        let entry = read_cons(entry_cell);

        if is_char_table(&entry.car) {
            // Copy char-table: for now we share (char-tables are mutable vectors)
            // A proper deep copy would require cloning the vector
            elements.push(entry.car);
        } else if is_list_keymap(&entry.car) {
            // Nested keymap in an alist entry — recursively copy
            elements.push(list_keymap_copy(&entry.car));
        } else if let Value::Cons(binding_cell) = entry.car {
            // Alist entry (EVENT . DEF) — copy the cons, recurse if DEF is a keymap
            let binding = read_cons(binding_cell);
            if is_list_keymap(&binding.cdr) {
                elements.push(Value::cons(binding.car, list_keymap_copy(&binding.cdr)));
            } else {
                elements.push(Value::cons(binding.car, binding.cdr));
            }
        } else {
            elements.push(entry.car);
        }

        // Check if cdr is a parent keymap
        if is_list_keymap(&entry.cdr) {
            // Keep parent shared (don't recursively copy parent chain)
            tail_parent = entry.cdr;
            break;
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
