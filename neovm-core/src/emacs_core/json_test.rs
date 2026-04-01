use super::*;
use crate::emacs_core::intern::intern;
use crate::emacs_core::value::{ValueKind, VecLikeType};

// -----------------------------------------------------------------------
// Serializer tests
// -----------------------------------------------------------------------

#[test]
fn serialize_null() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::NIL]);
    assert_eq!(result.unwrap().as_str(), Some("null"));
}

#[test]
fn serialize_true() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::T]);
    assert_eq!(result.unwrap().as_str(), Some("true"));
}

#[test]
fn serialize_false_keyword() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::keyword(intern(":false"))]);
    assert_eq!(result.unwrap().as_str(), Some("false"));
}

#[test]
fn serialize_json_false_keyword() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::keyword(intern(":json-false"))]);
    assert_eq!(result.unwrap().as_str(), Some("false"));
}

#[test]
fn serialize_integer() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::fixnum(42)]);
    assert_eq!(result.unwrap().as_str(), Some("42"));
}

#[test]
fn serialize_negative_integer() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::fixnum(-7)]);
    assert_eq!(result.unwrap().as_str(), Some("-7"));
}

#[test]
fn serialize_float() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::make_float(3.14)]);
    assert_eq!(result.unwrap().as_str(), Some("3.14"));
}

#[test]
fn serialize_float_whole() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::make_float(1.0)]);
    assert_eq!(result.unwrap().as_str(), Some("1.0"));
}

#[test]
fn serialize_nan_errors() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::make_float(f64::NAN)]);
    assert!(result.is_err());
}

#[test]
fn serialize_inf_errors() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::make_float(f64::INFINITY)]);
    assert!(result.is_err());
}

#[test]
fn serialize_string() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::string("hello")]);
    assert_eq!(result.unwrap().as_str(), Some("\"hello\""));
}

#[test]
fn serialize_string_with_escapes() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::string("a\"b\\c\ndef")]);
    assert_eq!(result.unwrap().as_str(), Some("\"a\\\"b\\\\c\\ndef\""));
}

#[test]
fn serialize_empty_vector() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::vector(vec![])]);
    assert_eq!(result.unwrap().as_str(), Some("[]"));
}

#[test]
fn serialize_vector() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![Value::vector(vec![
        Value::fixnum(1),
        Value::string("two"),
        Value::T,
        Value::NIL,
    ])]);
    assert_eq!(result.unwrap().as_str(), Some("[1,\"two\",true,null]"));
}

#[test]
fn serialize_hash_table() {
    crate::test_utils::init_test_tracing();
    let ht = Value::hash_table(HashTableTest::Equal);
    {
        let table = ht.as_hash_table_mut().unwrap();
        table
            .data
            .insert(HashKey::from_str("name"), Value::string("Alice"));
    }
    let result = builtin_json_serialize(vec![ht]);
    assert_eq!(result.unwrap().as_str(), Some("{\"name\":\"Alice\"}"));
}

#[test]
fn serialize_alist() {
    crate::test_utils::init_test_tracing();
    let alist = Value::list(vec![
        Value::cons(Value::symbol("a"), Value::fixnum(1)),
        Value::cons(Value::symbol("b"), Value::fixnum(2)),
    ]);
    let result = builtin_json_serialize(vec![alist]);
    assert_eq!(result.unwrap().as_str(), Some("{\"a\":1,\"b\":2}"));
}

#[test]
fn serialize_nested() {
    crate::test_utils::init_test_tracing();
    let inner = Value::vector(vec![Value::fixnum(1), Value::fixnum(2)]);
    let alist = Value::list(vec![Value::cons(Value::symbol("arr"), inner)]);
    let result = builtin_json_serialize(vec![alist]);
    assert_eq!(result.unwrap().as_str(), Some("{\"arr\":[1,2]}"));
}

#[test]
fn serialize_alist_string_key_type_error() {
    crate::test_utils::init_test_tracing();
    let alist = Value::list(vec![Value::cons(Value::string("a"), Value::fixnum(1))]);
    match builtin_json_serialize(vec![alist]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.first(), Some(&Value::symbol("symbolp")));
        }
        other => panic!("expected wrong-type-argument signal, got {:?}", other),
    }
}

#[test]
fn serialize_custom_false_object() {
    crate::test_utils::init_test_tracing();
    // Use nil as the false-object.
    let result = builtin_json_serialize(vec![
        Value::NIL,
        Value::keyword(intern(":false-object")),
        Value::NIL,
    ]);
    // nil matches both null_object (default) and false_object (nil).
    // null_object is checked first, so it becomes "null".
    assert_eq!(result.unwrap().as_str(), Some("null"));
}

#[test]
fn serialize_wrong_no_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_serialize(vec![]);
    assert!(result.is_err());
}

#[test]
fn json_parse_buffer_advances_point_after_value() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert(" 42 trailing");
        buf.goto_char(0);
    }

    let value = builtin_json_parse_buffer(&mut eval, vec![]).expect("parse buffer");
    assert_eq!(value, Value::fixnum(42));
    assert_eq!(
        eval.buffers
            .current_buffer()
            .expect("current buffer")
            .point(),
        3
    );
}

#[test]
fn json_insert_writes_at_point_and_advances() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.delete_region(buf.point_min(), buf.point_max());
        buf.insert("ab");
        buf.goto_char(1);
    }

    builtin_json_insert(
        &mut eval,
        vec![Value::vector(vec![Value::fixnum(1), Value::T])],
    )
    .expect("json insert");

    let buf = eval.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.buffer_string(), "a[1,true]b");
    assert_eq!(buf.point(), 9);
}

// -----------------------------------------------------------------------
// Parser tests
// -----------------------------------------------------------------------

#[test]
fn parse_null() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("null")]);
    let val = result.unwrap();
    assert!(
        val.as_keyword_id()
            .map_or(false, |k| resolve_sym(k) == ":null")
    );
}

#[test]
fn parse_true() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("true")]);
    assert!(result.unwrap().is_t());
}

#[test]
fn parse_false() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("false")]);
    let val = result.unwrap();
    assert!(
        val.as_keyword_id()
            .map_or(false, |k| resolve_sym(k) == ":false")
    );
}

#[test]
fn parse_integer() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("42")]);
    assert!(result.unwrap().is_fixnum());
}

#[test]
fn parse_negative_integer() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("-7")]);
    assert!(result.unwrap().is_fixnum());
}

#[test]
fn parse_float() {
    crate::test_utils::init_test_tracing();
    let val = builtin_json_parse_string(vec![Value::string("3.14")]).unwrap();
    match val.kind() {
        ValueKind::Float => assert!((val.as_float().unwrap() - 3.14).abs() < 1e-10),
        _ => panic!("expected float, got {:?}", val),
    }
}

#[test]
fn parse_float_exponent() {
    crate::test_utils::init_test_tracing();
    let val = builtin_json_parse_string(vec![Value::string("1.5e2")]).unwrap();
    match val.kind() {
        ValueKind::Float => assert!((val.as_float().unwrap() - 150.0).abs() < 1e-10),
        _ => panic!("expected float, got {:?}", val),
    }
}

#[test]
fn parse_zero() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("0")]);
    assert!(result.unwrap().is_fixnum());
}

#[test]
fn parse_string_simple() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("\"hello\"")]);
    assert_eq!(result.unwrap().as_str(), Some("hello"));
}

#[test]
fn parse_string_with_escapes() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("\"a\\\"b\\\\c\\nd\"")]);
    assert_eq!(result.unwrap().as_str(), Some("a\"b\\c\nd"));
}

#[test]
fn parse_string_unicode_escape() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("\"\\u0041\"")]);
    assert_eq!(result.unwrap().as_str(), Some("A"));
}

#[test]
fn parse_string_surrogate_pair() {
    crate::test_utils::init_test_tracing();
    // U+1F600 (grinning face) = \uD83D\uDE00
    let result = builtin_json_parse_string(vec![Value::string("\"\\uD83D\\uDE00\"")]);
    let val = result.unwrap();
    assert_eq!(val.as_str(), Some("\u{1F600}"));
}

#[test]
fn parse_empty_array() {
    crate::test_utils::init_test_tracing();
    let val = builtin_json_parse_string(vec![Value::string("[]")]).unwrap();
    match val.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            assert!(val.as_vector_data().unwrap().is_empty())
        }
        _ => panic!("expected vector, got {:?}", val),
    }
}

#[test]
fn parse_array() {
    crate::test_utils::init_test_tracing();
    let val = builtin_json_parse_string(vec![Value::string("[1, 2, 3]")]).unwrap();
    match val.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = val.as_vector_data().unwrap().clone();
            assert_eq!(items.len(), 3);
            assert!(items[0].is_fixnum());
            assert!(items[1].is_fixnum());
            assert!(items[2].is_fixnum());
        }
        _ => panic!("expected vector, got {:?}", val),
    }
}

#[test]
fn parse_array_as_list() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![
        Value::string("[1, 2]"),
        Value::keyword(intern(":array-type")),
        Value::symbol("list"),
    ]);
    let val = result.unwrap();
    let items = list_to_vec(&val).expect("should be a list");
    assert_eq!(items.len(), 2);
    assert!(items[0].is_fixnum());
    assert!(items[1].is_fixnum());
}

#[test]
fn parse_empty_object() {
    crate::test_utils::init_test_tracing();
    let val = builtin_json_parse_string(vec![Value::string("{}")]).unwrap();
    match val.kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = val.as_hash_table().unwrap();
            assert!(table.data.is_empty());
        }
        _ => panic!("expected hash-table, got {:?}", val),
    }
}

#[test]
fn parse_object_hash_table() {
    crate::test_utils::init_test_tracing();
    let val = builtin_json_parse_string(vec![Value::string("{\"a\": 1, \"b\": 2}")]).unwrap();
    match val.kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = val.as_hash_table().unwrap();
            assert_eq!(table.data.len(), 2);
            assert_eq!(table.key_snapshots.len(), 2);
            assert_eq!(
                table
                    .data
                    .get(&HashKey::from_str("a"))
                    .map(|v| v.as_fixnum()),
                Some(Some(1))
            );
            assert_eq!(
                table
                    .data
                    .get(&HashKey::from_str("b"))
                    .map(|v| v.as_fixnum()),
                Some(Some(2))
            );
            assert!(matches!(
                table.key_snapshots.get(&HashKey::from_str("a")),
                Some(key) if key.as_str() == Some("a")
            ));
            assert!(matches!(
                table.key_snapshots.get(&HashKey::from_str("b")),
                Some(key) if key.as_str() == Some("b")
            ));
        }
        _ => panic!("expected hash-table, got {:?}", val),
    }
}

#[test]
fn parse_object_as_alist() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![
        Value::string("{\"x\": 10}"),
        Value::keyword(intern(":object-type")),
        Value::symbol("alist"),
    ]);
    let val = result.unwrap();
    let items = list_to_vec(&val).expect("should be a list");
    assert_eq!(items.len(), 1);
    // Each item should be (key . value).
    match items[0].kind() {
        ValueKind::Cons => {
            let pair_car = items[0].cons_car();
            let pair_cdr = items[0].cons_cdr();
            assert_eq!(pair_car, Value::symbol("x"));
            assert!(pair_cdr.is_fixnum());
        }
        other => panic!("expected cons, got {:?}", items[0]),
    }
}

#[test]
fn parse_object_as_plist() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![
        Value::string("{\"key\": 42}"),
        Value::keyword(intern(":object-type")),
        Value::symbol("plist"),
    ]);
    let val = result.unwrap();
    let items = list_to_vec(&val).expect("should be a list");
    assert_eq!(items.len(), 2);
    assert!(
        items[0]
            .as_keyword_id()
            .map_or(false, |k| resolve_sym(k) == ":key")
    );
    assert!(items[1].is_fixnum());
}

#[test]
fn parse_nested() {
    crate::test_utils::init_test_tracing();
    let json = r#"{"arr": [1, {"nested": true}], "val": null}"#;
    let result = builtin_json_parse_string(vec![Value::string(json)]);
    assert!(result.is_ok());
}

#[test]
fn parse_custom_null_object() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![
        Value::string("null"),
        Value::keyword(intern(":null-object")),
        Value::NIL,
    ]);
    assert!(result.unwrap().is_nil());
}

#[test]
fn parse_custom_false_object() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![
        Value::string("false"),
        Value::keyword(intern(":false-object")),
        Value::keyword(intern(":json-false")),
    ]);
    let val = result.unwrap();
    assert!(
        val.as_keyword_id()
            .map_or(false, |k| resolve_sym(k) == ":json-false")
    );
}

#[test]
fn parse_trailing_content_error() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("42 extra")]);
    assert!(result.is_err());
}

#[test]
fn parse_empty_string_error() {
    crate::test_utils::init_test_tracing();
    match builtin_json_parse_string(vec![Value::string("")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "json-end-of-file");
        }
        other => panic!("expected json-end-of-file signal, got {:?}", other),
    }
}

#[test]
fn parse_invalid_json_error() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("{bad}")]);
    assert!(result.is_err());
}

#[test]
fn parse_wrong_type_argument() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::fixnum(42)]);
    assert!(result.is_err());
}

#[test]
fn parse_no_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// Round-trip tests
// -----------------------------------------------------------------------

#[test]
fn round_trip_integer() {
    crate::test_utils::init_test_tracing();
    let serialized = builtin_json_serialize(vec![Value::fixnum(123)]).unwrap();
    let parsed = builtin_json_parse_string(vec![serialized]).unwrap();
    assert!(parsed.is_fixnum());
}

#[test]
fn round_trip_string() {
    crate::test_utils::init_test_tracing();
    let original = Value::string("hello \"world\"\ntest");
    let serialized = builtin_json_serialize(vec![original]).unwrap();
    let parsed = builtin_json_parse_string(vec![serialized]).unwrap();
    assert_eq!(parsed.as_str(), Some("hello \"world\"\ntest"));
}

#[test]
fn round_trip_array() {
    crate::test_utils::init_test_tracing();
    let original = Value::vector(vec![Value::fixnum(1), Value::string("two"), Value::T]);
    let serialized = builtin_json_serialize(vec![original]).unwrap();
    let parsed = builtin_json_parse_string(vec![serialized]).unwrap();
    match parsed.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = parsed.as_vector_data().unwrap().clone();
            assert_eq!(items.len(), 3);
            assert!(items[0].is_fixnum());
            assert_eq!(items[1].as_str(), Some("two"));
            assert!(items[2].is_t());
        }
        _ => panic!("expected vector"),
    }
}

#[test]
fn round_trip_object() {
    crate::test_utils::init_test_tracing();
    let ht = Value::hash_table(HashTableTest::Equal);
    {
        let table = ht.as_hash_table_mut().unwrap();
        table
            .data
            .insert(HashKey::from_str("key"), Value::fixnum(99));
    }
    let serialized = builtin_json_serialize(vec![ht]).unwrap();
    let parsed = builtin_json_parse_string(vec![serialized]).unwrap();
    match parsed.kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = parsed.as_hash_table().unwrap();
            assert_eq!(
                table
                    .data
                    .get(&HashKey::from_str("key"))
                    .map(|v| v.as_fixnum()),
                Some(Some(99))
            );
        }
        _ => panic!("expected hash-table"),
    }
}

// -----------------------------------------------------------------------
// String encoding edge cases
// -----------------------------------------------------------------------

#[test]
fn encode_control_chars() {
    crate::test_utils::init_test_tracing();
    let s = "a\x00b\x01c";
    let encoded = json_encode_string(s);
    assert_eq!(encoded, "\"a\\u0000b\\u0001c\"");
}

#[test]
fn encode_backspace_formfeed() {
    crate::test_utils::init_test_tracing();
    let s = "\x08\x0C";
    let encoded = json_encode_string(s);
    assert_eq!(encoded, "\"\\b\\f\"");
}

#[test]
fn parse_large_number_as_float() {
    crate::test_utils::init_test_tracing();
    // Number too large for i64.
    let val = builtin_json_parse_string(vec![Value::string("99999999999999999999")]).unwrap();
    match val.kind() {
        ValueKind::Float => {} // OK — fell back to f64
        _ => panic!("expected float for large number, got {:?}", val),
    }
}

#[test]
fn serialize_symbol_key_in_alist() {
    crate::test_utils::init_test_tracing();
    let alist = Value::list(vec![Value::cons(
        Value::symbol("name"),
        Value::string("test"),
    )]);
    let result = builtin_json_serialize(vec![alist]);
    assert_eq!(result.unwrap().as_str(), Some("{\"name\":\"test\"}"));
}

#[test]
fn parse_whitespace_around_values() {
    crate::test_utils::init_test_tracing();
    let result = builtin_json_parse_string(vec![Value::string("  {  \"a\"  :  1  }  ")]);
    assert!(result.is_ok());
}

#[test]
fn parse_deeply_nested() {
    crate::test_utils::init_test_tracing();
    let json = "[[[[[[1]]]]]]";
    let result = builtin_json_parse_string(vec![Value::string(json)]);
    assert!(result.is_ok());
}
