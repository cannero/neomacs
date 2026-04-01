use super::pure::{
    KEY_CHAR_CODE_MASK, KEY_CHAR_CTRL, basic_char_code, describe_single_key_value,
    event_modifier_bit, event_modifier_prefix, key_sequence_values, resolve_control_code,
    symbol_has_modifier_prefix,
};
use crate::emacs_core::value::Value;

#[test]
fn describe_int_key_succeeds() {
    let value = Value::fixnum(97);
    assert_eq!(describe_single_key_value(&value, false).unwrap(), "a");
}

#[test]
fn key_sequence_values_accept_string_and_list() {
    let string = Value::string("abc");
    let list: Value =
        crate::emacs_core::value::Value::list(vec![Value::fixnum(97), Value::fixnum(98)]);
    assert_eq!(
        key_sequence_values(&string).unwrap(),
        vec![Value::fixnum(97), Value::fixnum(98), Value::fixnum(99)]
    );
    assert_eq!(
        key_sequence_values(&list).unwrap(),
        vec![Value::fixnum(97), Value::fixnum(98)]
    );
}

#[test]
fn symbol_modifier_helpers() {
    assert!(symbol_has_modifier_prefix("C-x"));
    assert!(!symbol_has_modifier_prefix("foo"));
    assert_eq!(event_modifier_bit("control"), Some(KEY_CHAR_CTRL));
    assert!(event_modifier_prefix(KEY_CHAR_CTRL).starts_with("C-"));
}

#[test]
fn control_code_resolution() {
    assert_eq!(resolve_control_code(65), Some(1));
    assert_eq!(resolve_control_code(97), Some(1));
    assert!(resolve_control_code(999).is_none());
}

#[test]
fn basic_char_code_masks() {
    let bits = 0x123456;
    assert!(basic_char_code(bits) <= KEY_CHAR_CODE_MASK);
}
