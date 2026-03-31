use super::*;
use super::value::{ValueKind, VecLikeType};

// -----------------------------------------------------------------------
// Serializer tests
// -----------------------------------------------------------------------

#[test]
fn serialize_null() {
    let result = builtin_json_serialize(vec![Value::NIL]);
    assert_eq!(result.unwrap().as_str(), Some("null"));
}

#[test]
fn serialize_true() {
    let result = builtin_json_serialize(vec![Value::T]);
    assert_eq!(result.unwrap().as_str(), Some("true"));
}

#[test]
fn serialize_false_keyword() {
    let result = builtin_json_serialize(vec![Value::keyword(intern(":false"))]);
    assert_eq!(result.unwrap().as_str(), Some("false"));
}

#[test]
fn serialize_json_false_keyword() {
    let result = builtin_json_serialize(vec![Value::keyword(intern(":json-false"))]);
    assert_eq!(result.unwrap().as_str(), Some("false"));
}

#[test]
fn serialize_integer() {
    let result = builtin_json_serialize(vec![Value::fixnum(42)]);
    assert_eq!(result.unwrap().as_str(), Some("42"));
}

#[test]
fn serialize_negative_integer() {
    let result = builtin_json_serialize(vec![Value::fixnum(-7)]);
    assert_eq!(result.unwrap().as_str(), Some("-7"));
}

#[test]
fn serialize_float() {
    let result = builtin_json_serialize(vec![Value::make_float(3.14)]);
    assert_eq!(result.unwrap().as_str(), Some("3.14"));
}

#[test]
fn serialize_float_whole() {
    let result = builtin_json_serialize(vec![Value::make_float(1.0)]);
    assert_eq!(result.unwrap().as_str(), Some("1.0"));
}

#[test]
fn serialize_nan_errors() {
    let result = builtin_json_serialize(vec![Value::make_float(f64::NAN)]);
    assert!(result.is_err());
}

#[test]
fn serialize_inf_errors() {
    let result = builtin_json_serialize(vec![Value::make_float(f64::INFINITY)]);
    assert!(result.is_err());
}

#[test]
fn serialize_string() {
    let result = builtin_json_serialize(vec![Value::string("hello")]);
    assert_eq!(result.unwrap().as_str(), Some("\"hello\""));
}

#[test]
fn serialize_string_with_escapes() {
    let result = builtin_json_serialize(vec![Value::string("a\"b\\c\ndef")]);
    assert_eq!(result.unwrap().as_str(), Some("\"a\\\"b\\\\c\\ndef\""));
}

#[test]
fn serialize_empty_vector() {
    let result = builtin_json_serialize(vec![Value::vector(vec![])]);
    assert_eq!(result.unwrap().as_str(), Some("[]"));
}

#[test]
fn serialize_vector() {
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
    let ht = Value::hash_table(HashTableTest::Equal);
    if let Value::HashTable(ref table_arc) /* TODO(tagged): convert Value::HashTable to new API */ = ht {
        with_heap_mut(|h| {
            h.get_hash_table_mut(*table_arc)
                .data
                .insert(HashKey::from_str("name"), Value::string("Alice"));
        });
    }
    let result = builtin_json_serialize(vec![ht]);
    assert_eq!(result.unwrap().as_str(), Some("{\"name\":\"Alice\"}"));
}

#[test]
fn serialize_alist() {
    let alist = Value::list(vec![
        Value::cons(Value::symbol("a"), Value::fixnum(1)),
        Value::cons(Value::symbol("b"), Value::fixnum(2)),
    ]);
    let result = builtin_json_serialize(vec![alist]);
    assert_eq!(result.unwrap().as_str(), Some("{\"a\":1,\"b\":2}"));
}

#[test]
fn serialize_nested() {
    let inner = Value::vector(vec![Value::fixnum(1), Value::fixnum(2)]);
    let alist = Value::list(vec![Value::cons(Value::symbol("arr"), inner)]);
    let result = builtin_json_serialize(vec![alist]);
    assert_eq!(result.unwrap().as_str(), Some("{\"arr\":[1,2]}"));
}

#[test]
fn serialize_alist_string_key_type_error() {
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
    let result = builtin_json_serialize(vec![]);
    assert!(result.is_err());
}

#[test]
fn json_parse_buffer_advances_point_after_value() {
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
    let result = builtin_json_parse_string(vec![Value::string("null")]);
    let val = result.unwrap();
    assert!(val.as_keyword_id().map_or(false, |k| resolve_sym(k) == ":null"));
}

#[test]
fn parse_true() {
    let result = builtin_json_parse_string(vec![Value::string("true")]);
    assert!(matches!(result.unwrap(), Value::T));
}

#[test]
fn parse_false() {
    let result = builtin_json_parse_string(vec![Value::string("false")]);
    let val = result.unwrap();
    assert!(val.as_keyword_id().map_or(false, |k| resolve_sym(k) == ":false"));
}

#[test]
fn parse_integer() {
    let result = builtin_json_parse_string(vec![Value::string("42")]);
    assert!(matches!(result.unwrap(), Value::fixnum(42)));
}

#[test]
fn parse_negative_integer() {
    let result = builtin_json_parse_string(vec![Value::string("-7")]);
    assert!(matches!(result.unwrap(), Value::fixnum(-7)));
}

#[test]
fn parse_float() {
    let result = builtin_json_parse_string(vec![Value::string("3.14")]);
    match result.unwrap().kind() {
        ValueKind::Float /* TODO(tagged): extract float via .xfloat() */ => assert!((f - 3.14).abs() < 1e-10),
        other => panic!("expected float, got {:?}", other),
    }
}

#[test]
fn parse_float_exponent() {
    let result = builtin_json_parse_string(vec![Value::string("1.5e2")]);
    match result.unwrap().kind() {
        ValueKind::Float /* TODO(tagged): extract float via .xfloat() */ => assert!((f - 150.0).abs() < 1e-10),
        other => panic!("expected float, got {:?}", other),
    }
}

#[test]
fn parse_zero() {
    let result = builtin_json_parse_string(vec![Value::string("0")]);
    assert!(matches!(result.unwrap(), Value::fixnum(0)));
}

#[test]
fn parse_string_simple() {
    let result = builtin_json_parse_string(vec![Value::string("\"hello\"")]);
    assert_eq!(result.unwrap().as_str(), Some("hello"));
}

#[test]
fn parse_string_with_escapes() {
    let result = builtin_json_parse_string(vec![Value::string("\"a\\\"b\\\\c\\nd\"")]);
    assert_eq!(result.unwrap().as_str(), Some("a\"b\\c\nd"));
}

#[test]
fn parse_string_unicode_escape() {
    let result = builtin_json_parse_string(vec![Value::string("\"\\u0041\"")]);
    assert_eq!(result.unwrap().as_str(), Some("A"));
}

#[test]
fn parse_string_surrogate_pair() {
    // U+1F600 (grinning face) = \uD83D\uDE00
    let result = builtin_json_parse_string(vec![Value::string("\"\\uD83D\\uDE00\"")]);
    let val = result.unwrap();
    assert_eq!(val.as_str(), Some("\u{1F600}"));
}

#[test]
fn parse_empty_array() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);

    let result = builtin_json_parse_string(vec![Value::string("[]")]);
    match result.unwrap().kind() {
        ValueKind::Veclike(VecLikeType::Vector) => assert!(with_heap(|h| h.get_vector(v).is_empty())),
        other => panic!("expected vector, got {:?}", other),
    }
}

#[test]
fn parse_array() {
    let result = builtin_json_parse_string(vec![Value::string("[1, 2, 3]")]);
    match result.unwrap().kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.len(), 3);
            assert!(matches!(items[0], ValueKind::Fixnum(1)));
            assert!(matches!(items[1], ValueKind::Fixnum(2)));
            assert!(matches!(items[2], ValueKind::Fixnum(3)));
        }
        other => panic!("expected vector, got {:?}", other),
    }
}

#[test]
fn parse_array_as_list() {
    let result = builtin_json_parse_string(vec![
        Value::string("[1, 2]"),
        Value::keyword(intern(":array-type")),
        Value::symbol("list"),
    ]);
    let val = result.unwrap();
    let items = list_to_vec(&val).expect("should be a list");
    assert_eq!(items.len(), 2);
    assert!(matches!(items[0], Value::fixnum(1)));
    assert!(matches!(items[1], Value::fixnum(2)));
}

#[test]
fn parse_empty_object() {
    let result = builtin_json_parse_string(vec![Value::string("{}")]);
    match result.unwrap().kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = with_heap(|h| h.get_hash_table(ht).clone());
            assert!(table.data.is_empty());
        }
        other => panic!("expected hash-table, got {:?}", other),
    }
}

#[test]
fn parse_object_hash_table() {
    let result = builtin_json_parse_string(vec![Value::string("{\"a\": 1, \"b\": 2}")]);
    match result.unwrap().kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = with_heap(|h| h.get_hash_table(ht).clone());
            assert_eq!(table.data.len(), 2);
            assert_eq!(table.key_snapshots.len(), 2);
            assert!(matches!(
                table.data.get(&HashKey::from_str("a")),
                Some(ValueKind::Fixnum(1))
            ));
            assert!(matches!(
                table.data.get(&HashKey::from_str("b")),
                Some(ValueKind::Fixnum(2))
            ));
            assert!(matches!(
                table.key_snapshots.get(&HashKey::from_str("a")),
                Some(key) if key.as_str() == Some("a")
            ));
            assert!(matches!(
                table.key_snapshots.get(&HashKey::from_str("b")),
                Some(key) if key.as_str() == Some("b")
            ));
        }
        other => panic!("expected hash-table, got {:?}", other),
    }
}

#[test]
fn parse_object_as_alist() {
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
            let pair = read_cons(*cell);  // TODO(tagged): replace read_cons with cons accessors
            assert_eq!(pair.car, Value::symbol("x"));
            assert!(matches!(pair.cdr, ValueKind::Fixnum(10)));
        }
        other => panic!("expected cons, got {:?}", other),
    }
}

#[test]
fn parse_object_as_plist() {
    let result = builtin_json_parse_string(vec![
        Value::string("{\"key\": 42}"),
        Value::keyword(intern(":object-type")),
        Value::symbol("plist"),
    ]);
    let val = result.unwrap();
    let items = list_to_vec(&val).expect("should be a list");
    assert_eq!(items.len(), 2);
    assert!(items[0].as_keyword_id().map_or(false, |k| resolve_sym(k) == ":key"));
    assert!(matches!(items[1], Value::fixnum(42)));
}

#[test]
fn parse_nested() {
    let json = r#"{"arr": [1, {"nested": true}], "val": null}"#;
    let result = builtin_json_parse_string(vec![Value::string(json)]);
    assert!(result.is_ok());
}

#[test]
fn parse_custom_null_object() {
    let result = builtin_json_parse_string(vec![
        Value::string("null"),
        Value::keyword(intern(":null-object")),
        Value::NIL,
    ]);
    assert!(matches!(result.unwrap(), Value::NIL));
}

#[test]
fn parse_custom_false_object() {
    let result = builtin_json_parse_string(vec![
        Value::string("false"),
        Value::keyword(intern(":false-object")),
        Value::keyword(intern(":json-false")),
    ]);
    let val = result.unwrap();
    assert!(val.as_keyword_id().map_or(false, |k| resolve_sym(k) == ":json-false"));
}

#[test]
fn parse_trailing_content_error() {
    let result = builtin_json_parse_string(vec![Value::string("42 extra")]);
    assert!(result.is_err());
}

#[test]
fn parse_empty_string_error() {
    match builtin_json_parse_string(vec![Value::string("")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "json-end-of-file");
        }
        other => panic!("expected json-end-of-file signal, got {:?}", other),
    }
}

#[test]
fn parse_invalid_json_error() {
    let result = builtin_json_parse_string(vec![Value::string("{bad}")]);
    assert!(result.is_err());
}

#[test]
fn parse_wrong_type_argument() {
    let result = builtin_json_parse_string(vec![Value::fixnum(42)]);
    assert!(result.is_err());
}

#[test]
fn parse_no_args() {
    let result = builtin_json_parse_string(vec![]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// Round-trip tests
// -----------------------------------------------------------------------

#[test]
fn round_trip_integer() {
    let serialized = builtin_json_serialize(vec![Value::fixnum(123)]).unwrap();
    let parsed = builtin_json_parse_string(vec![serialized]).unwrap();
    assert!(matches!(parsed, Value::fixnum(123)));
}

#[test]
fn round_trip_string() {
    let original = Value::string("hello \"world\"\ntest");
    let serialized = builtin_json_serialize(vec![original]).unwrap();
    let parsed = builtin_json_parse_string(vec![serialized]).unwrap();
    assert_eq!(parsed.as_str(), Some("hello \"world\"\ntest"));
}

#[test]
fn round_trip_array() {
    let original = Value::vector(vec![Value::fixnum(1), Value::string("two"), Value::T]);
    let serialized = builtin_json_serialize(vec![original]).unwrap();
    let parsed = builtin_json_parse_string(vec![serialized]).unwrap();
    match parsed.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.len(), 3);
            assert!(matches!(items[0], ValueKind::Fixnum(1)));
            assert_eq!(items[1].as_str(), Some("two"));
            assert!(matches!(items[2], ValueKind::T));
        }
        _ => panic!("expected vector"),
    }
}

#[test]
fn round_trip_object() {
    let ht = Value::hash_table(HashTableTest::Equal);
    if let Value::HashTable(ref table_arc) /* TODO(tagged): convert Value::HashTable to new API */ = ht {
        with_heap_mut(|h| {
            h.get_hash_table_mut(*table_arc)
                .data
                .insert(HashKey::from_str("key"), Value::fixnum(99));
        });
    }
    let serialized = builtin_json_serialize(vec![ht]).unwrap();
    let parsed = builtin_json_parse_string(vec![serialized]).unwrap();
    match parsed.kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = with_heap(|h| h.get_hash_table(ht).clone());
            assert!(matches!(
                table.data.get(&HashKey::from_str("key")),
                Some(ValueKind::Fixnum(99))
            ));
        }
        _ => panic!("expected hash-table"),
    }
}

// -----------------------------------------------------------------------
// String encoding edge cases
// -----------------------------------------------------------------------

#[test]
fn encode_control_chars() {
    let s = "a\x00b\x01c";
    let encoded = json_encode_string(s);
    assert_eq!(encoded, "\"a\\u0000b\\u0001c\"");
}

#[test]
fn encode_backspace_formfeed() {
    let s = "\x08\x0C";
    let encoded = json_encode_string(s);
    assert_eq!(encoded, "\"\\b\\f\"");
}

#[test]
fn parse_large_number_as_float() {
    // Number too large for i64.
    let result = builtin_json_parse_string(vec![Value::string("99999999999999999999")]);
    match result.unwrap().kind() {
        ValueKind::Float /* TODO(tagged): extract float via .xfloat() */ => {} // OK — fell back to f64
        other => panic!("expected float for large number, got {:?}", other),
    }
}

#[test]
fn serialize_symbol_key_in_alist() {
    let alist = Value::list(vec![Value::cons(
        Value::symbol("name"),
        Value::string("test"),
    )]);
    let result = builtin_json_serialize(vec![alist]);
    assert_eq!(result.unwrap().as_str(), Some("{\"name\":\"test\"}"));
}

#[test]
fn parse_whitespace_around_values() {
    let result = builtin_json_parse_string(vec![Value::string("  {  \"a\"  :  1  }  ")]);
    assert!(result.is_ok());
}

#[test]
fn parse_deeply_nested() {
    let json = "[[[[[[1]]]]]]";
    let result = builtin_json_parse_string(vec![Value::string(json)]);
    assert!(result.is_ok());
}
