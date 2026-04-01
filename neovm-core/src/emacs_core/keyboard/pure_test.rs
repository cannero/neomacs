use super::*;

// --- describe_int_key ---

#[test]
fn describe_int_key_plain_char() {
    assert_eq!(describe_int_key(65).unwrap(), "A");
    assert_eq!(describe_int_key(120).unwrap(), "x");
    assert_eq!(describe_int_key(48).unwrap(), "0");
}

#[test]
fn describe_int_key_with_control() {
    // C-a: control bit + 'a' (97)
    let code = KEY_CHAR_CTRL | 97;
    let desc = describe_int_key(code).unwrap();
    assert_eq!(desc, "C-a");
}

#[test]
fn describe_int_key_with_meta() {
    // M-x: meta bit + 'x' (120)
    let code = KEY_CHAR_META | 120;
    let desc = describe_int_key(code).unwrap();
    assert_eq!(desc, "M-x");
}

#[test]
fn describe_int_key_with_combined_modifiers() {
    // C-M-S-x: control + meta + shift + 'x' (120)
    let code = KEY_CHAR_CTRL | KEY_CHAR_META | KEY_CHAR_SHIFT | 120;
    let desc = describe_int_key(code).unwrap();
    assert_eq!(desc, "C-M-S-x");
}

#[test]
fn describe_int_key_named_chars() {
    assert_eq!(describe_int_key(9).unwrap(), "TAB");
    assert_eq!(describe_int_key(13).unwrap(), "RET");
    assert_eq!(describe_int_key(27).unwrap(), "ESC");
    assert_eq!(describe_int_key(32).unwrap(), "SPC");
    assert_eq!(describe_int_key(127).unwrap(), "DEL");
}

#[test]
fn describe_int_key_control_named() {
    // C-TAB
    let code = KEY_CHAR_CTRL | 9;
    assert_eq!(describe_int_key(code).unwrap(), "C-TAB");
    // C-RET
    let code = KEY_CHAR_CTRL | 13;
    assert_eq!(describe_int_key(code).unwrap(), "C-RET");
}

#[test]
fn describe_int_key_meta_tab_uses_control_notation() {
    // M-TAB → C-M-i (Emacs renders M-TAB through control notation)
    let code = KEY_CHAR_META | 9;
    let desc = describe_int_key(code).unwrap();
    assert_eq!(desc, "C-M-i");
}

// --- describe_single_key_value ---

#[test]
fn describe_single_key_value_symbol() {
    let val = Value::symbol("left");
    assert_eq!(describe_single_key_value(&val, false).unwrap(), "<left>");
    assert_eq!(describe_single_key_value(&val, true).unwrap(), "left");
}

#[test]
fn describe_single_key_value_string() {
    let val = Value::string("a");
    assert_eq!(describe_single_key_value(&val, false).unwrap(), "a");
}

// --- key_sequence_values ---

#[test]
fn key_sequence_values_string() {
    let val = Value::string("abc");
    let keys = key_sequence_values(&val).unwrap();
    assert_eq!(keys.len(), 3);
    assert_val_eq!(keys[0], Value::fixnum('a' as i64));
    assert_val_eq!(keys[1], Value::fixnum('b' as i64));
    assert_val_eq!(keys[2], Value::fixnum('c' as i64));
}

#[test]
fn key_sequence_values_vector() {
    let val = Value::vector(vec![Value::fixnum(1), Value::fixnum(2)]);
    let keys = key_sequence_values(&val).unwrap();
    assert_eq!(keys, vec![Value::fixnum(1), Value::fixnum(2)]);
}

#[test]
fn key_sequence_values_nil() {
    let keys = key_sequence_values(&Value::NIL).unwrap();
    assert!(keys.is_empty());
}

// --- resolve_control_code ---

#[test]
fn resolve_control_code_letters() {
    // a (97) → 1, z (122) → 26
    assert_eq!(resolve_control_code(97), Some(1));
    assert_eq!(resolve_control_code(122), Some(26));
    // A (65) → 1, Z (90) → 26
    assert_eq!(resolve_control_code(65), Some(1));
    assert_eq!(resolve_control_code(90), Some(26));
    // SPC → 0, ? → 127, @ → 0
    assert_eq!(resolve_control_code(32), Some(0));
    assert_eq!(resolve_control_code(63), Some(127));
    assert_eq!(resolve_control_code(64), Some(0));
}

#[test]
fn resolve_control_code_none() {
    assert_eq!(resolve_control_code(0), None);
    assert_eq!(resolve_control_code(200), None);
}

// --- event_modifier_bit ---

#[test]
fn event_modifier_bit_all() {
    assert_eq!(event_modifier_bit("control"), Some(KEY_CHAR_CTRL));
    assert_eq!(event_modifier_bit("meta"), Some(KEY_CHAR_META));
    assert_eq!(event_modifier_bit("shift"), Some(KEY_CHAR_SHIFT));
    assert_eq!(event_modifier_bit("super"), Some(KEY_CHAR_SUPER));
    assert_eq!(event_modifier_bit("hyper"), Some(KEY_CHAR_HYPER));
    assert_eq!(event_modifier_bit("alt"), Some(KEY_CHAR_ALT));
    assert_eq!(event_modifier_bit("unknown"), None);
}

// --- event_modifier_prefix ---

#[test]
fn event_modifier_prefix_combined() {
    let bits = KEY_CHAR_CTRL | KEY_CHAR_META;
    assert_eq!(event_modifier_prefix(bits), "C-M-");
}

// --- basic_char_code ---

#[test]
fn basic_char_code_cases() {
    assert_eq!(basic_char_code(0), 64); // NUL → @
    assert_eq!(basic_char_code(1), 97); // C-a → a
    assert_eq!(basic_char_code(26), 122); // C-z → z
    assert_eq!(basic_char_code(65), 97); // A → a (lowercase)
}

// --- symbol_has_modifier_prefix ---

#[test]
fn symbol_has_modifier_prefix_cases() {
    assert!(symbol_has_modifier_prefix("C-x"));
    assert!(symbol_has_modifier_prefix("M-x"));
    assert!(symbol_has_modifier_prefix("S-left"));
    assert!(symbol_has_modifier_prefix("s-a")); // super
    assert!(symbol_has_modifier_prefix("H-a")); // hyper
    assert!(symbol_has_modifier_prefix("A-a")); // alt
    assert!(!symbol_has_modifier_prefix("foo"));
    assert!(!symbol_has_modifier_prefix("left"));
}
