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

#[test]
fn concat_preserves_multibyte_text_properties_as_char_intervals() {
    crate::test_utils::init_test_tracing();

    let source = Value::string("é");
    let mut table = crate::buffer::text_props::TextPropertyTable::new();
    table.put_property(0, 1, Value::symbol("face"), Value::symbol("bold"));
    crate::emacs_core::value::set_string_text_properties_table_for_value(source, table);

    let result = builtin_concat(vec![Value::string("x"), source, Value::string("z")])
        .expect("concat should preserve string properties");
    let props = crate::emacs_core::value::get_string_text_properties_table_for_value(result)
        .expect("result should carry text properties");
    let intervals = props.intervals_snapshot();

    assert_eq!(intervals.len(), 1);
    assert_eq!((intervals[0].start, intervals[0].end), (1, 2));
    assert_eq!(
        intervals[0].properties.get(&Value::symbol("face")),
        Some(&Value::symbol("bold"))
    );
}

#[test]
fn format_preserves_multibyte_text_properties_as_char_intervals() {
    crate::test_utils::init_test_tracing();

    let source = Value::string("éz");
    let mut table = crate::buffer::text_props::TextPropertyTable::new();
    table.put_property(0, 1, Value::symbol("face"), Value::symbol("bold"));
    crate::emacs_core::value::set_string_text_properties_table_for_value(source, table);

    let mut ctx = crate::emacs_core::eval::Context::new();
    let result = builtin_format_wrapper_strict_slice(&mut ctx, &[Value::string("%4s"), source])
        .expect("format should preserve string properties");
    let props = crate::emacs_core::value::get_string_text_properties_table_for_value(result)
        .expect("result should carry text properties");
    let intervals = props.intervals_snapshot();

    assert_eq!(result.as_utf8_str(), Some("  éz"));
    assert_eq!(intervals.len(), 1);
    assert_eq!((intervals[0].start, intervals[0].end), (2, 3));
    assert_eq!(
        intervals[0].properties.get(&Value::symbol("face")),
        Some(&Value::symbol("bold"))
    );
}
