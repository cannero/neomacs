use super::*;
use crate::heap_types::LispString;

#[test]
fn substring_preserves_raw_unibyte_storage_semantics() {
    crate::test_utils::init_test_tracing();

    let source = Value::heap_string(LispString::from_unibyte(vec![0xff, b'a', b'b']));
    let result = builtin_substring(vec![source, Value::fixnum(1), Value::fixnum(3)])
        .expect("substring should accept raw unibyte storage");
    let string = result
        .as_lisp_string()
        .expect("substring should return a string");

    assert!(!string.is_multibyte());
    assert_eq!(string.as_bytes(), b"ab");
}

#[test]
fn substring_can_return_raw_non_utf8_unibyte_bytes() {
    crate::test_utils::init_test_tracing();

    let source = Value::heap_string(LispString::from_unibyte(vec![0xff, 0xfe, b'x']));
    let result = builtin_substring(vec![source, Value::fixnum(0), Value::fixnum(2)])
        .expect("substring should slice raw unibyte bytes");
    let string = result
        .as_lisp_string()
        .expect("substring should return a string");

    assert!(!string.is_multibyte());
    assert_eq!(string.as_bytes(), &[0xff, 0xfe]);
}
