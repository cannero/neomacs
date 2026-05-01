use super::*;
use crate::emacs_core::error::Flow;
use crate::emacs_core::value::{Value, get_string_text_properties_for_value};

#[test]
fn ascii_width() {
    crate::test_utils::init_test_tracing();
    assert_eq!(char_width('a'), 1);
    assert_eq!(char_width(' '), 1);
    assert_eq!(char_width('Z'), 1);
}

#[test]
fn cjk_width() {
    crate::test_utils::init_test_tracing();
    assert_eq!(char_width('中'), 2);
    assert_eq!(char_width('日'), 2);
    assert_eq!(char_width('あ'), 2);
    assert_eq!(char_width('ア'), 2);
}

#[test]
fn gnu_default_emoji_symbol_widths() {
    crate::test_utils::init_test_tracing();
    assert_eq!(char_width('\u{2603}'), 1);
    assert_eq!(char_width('\u{2615}'), 2);
    assert_eq!(char_width('\u{263A}'), 1);
}

#[test]
fn control_char_width() {
    crate::test_utils::init_test_tracing();
    assert_eq!(char_width('\0'), 2);
    assert_eq!(char_width('\x01'), 2); // ^A
    assert_eq!(char_width('\n'), 0);
    assert_eq!(char_width('\x7f'), 2); // ^?
    assert_eq!(char_width('\u{0080}'), 4);
    assert_eq!(char_width('\u{009f}'), 4);
}

#[test]
fn string_width_mixed() {
    crate::test_utils::init_test_tracing();
    assert_eq!(string_width("hello"), 5);
    assert_eq!(string_width("中文"), 4);
    assert_eq!(string_width("hi中"), 4);
}

#[test]
fn builtin_string_bytes_counts_utf8_length() {
    crate::test_utils::init_test_tracing();
    let result = builtin_string_bytes(vec![Value::string("Aé中")]).unwrap();
    assert_eq!(result, Value::fixnum(6));
}

#[test]
fn builtin_char_displayable_p_matches_oracle_bounds_and_types() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        builtin_char_displayable_p(vec![Value::fixnum('a' as i64)]).unwrap(),
        Value::T
    );
    assert_eq!(
        builtin_char_displayable_p(vec![Value::fixnum(0x00E9)]).unwrap(),
        Value::symbol("unicode")
    );
    assert_eq!(
        builtin_char_displayable_p(vec![Value::fixnum(0x11_0000)]).unwrap(),
        Value::NIL
    );

    let overflow = builtin_char_displayable_p(vec![Value::fixnum(0x40_0000)])
        .expect_err("overflow char code should signal wrong-type-argument characterp");
    match overflow {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("characterp"), Value::fixnum(0x40_0000)]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let non_number = builtin_char_displayable_p(vec![Value::symbol("x")])
        .expect_err("non-number should signal number-or-marker-p");
    match non_number {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("number-or-marker-p"), Value::symbol("x")]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn builtin_char_width_matches_oracle_control_and_bounds() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        builtin_char_width(vec![Value::fixnum(0)]).unwrap(),
        Value::fixnum(2)
    );
    assert_eq!(
        builtin_char_width(vec![Value::fixnum(9)]).unwrap(),
        Value::fixnum(8)
    );
    assert_eq!(
        builtin_char_width(vec![Value::fixnum(10)]).unwrap(),
        Value::fixnum(0)
    );
    assert_eq!(
        builtin_char_width(vec![Value::fixnum(0x80)]).unwrap(),
        Value::fixnum(4)
    );
    assert_eq!(
        builtin_char_width(vec![Value::fixnum(0x11_0000)]).unwrap(),
        Value::fixnum(1)
    );

    let negative = builtin_char_width(vec![Value::fixnum(-1)])
        .expect_err("negative character code should signal");
    match negative {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("characterp"), Value::fixnum(-1)]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let overflow = builtin_char_width(vec![Value::fixnum(0x40_0000)])
        .expect_err("overflow character code should signal");
    match overflow {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("characterp"), Value::fixnum(0x40_0000)]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn builtin_char_or_string_p_respects_character_bounds() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        builtin_char_or_string_p(vec![Value::fixnum(0)]).unwrap(),
        Value::T
    );
    assert_eq!(
        builtin_char_or_string_p(vec![Value::fixnum(0x3F_FFFF)]).unwrap(),
        Value::T
    );
    assert_eq!(
        builtin_char_or_string_p(vec![Value::fixnum(-1)]).unwrap(),
        Value::NIL
    );
    assert_eq!(
        builtin_char_or_string_p(vec![Value::fixnum(0x40_0000)]).unwrap(),
        Value::NIL
    );
    assert_eq!(
        builtin_char_or_string_p(vec![Value::symbol("x")]).unwrap(),
        Value::NIL
    );
}

#[test]
fn builtin_max_char_optional_unicode_matches_oracle() {
    crate::test_utils::init_test_tracing();
    assert_eq!(builtin_max_char(vec![]).unwrap(), Value::fixnum(0x3F_FFFF));
    assert_eq!(
        builtin_max_char(vec![Value::NIL]).unwrap(),
        Value::fixnum(0x3F_FFFF)
    );
    assert_eq!(
        builtin_max_char(vec![Value::T]).unwrap(),
        Value::fixnum(0x10_FFFF)
    );
    assert_eq!(
        builtin_max_char(vec![Value::symbol("foo")]).unwrap(),
        Value::fixnum(0x10_FFFF)
    );

    let wrong_arity = builtin_max_char(vec![Value::fixnum(1), Value::fixnum(2)])
        .expect_err("max-char should reject more than one argument");
    match wrong_arity {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data, vec![Value::symbol("max-char"), Value::fixnum(2)]);
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn builtin_coding_string_helpers_enforce_max_arity() {
    crate::test_utils::init_test_tracing();
    let encode_over = builtin_encode_coding_string(vec![
        Value::string("a"),
        Value::symbol("utf-8"),
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ])
    .expect_err("encode-coding-string should reject more than four arguments");
    match encode_over {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("encode-coding-string"), Value::fixnum(5)]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let decode_over = builtin_decode_coding_string(vec![
        Value::string("a"),
        Value::symbol("utf-8"),
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ])
    .expect_err("decode-coding-string should reject more than four arguments");
    match decode_over {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("decode-coding-string"), Value::fixnum(5)]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }
}

#[test]
fn builtin_coding_string_helpers_runtime_match_oracle_core_cases() {
    crate::test_utils::init_test_tracing();
    let encoded = builtin_encode_coding_string(vec![Value::string("é"), Value::symbol("utf-8")])
        .expect("encode-coding-string should evaluate");
    let ls = encoded
        .as_lisp_string()
        .expect("encode-coding-string should return a string");
    assert_eq!(ls.as_bytes(), &[0xC3, 0xA9]);

    let decode_utf8 =
        builtin_decode_coding_string(vec![Value::string("é"), Value::symbol("utf-8")])
            .expect("decode-coding-string should evaluate");
    assert_eq!(decode_utf8, Value::string("é"));

    let nil_encode =
        builtin_encode_coding_string(vec![Value::string("é"), Value::NIL]).expect("nil coding");
    assert_eq!(nil_encode, Value::string("é"));

    let nil_decode =
        builtin_decode_coding_string(vec![Value::string("é"), Value::NIL]).expect("nil coding");
    assert_eq!(nil_decode, Value::string("é"));

    let coding_string =
        builtin_encode_coding_string(vec![Value::string("a"), Value::string("utf-8")])
            .expect_err("string coding-system should signal symbolp");
    match coding_string {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("symbolp"), Value::string("utf-8")]
            );
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let unknown_encode =
        builtin_encode_coding_string(vec![Value::string("a"), Value::symbol("vm-no-such-coding")])
            .expect_err("unknown coding-system should signal coding-system-error");
    match unknown_encode {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "coding-system-error");
            assert_eq!(sig.data, vec![Value::symbol("vm-no-such-coding")]);
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let unknown_decode =
        builtin_decode_coding_string(vec![Value::string("a"), Value::symbol("vm-no-such-coding")])
            .expect_err("unknown coding-system should signal coding-system-error");
    match unknown_decode {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "coding-system-error");
            assert_eq!(sig.data, vec![Value::symbol("vm-no-such-coding")]);
        }
        other => panic!("expected signal, got: {other:?}"),
    }

    let unibyte_val = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xE9]));
    let decoded_unibyte = builtin_decode_coding_string(vec![unibyte_val, Value::symbol("utf-8")])
        .expect("decode-coding-string should preserve invalid bytes");
    let decoded_ls = decoded_unibyte
        .as_lisp_string()
        .expect("decode-coding-string should return string");
    // 0xE9 is invalid UTF-8, so it becomes raw-byte char 0x3FFF00 + 0xE9
    let codes: Vec<u32> = crate::emacs_core::builtins::lisp_string_char_codes(decoded_ls);
    assert_eq!(codes, vec![0x3FFF00 + 0xE9]);

    let unibyte_val2 = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xE9]));
    let encoded_unibyte = builtin_encode_coding_string(vec![unibyte_val2, Value::symbol("utf-8")])
        .expect("encode-coding-string should preserve unibyte bytes");
    let encoded_ls = encoded_unibyte.as_lisp_string().unwrap();
    assert_eq!(encoded_ls.as_bytes(), &[0xE9]);
}

#[test]
fn nil_coding_string_respects_nocopy_identity() {
    crate::test_utils::init_test_tracing();
    let source = Value::string("abc");

    let encoded_copy =
        builtin_encode_coding_string(vec![source, Value::NIL, Value::NIL]).expect("nil coding");
    assert_eq!(encoded_copy.as_utf8_str(), Some("abc"));
    assert!(!crate::emacs_core::value::eq_value(&source, &encoded_copy));

    let encoded_nocopy =
        builtin_encode_coding_string(vec![source, Value::NIL, Value::T]).expect("nil coding");
    assert!(crate::emacs_core::value::eq_value(&source, &encoded_nocopy));

    let decoded_copy =
        builtin_decode_coding_string(vec![source, Value::NIL, Value::NIL]).expect("nil coding");
    assert_eq!(decoded_copy.as_utf8_str(), Some("abc"));
    assert!(!crate::emacs_core::value::eq_value(&source, &decoded_copy));

    let decoded_nocopy =
        builtin_decode_coding_string(vec![source, Value::NIL, Value::T]).expect("nil coding");
    assert!(crate::emacs_core::value::eq_value(&source, &decoded_nocopy));
}

#[test]
fn ascii_coding_string_respects_nocopy_fast_path_identity() {
    crate::test_utils::init_test_tracing();
    let source = Value::string("abc");

    let encoded_copy =
        builtin_encode_coding_string(vec![source, Value::symbol("utf-8"), Value::NIL])
            .expect("utf-8 encode");
    assert_eq!(encoded_copy.as_utf8_str(), Some("abc"));
    assert!(!crate::emacs_core::value::eq_value(&source, &encoded_copy));

    let encoded_nocopy =
        builtin_encode_coding_string(vec![source, Value::symbol("utf-8"), Value::T])
            .expect("utf-8 encode");
    assert!(crate::emacs_core::value::eq_value(&source, &encoded_nocopy));

    let decoded_copy =
        builtin_decode_coding_string(vec![source, Value::symbol("utf-8"), Value::NIL])
            .expect("utf-8 decode");
    assert_eq!(decoded_copy.as_utf8_str(), Some("abc"));
    assert!(!crate::emacs_core::value::eq_value(&source, &decoded_copy));

    let decoded_nocopy =
        builtin_decode_coding_string(vec![source, Value::symbol("utf-8"), Value::T])
            .expect("utf-8 decode");
    assert!(crate::emacs_core::value::eq_value(&source, &decoded_nocopy));
}

#[test]
fn ascii_coding_string_nocopy_allocates_when_eol_conversion_needed() {
    crate::test_utils::init_test_tracing();
    let encode_source = Value::string("a\nb");
    let encoded =
        builtin_encode_coding_string(vec![encode_source, Value::symbol("utf-8-dos"), Value::T])
            .expect("utf-8-dos encode");
    assert_eq!(encoded.as_lisp_string().unwrap().as_bytes(), b"a\r\nb");
    assert!(!crate::emacs_core::value::eq_value(
        &encode_source,
        &encoded
    ));

    let decode_source = Value::heap_string(crate::heap_types::LispString::from_unibyte(
        b"a\r\nb".to_vec(),
    ));
    let decoded =
        builtin_decode_coding_string(vec![decode_source, Value::symbol("utf-8-dos"), Value::T])
            .expect("utf-8-dos decode");
    assert_eq!(decoded.as_utf8_str(), Some("a\nb"));
    assert!(!crate::emacs_core::value::eq_value(
        &decode_source,
        &decoded
    ));
}

#[test]
fn encode_coding_string_buffer_destination_inserts_without_moving_point() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let dest = eval.buffers.create_buffer("*encode-coding-string-dest*");
    eval.buffers
        .insert_lisp_string_into_buffer(dest, &crate::heap_types::LispString::from_utf8("XY"))
        .expect("insert destination seed");
    eval.buffers
        .goto_buffer_byte(dest, 1)
        .expect("move destination point");

    let produced = builtin_encode_coding_string_in_context(
        &mut eval,
        vec![
            Value::string("a\n"),
            Value::symbol("utf-8-dos"),
            Value::NIL,
            Value::make_buffer(dest),
        ],
    )
    .expect("encode-coding-string should insert in destination buffer");

    assert_eq!(produced, Value::fixnum(3));
    assert_eq!(
        eval.visible_variable_value_or_nil("last-coding-system-used"),
        Value::symbol("utf-8-dos")
    );
    let buf = eval.buffers.get(dest).expect("destination buffer");
    assert_eq!(buf.buffer_string(), "Xa\r\nY");
    assert_eq!(buf.point_byte(), 1);
}

#[test]
fn decode_coding_string_buffer_destination_inserts_without_moving_point() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let dest = eval.buffers.create_buffer("*decode-coding-string-dest*");
    let encoded = Value::heap_string(crate::heap_types::LispString::from_unibyte(
        b"a\r\nb".to_vec(),
    ));

    let produced = builtin_decode_coding_string_in_context(
        &mut eval,
        vec![
            encoded,
            Value::symbol("utf-8-dos"),
            Value::NIL,
            Value::make_buffer(dest),
        ],
    )
    .expect("decode-coding-string should insert in destination buffer");

    assert_eq!(produced, Value::fixnum(3));
    assert_eq!(
        eval.visible_variable_value_or_nil("last-coding-system-used"),
        Value::symbol("utf-8-dos")
    );
    let buf = eval.buffers.get(dest).expect("destination buffer");
    assert_eq!(buf.buffer_string(), "a\nb");
    assert_eq!(buf.point_byte(), 0);
}

#[test]
fn builtin_coding_string_helpers_accept_iso_8859_15_alias() {
    crate::test_utils::init_test_tracing();
    let encoded =
        builtin_encode_coding_string(vec![Value::string("abc"), Value::symbol("iso-8859-15")])
            .expect("iso-8859-15 should be accepted as a known coding system");
    assert_eq!(encoded.as_utf8_str(), Some("abc"));

    let decoded =
        builtin_decode_coding_string(vec![Value::string("abc"), Value::symbol("iso-8859-15")])
            .expect("iso-8859-15 should be accepted as a known coding system");
    assert_eq!(decoded.as_utf8_str(), Some("abc"));
}

#[test]
fn encode_lisp_string_emacs_internal_uses_utf8_emacs_alias() {
    crate::test_utils::init_test_tracing();
    let text = crate::heap_types::LispString::from_utf8("abc\n");

    assert_eq!(encode_lisp_string(&text, "emacs-internal"), b"abc\n");
    assert_eq!(encode_lisp_string(&text, "emacs-internal-dos"), b"abc\r\n");
}

#[test]
fn builtin_coding_string_helpers_accept_iso_8859_9_alias() {
    crate::test_utils::init_test_tracing();
    let encoded =
        builtin_encode_coding_string(vec![Value::string("abc"), Value::symbol("iso-8859-9")])
            .expect("iso-8859-9 should be accepted as a known coding system");
    assert_eq!(encoded.as_utf8_str(), Some("abc"));

    let decoded =
        builtin_decode_coding_string(vec![Value::string("abc"), Value::symbol("iso-8859-9")])
            .expect("iso-8859-9 should be accepted as a known coding system");
    assert_eq!(decoded.as_utf8_str(), Some("abc"));
}

#[test]
fn decode_latin1_attaches_charset_text_property() {
    crate::test_utils::init_test_tracing();
    let encoded = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xE9]));
    let decoded = builtin_decode_coding_string(vec![encoded, Value::symbol("latin-1")])
        .expect("latin-1 decode should succeed");
    if !decoded.is_string() {
        panic!("decode-coding-string should return a string");
    };
    let props = get_string_text_properties_for_value(decoded)
        .expect("decoded string should be propertized");
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].start, 0);
    assert_eq!(props[0].end, 1);
    assert_eq!(
        props[0].plist,
        Value::list(vec![Value::symbol("charset"), Value::symbol("iso-8859-1")])
    );
}

#[test]
fn decode_latin1_charset_property_spans_ascii_like_gnu() {
    crate::test_utils::init_test_tracing();
    let encoded = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
        b'A', 0xE9, b'B',
    ]));
    let decoded = builtin_decode_coding_string(vec![encoded, Value::symbol("latin-1")])
        .expect("latin-1 decode should succeed");

    assert_eq!(
        decoded.as_lisp_string().and_then(|s| s.as_utf8_str()),
        Some("AéB")
    );
    let props = get_string_text_properties_for_value(decoded)
        .expect("decoded Latin-1 string should be propertized");
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].start, 0);
    assert_eq!(props[0].end, 3);
    assert_eq!(
        props[0].plist,
        Value::list(vec![Value::symbol("charset"), Value::symbol("iso-8859-1")])
    );
}

#[test]
fn encode_no_conversion_preserves_unibyte_storage_bytes() {
    crate::test_utils::init_test_tracing();
    let source = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xE9]));
    let encoded =
        builtin_encode_coding_string(vec![source, Value::symbol("no-conversion")]).unwrap();
    if !encoded.is_string() {
        panic!("encode-coding-string should return a string");
    };
    assert!(!encoded.string_is_multibyte());
    let ls = encoded.as_lisp_string().unwrap();
    assert_eq!(ls.as_bytes(), &[0xE9]);
}

#[test]
fn decode_no_conversion_returns_unibyte_bytes_for_non_ascii_input() {
    crate::test_utils::init_test_tracing();
    let encoded =
        builtin_encode_coding_string(vec![Value::string("é"), Value::symbol("no-conversion")])
            .expect("encoding should succeed");
    let decoded =
        builtin_decode_coding_string(vec![encoded, Value::symbol("no-conversion")]).unwrap();
    if !decoded.is_string() {
        panic!("decode-coding-string should return a string");
    };
    assert!(!decoded.string_is_multibyte());
    let ls = decoded.as_lisp_string().unwrap();
    assert_eq!(ls.as_bytes(), &[0xC3, 0xA9]);
}

#[test]
fn char_byte_conversion() {
    crate::test_utils::init_test_tracing();
    let s = "hello中文";
    assert_eq!(char_to_byte_pos(s, 5), 5);
    assert_eq!(char_to_byte_pos(s, 6), 8); // '中' is 3 bytes
    assert_eq!(byte_to_char_pos(s, 5), 5);
    assert_eq!(byte_to_char_pos(s, 8), 6);
}

#[test]
fn encoding_utf8() {
    crate::test_utils::init_test_tracing();
    let bytes = encode_string("hello", "utf-8");
    assert_eq!(bytes, b"hello");
    let decoded = decode_bytes(b"hello", "utf-8");
    assert_eq!(decoded, "hello");
}

#[test]
fn encoding_utf16_gnu_compatible_signatures_and_endianness() {
    crate::test_utils::init_test_tracing();
    assert_eq!(encode_string("a", "utf-16"), vec![0xfe, 0xff, 0x00, 0x61]);
    assert_eq!(
        encode_string("a", "utf-16-be"),
        vec![0xfe, 0xff, 0x00, 0x61]
    );
    assert_eq!(encode_string("a", "utf-16be"), vec![0x00, 0x61]);
    assert_eq!(
        encode_string("a", "utf-16-le"),
        vec![0xff, 0xfe, 0x61, 0x00]
    );
    assert_eq!(encode_string("a", "utf-16le"), vec![0x61, 0x00]);

    assert_eq!(decode_bytes(&[0x00, 0x61], "utf-16be"), "a");
    assert_eq!(decode_bytes(&[0x61, 0x00], "utf-16le"), "a");
    assert_eq!(
        decode_bytes(&[0xff, 0xfe, 0x3d, 0xd8, 0x00, 0xde], "utf-16-be"),
        "\u{1f600}"
    );

    let encoded =
        builtin_encode_coding_string(vec![Value::string("a"), Value::symbol("utf-16-be")])
            .expect("utf-16-be should be a valid coding system");
    let encoded_string = encoded
        .as_lisp_string()
        .expect("encode-coding-string should return a string");
    assert_eq!(encoded_string.as_bytes(), &[0xfe, 0xff, 0x00, 0x61]);
}

#[test]
fn encoding_utf8_dos_applies_eol_conversion() {
    crate::test_utils::init_test_tracing();
    let bytes = encode_string("a\nb", "utf-8-dos");
    assert_eq!(bytes, b"a\r\nb");
    let decoded = decode_bytes(b"a\r\nb", "utf-8-dos");
    assert_eq!(decoded, "a\nb");
}

#[test]
fn raw_text_dos_preserves_bytes_but_converts_eol() {
    crate::test_utils::init_test_tracing();
    let encoded =
        builtin_encode_coding_string(vec![Value::string("a\nb"), Value::symbol("raw-text-dos")])
            .unwrap();
    if !encoded.is_string() {
        panic!("encode-coding-string should return a string");
    };
    let ls = encoded.as_lisp_string().unwrap();
    assert_eq!(ls.as_bytes(), b"a\r\nb");

    let decoded = builtin_decode_coding_string(vec![
        Value::heap_string(crate::heap_types::LispString::from_unibyte(
            b"a\r\nb".to_vec(),
        )),
        Value::symbol("raw-text-dos"),
    ])
    .unwrap();
    if !decoded.is_string() {
        panic!("decode-coding-string should return a string");
    };
    let ls = decoded.as_lisp_string().unwrap();
    assert_eq!(ls.as_bytes(), b"a\nb");
}

#[test]
fn encode_coding_region_destination_t_returns_encoded_string() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let current = eval.buffers.current_buffer_id().expect("current buffer");
    eval.buffers
        .insert_lisp_string_into_buffer(current, &crate::heap_types::LispString::from_utf8("é"))
        .expect("insert source text");

    let encoded = builtin_encode_coding_region(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("utf-8"),
            Value::T,
        ],
    )
    .expect("encode-coding-region should return a string destination");
    let encoded = encoded.as_lisp_string().expect("encoded string");
    assert!(!encoded.is_multibyte());
    assert_eq!(encoded.as_bytes(), &[0xC3, 0xA9]);
    assert_eq!(
        eval.visible_variable_value_or_nil("last-coding-system-used"),
        Value::symbol("utf-8")
    );

    let buffer_text = eval
        .buffers
        .get(current)
        .expect("current buffer")
        .buffer_string();
    assert_eq!(buffer_text, "é");
}

#[test]
fn decode_coding_region_replaces_current_region() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let current = eval.buffers.current_buffer_id().expect("current buffer");
    eval.buffers
        .insert_lisp_string_into_buffer(
            current,
            &crate::heap_types::LispString::from_utf8("a\r\nb"),
        )
        .expect("insert encoded bytes");

    let produced = builtin_decode_coding_region(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(5),
            Value::symbol("utf-8-dos"),
            Value::NIL,
        ],
    )
    .expect("decode-coding-region should replace the region");
    assert_eq!(produced, Value::fixnum(3));
    assert_eq!(
        eval.visible_variable_value_or_nil("last-coding-system-used"),
        Value::symbol("utf-8-dos")
    );

    let buffer = eval.buffers.get(current).expect("current buffer");
    assert_eq!(buffer.buffer_string(), "a\nb");
    assert_eq!(buffer.point_min_char(), 0);
    assert_eq!(buffer.point_max_char(), 3);
}

#[test]
fn undecided_write_encoding_preserves_bytes_and_converts_eol() {
    crate::test_utils::init_test_tracing();

    let encoded = builtin_encode_coding_string(vec![
        Value::string("alpha\nomega"),
        Value::symbol("undecided-unix"),
    ])
    .unwrap();
    let ls = encoded
        .as_lisp_string()
        .expect("encode-coding-string should return a string");
    assert_eq!(ls.as_bytes(), b"alpha\nomega");

    let encoded = builtin_encode_coding_string(vec![
        Value::string("alpha\nomega"),
        Value::symbol("undecided-dos"),
    ])
    .unwrap();
    let ls = encoded
        .as_lisp_string()
        .expect("encode-coding-string should return a string");
    assert_eq!(ls.as_bytes(), b"alpha\r\nomega");
}

#[test]
fn encoding_latin1() {
    crate::test_utils::init_test_tracing();
    let bytes = encode_string("café", "latin-1");
    assert_eq!(bytes.len(), 4); // é maps to 0xe9
    let decoded = decode_bytes(&[0x63, 0x61, 0x66, 0xe9], "latin-1");
    assert_eq!(decoded, "café");
}

#[test]
fn encoding_big5_decodes_generated_leim_dictionary_bytes_like_gnu() {
    crate::test_utils::init_test_tracing();
    assert_eq!(decode_bytes(&[0xa4, 0x40], "big5"), "\u{4e00}");
    assert_eq!(decode_bytes(&[0xa4, 0x40], "chinese-big5-unix"), "\u{4e00}");
    assert_eq!(decode_bytes(&[0xa4, 0x40], "cp950"), "\u{4e00}");
    assert_eq!(
        decode_bytes(&[0xa4, 0x40, b'\r', b'\n'], "big5-dos"),
        "一\n"
    );
    assert_eq!(encode_string("一", "big5"), vec![0xa4, 0x40]);
    assert_eq!(encode_string("一", "chinese-big5-unix"), vec![0xa4, 0x40]);
}

#[test]
fn encoding_gb2312_decodes_generated_leim_dictionary_bytes_like_gnu() {
    crate::test_utils::init_test_tracing();
    assert_eq!(decode_bytes(&[0xd2, 0xbb], "cn-gb-2312"), "一");
    assert_eq!(decode_bytes(&[0xd2, 0xbb], "chinese-iso-8bit-unix"), "一");
    assert_eq!(
        decode_bytes(&[0xd2, 0xbb, b'\r', b'\n'], "gb2312-dos"),
        "一\n"
    );
    assert_eq!(encode_string("一", "cn-gb-2312"), vec![0xd2, 0xbb]);
    assert_eq!(
        encode_string("一", "chinese-iso-8bit-unix"),
        vec![0xd2, 0xbb]
    );
}

#[test]
fn decode_coding_string_big5_marks_charset_like_gnu() {
    crate::test_utils::init_test_tracing();
    let decoded = builtin_decode_coding_string(vec![
        Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
            0xa4, 0x40,
        ])),
        Value::symbol("chinese-big5-unix"),
    ])
    .expect("decode-coding-string chinese-big5-unix should succeed");

    assert_eq!(
        decoded.as_lisp_string().and_then(|s| s.as_utf8_str()),
        Some("一")
    );
    let props = get_string_text_properties_for_value(decoded)
        .expect("decoded Big5 string should be propertized");
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].start, 0);
    assert_eq!(props[0].end, 1);
    assert_eq!(
        props[0].plist,
        Value::list(vec![Value::symbol("charset"), Value::symbol("big5")])
    );
}

#[test]
fn decode_coding_string_gb2312_marks_charset_like_gnu() {
    crate::test_utils::init_test_tracing();
    let decoded = builtin_decode_coding_string(vec![
        Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
            0xd2, 0xbb,
        ])),
        Value::symbol("cn-gb-2312-unix"),
    ])
    .expect("decode-coding-string cn-gb-2312-unix should succeed");

    assert_eq!(
        decoded.as_lisp_string().and_then(|s| s.as_utf8_str()),
        Some("一")
    );
    let props = get_string_text_properties_for_value(decoded)
        .expect("decoded GB2312 string should be propertized");
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].start, 0);
    assert_eq!(props[0].end, 1);
    assert_eq!(
        props[0].plist,
        Value::list(vec![
            Value::symbol("charset"),
            Value::symbol("chinese-gb2312"),
        ])
    );
}

#[test]
fn decode_coding_string_gb2312_extends_charset_after_first_non_ascii_like_gnu() {
    crate::test_utils::init_test_tracing();
    let decoded = builtin_decode_coding_string(vec![
        Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
            b'A', 0xd2, 0xbb, b'B',
        ])),
        Value::symbol("cn-gb-2312-unix"),
    ])
    .expect("decode-coding-string cn-gb-2312-unix should succeed");

    assert_eq!(
        decoded.as_lisp_string().and_then(|s| s.as_utf8_str()),
        Some("A一B")
    );
    let props = get_string_text_properties_for_value(decoded)
        .expect("decoded GB2312 string should be propertized");
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].start, 1);
    assert_eq!(props[0].end, 3);
    assert_eq!(
        props[0].plist,
        Value::list(vec![
            Value::symbol("charset"),
            Value::symbol("chinese-gb2312"),
        ])
    );
}

#[test]
fn glyphless_display() {
    crate::test_utils::init_test_tracing();
    assert_eq!(glyphless_char_display('\x01'), "^A");
    assert_eq!(glyphless_char_display('\x7f'), "^?");
    assert_eq!(glyphless_char_display('\u{FEFF}'), "\\uFEFF");
}

#[test]
fn multibyte_detection() {
    crate::test_utils::init_test_tracing();
    assert!(!is_multibyte_string("hello"));
    assert!(is_multibyte_string("héllo"));
    assert!(is_multibyte_string("中文"));
}

#[test]
fn multibyte_detection_treats_unibyte_storage_as_unibyte() {
    crate::test_utils::init_test_tracing();
    assert!(!is_multibyte_string("abc"));
    // Pure ASCII is not multibyte
    assert!(!is_multibyte_string("hello"));
}

#[test]
fn builtin_multibyte_string_p_matches_oracle_non_string_and_unibyte_storage() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        builtin_multibyte_string_p(vec![Value::string("abc")]).unwrap(),
        Value::NIL
    );
    assert_eq!(
        builtin_multibyte_string_p(vec![Value::string("é")]).unwrap(),
        Value::T
    );

    let unibyte_val =
        Value::heap_string(crate::heap_types::LispString::from_unibyte(b"abc".to_vec()));
    assert_eq!(
        builtin_multibyte_string_p(vec![unibyte_val]).unwrap(),
        Value::NIL
    );

    assert_eq!(
        builtin_multibyte_string_p(vec![Value::fixnum(1)]).unwrap(),
        Value::NIL
    );
}

#[test]
fn builtin_unibyte_string_p_basics() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        builtin_unibyte_string_p(vec![Value::string("hello")]).unwrap(),
        Value::T
    );
    assert_eq!(
        builtin_unibyte_string_p(vec![Value::string("héllo")]).unwrap(),
        Value::NIL
    );
}

#[test]
fn builtin_unibyte_string_p_errors() {
    crate::test_utils::init_test_tracing();
    // Wrong arity signals error.
    assert!(builtin_unibyte_string_p(vec![]).is_err());
    // Non-string arg returns nil (type predicates don't error on wrong type).
    assert_eq!(
        builtin_unibyte_string_p(vec![Value::fixnum(1)]).unwrap(),
        Value::NIL
    );
}

#[test]
fn printable_check() {
    crate::test_utils::init_test_tracing();
    assert!(is_printable('a'));
    assert!(is_printable('中'));
    assert!(!is_printable('\x00'));
    assert!(!is_printable('\x7f'));
}
