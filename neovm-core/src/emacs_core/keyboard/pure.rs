use crate::emacs_core::{
    error::{Flow, signal},
    intern::resolve_sym,
    string_escape::encode_nonunicode_char_for_storage,
    value::{Value, list_to_vec, with_heap},
};

pub(crate) const KEY_CHAR_META: i64 = 0x8000000;
pub(crate) const KEY_CHAR_CTRL: i64 = 0x4000000;
pub(crate) const KEY_CHAR_SHIFT: i64 = 0x2000000;
pub(crate) const KEY_CHAR_SUPER: i64 = 0x0800000;
pub(crate) const KEY_CHAR_HYPER: i64 = 0x1000000;
pub(crate) const KEY_CHAR_ALT: i64 = 0x0400000;
pub(crate) const KEY_CHAR_MOD_MASK: i64 =
    KEY_CHAR_META | KEY_CHAR_CTRL | KEY_CHAR_SHIFT | KEY_CHAR_SUPER | KEY_CHAR_HYPER | KEY_CHAR_ALT;
pub(crate) const KEY_CHAR_CODE_MASK: i64 = 0x3FFFFF;

fn invalid_single_key_error() -> Flow {
    signal(
        "error",
        vec![Value::string(
            "KEY must be an integer, cons, symbol, or string",
        )],
    )
}

fn control_char_suffix(code: i64) -> Option<char> {
    match code {
        0 => Some('@'),
        1..=26 => char::from_u32((code as u32) + 96),
        28 => Some('\\'),
        29 => Some(']'),
        30 => Some('^'),
        31 => Some('_'),
        _ => None,
    }
}

fn named_char_name(code: i64) -> Option<&'static str> {
    match code {
        9 => Some("TAB"),
        13 => Some("RET"),
        27 => Some("ESC"),
        32 => Some("SPC"),
        127 => Some("DEL"),
        _ => None,
    }
}

fn split_symbol_modifiers(mut name: &str) -> (String, &str) {
    let mut prefix = String::new();
    let is_single_char = |s: &str| {
        let mut chars = s.chars();
        chars.next().is_some() && chars.next().is_none()
    };
    loop {
        if let Some(rest) = name.strip_prefix("C-") {
            if is_single_char(rest) {
                break;
            }
            prefix.push_str("C-");
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("M-") {
            if is_single_char(rest) {
                break;
            }
            prefix.push_str("M-");
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("S-") {
            if is_single_char(rest) {
                break;
            }
            prefix.push_str("S-");
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("s-") {
            if is_single_char(rest) {
                break;
            }
            prefix.push_str("s-");
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("H-") {
            if is_single_char(rest) {
                break;
            }
            prefix.push_str("H-");
            name = rest;
            continue;
        }
        if let Some(rest) = name.strip_prefix("A-") {
            if is_single_char(rest) {
                break;
            }
            prefix.push_str("A-");
            name = rest;
            continue;
        }
        break;
    }
    (prefix, name)
}

fn describe_symbol_key(name: &str, no_angles: bool) -> String {
    let (prefix, base) = split_symbol_modifiers(name);
    if no_angles {
        return format!("{prefix}{base}");
    }
    format!("{prefix}<{base}>")
}

fn describe_int_key(code: i64) -> Result<String, Flow> {
    let mods = code & KEY_CHAR_MOD_MASK;
    let base = code & !KEY_CHAR_MOD_MASK;
    if !(0..=KEY_CHAR_CODE_MASK).contains(&base) {
        return Err(invalid_single_key_error());
    }

    let ctrl = (mods & KEY_CHAR_CTRL) != 0;
    let meta = (mods & KEY_CHAR_META) != 0;
    let shift = (mods & KEY_CHAR_SHIFT) != 0;
    let super_ = (mods & KEY_CHAR_SUPER) != 0;

    let push_prefixes = |out: &mut String, with_ctrl: bool| {
        if (mods & KEY_CHAR_ALT) != 0 {
            out.push_str("A-");
        }
        if with_ctrl {
            out.push_str("C-");
        }
        if (mods & KEY_CHAR_HYPER) != 0 {
            out.push_str("H-");
        }
        if meta {
            out.push_str("M-");
        }
        if shift {
            out.push_str("S-");
        }
        if super_ {
            out.push_str("s-");
        }
    };

    let mut out = String::new();

    // Emacs renders M-TAB style integer events through control notation (`C-M-i`),
    // while plain/shift/super/alt TAB keeps named `TAB` rendering.
    let tab_meta_control_notation = base == 9 && meta;
    if !tab_meta_control_notation {
        if let Some(name) = named_char_name(base) {
            push_prefixes(&mut out, ctrl);
            out.push_str(name);
            return Ok(out);
        }
    }

    if let Some(sfx) = control_char_suffix(base) {
        push_prefixes(&mut out, true);
        out.push(sfx.to_ascii_lowercase());
        return Ok(out);
    }

    push_prefixes(&mut out, ctrl);
    if let Some(ch) = char::from_u32(base as u32) {
        out.push(ch);
    } else if let Some(encoded) = encode_nonunicode_char_for_storage(base as u32) {
        out.push_str(&encoded);
    } else {
        return Err(invalid_single_key_error());
    }
    Ok(out)
}

pub(crate) fn describe_single_key_value(value: &Value, no_angles: bool) -> Result<String, Flow> {
    match value {
        Value::Int(n) => describe_int_key(*n),
        Value::Char(c) => describe_int_key(*c as i64),
        Value::Symbol(id) => Ok(describe_symbol_key(resolve_sym(*id), no_angles)),
        Value::True => Ok(describe_symbol_key("t", no_angles)),
        Value::Nil => Ok(describe_symbol_key("nil", no_angles)),
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        Value::Cons(_) => {
            let items = list_to_vec(value).ok_or_else(invalid_single_key_error)?;
            if items.len() != 1 {
                return Err(invalid_single_key_error());
            }
            describe_single_key_value(&items[0], no_angles)
        }
        _ => Err(invalid_single_key_error()),
    }
}

pub(crate) fn key_sequence_values(value: &Value) -> Result<Vec<Value>, Flow> {
    match value {
        Value::Nil => Ok(vec![]),
        Value::Str(id) => {
            let s = with_heap(|h| h.get_string(*id).to_owned());
            Ok(s.chars().map(|ch| Value::Int(ch as i64)).collect())
        }
        Value::Vector(v) => Ok(with_heap(|h| h.get_vector(*v).clone())),
        Value::Cons(_) => list_to_vec(value).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("sequencep"), *value],
            )
        }),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *value],
        )),
    }
}

pub(crate) fn resolve_control_code(code: i64) -> Option<i64> {
    match code {
        32 => Some(0),               // SPC
        63 => Some(127),             // ?
        64 => Some(0),               // @
        65..=90 => Some(code - 64),  // A-Z
        91 => Some(27),              // [
        92 => Some(28),              // \
        93 => Some(29),              // ]
        94 => Some(30),              // ^
        95 => Some(31),              // _
        97..=122 => Some(code - 96), // a-z
        _ => None,
    }
}

pub(crate) fn event_modifier_bit(symbol: &str) -> Option<i64> {
    match symbol {
        "control" => Some(KEY_CHAR_CTRL),
        "meta" => Some(KEY_CHAR_META),
        "shift" => Some(KEY_CHAR_SHIFT),
        "super" => Some(KEY_CHAR_SUPER),
        "hyper" => Some(KEY_CHAR_HYPER),
        "alt" => Some(KEY_CHAR_ALT),
        _ => None,
    }
}

pub(crate) fn event_modifier_prefix(bits: i64) -> String {
    let mut out = String::new();
    if (bits & KEY_CHAR_CTRL) != 0 {
        out.push_str("C-");
    }
    if (bits & KEY_CHAR_META) != 0 {
        out.push_str("M-");
    }
    if (bits & KEY_CHAR_SHIFT) != 0 {
        out.push_str("S-");
    }
    if (bits & KEY_CHAR_SUPER) != 0 {
        out.push_str("s-");
    }
    if (bits & KEY_CHAR_HYPER) != 0 {
        out.push_str("H-");
    }
    if (bits & KEY_CHAR_ALT) != 0 {
        out.push_str("A-");
    }
    out
}

pub(crate) fn basic_char_code(mut code: i64) -> i64 {
    code &= KEY_CHAR_CODE_MASK;
    match code {
        0 => 64,
        1..=26 => code + 96,
        27..=31 => code + 64,
        65..=90 => code + 32,
        _ => code,
    }
}

pub(crate) fn symbol_has_modifier_prefix(name: &str) -> bool {
    name.starts_with("C-")
        || name.starts_with("M-")
        || name.starts_with("S-")
        || name.starts_with("s-")
        || name.starts_with("H-")
        || name.starts_with("A-")
}

pub(crate) fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    use crate::emacs_core::value::Value;

    obarray.set_symbol_value("help-char", Value::Int(8)); // Ctrl-H, keyboard.c:13058
    obarray.set_symbol_value("help-form", Value::Nil);
    obarray.set_symbol_value("help-event-list", Value::Nil);
    obarray.set_symbol_value("suggest-key-bindings", Value::True);
    obarray.set_symbol_value("timer-idle-list", Value::Nil);
    obarray.set_symbol_value("timer-list", Value::Nil);
    obarray.set_symbol_value("input-method-previous-message", Value::Nil);
    obarray.set_symbol_value("auto-save-interval", Value::Int(300));
    obarray.set_symbol_value("auto-save-timeout", Value::Int(30));
    obarray.set_symbol_value("echo-keystrokes", Value::Int(1));
    obarray.set_symbol_value("polling-period", Value::Int(2));
    obarray.set_symbol_value("double-click-time", Value::Int(500));
    obarray.set_symbol_value("double-click-fuzz", Value::Int(3));
    obarray.set_symbol_value("num-input-keys", Value::Int(0));
    obarray.set_symbol_value("num-nonmacro-input-events", Value::Int(0));
    obarray.set_symbol_value("last-event-frame", Value::Nil);
    obarray.set_symbol_value("tty-erase-char", Value::Int(0));
    obarray.set_symbol_value("extra-keyboard-modifiers", Value::Int(0));
    obarray.set_symbol_value("inhibit-local-menu-bar-menus", Value::Nil);
    obarray.set_symbol_value("meta-prefix-char", Value::Int(27));
    obarray.set_symbol_value("enable-disabled-menus-and-buttons", Value::Nil);
    obarray.set_symbol_value("select-active-regions", Value::symbol("only"));
    obarray.set_symbol_value("saved-region-selection", Value::Nil);
    obarray.set_symbol_value(
        "selection-inhibit-update-commands",
        Value::list(vec![
            Value::symbol("handle-switch-frame"),
            Value::symbol("handle-select-window"),
            Value::symbol("handle-focus-in"),
            Value::symbol("handle-focus-out"),
        ]),
    );
    obarray.set_symbol_value("minor-mode-map-alist", Value::Nil);
    obarray.set_symbol_value("minor-mode-overriding-map-alist", Value::Nil);
    obarray.set_symbol_value("emulation-mode-map-alists", Value::Nil);
}
#[cfg(test)]
#[path = "pure_test.rs"]
mod tests;
