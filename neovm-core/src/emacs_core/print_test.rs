use super::super::intern::{intern, intern_uninterned};
use super::super::marker::make_marker_value;
use super::*;
use crate::emacs_core::value::{
    HashTableTest, LambdaData, LambdaParams, StringTextPropertyRun, next_float_id,
};

#[test]
fn print_basic_values() {
    assert_eq!(print_value(&Value::NIL), "nil");
    assert_eq!(print_value(&Value::T), "t");
    assert_eq!(print_value(&Value::fixnum(42)), "42");
    assert_eq!(print_value(&Value::make_float(3.14)), "3.14");
    assert_eq!(print_value(&Value::make_float(1.0)), "1.0");
    assert_eq!(print_value(&Value::symbol("foo")), "foo");
    assert_eq!(print_value(&Value::symbol(".foo")), "\\.foo");
    assert_eq!(print_value(&Value::symbol("")), "##");
    assert_eq!(print_value(&Value::keyword(":bar")), ":bar");
}

#[test]
fn print_symbol_escapes_reader_sensitive_chars() {
    assert_eq!(print_value(&Value::symbol("a b")), "a\\ b");
    assert_eq!(print_value(&Value::symbol("a,b")), "a\\,b");
    assert_eq!(print_value(&Value::symbol("a,@b")), "a\\,@b");
    assert_eq!(print_value(&Value::symbol("a#b")), "a\\#b");
    assert_eq!(print_value(&Value::symbol("a'b")), "a\\'b");
    assert_eq!(print_value(&Value::symbol("a`b")), "a\\`b");
    assert_eq!(print_value(&Value::symbol("a\\b")), "a\\\\b");
    assert_eq!(print_value(&Value::symbol("a\"b")), "a\\\"b");
    assert_eq!(print_value(&Value::symbol("a(b")), "a\\(b");
    assert_eq!(print_value(&Value::symbol("a)b")), "a\\)b");
    assert_eq!(print_value(&Value::symbol("a[b")), "a\\[b");
    assert_eq!(print_value(&Value::symbol("a]b")), "a\\]b");
    assert_eq!(print_value(&Value::symbol("##")), "\\#\\#");
    assert_eq!(print_value(&Value::symbol("?a")), "\\?a");
    assert_eq!(print_value(&Value::symbol("a?b")), "a?b");
}

#[test]
fn print_uninterned_symbols_follow_gnu_default_print_gensym_nil() {
    assert_eq!(print_value(&Value::symbol(intern_uninterned("foo"))), "foo");
    assert_eq!(
        print_value(&Value::symbol(intern_uninterned(":foo"))),
        ":foo"
    );
    assert_eq!(print_value(&Value::symbol(intern_uninterned(""))), "##");
}

#[test]
fn print_uninterned_symbols_support_print_gensym_round_trip_syntax() {
    let options = PrintOptions::with_print_gensym(true);
    assert_eq!(
        print_value_with_options(&Value::symbol(intern_uninterned("foo")), options),
        "#:foo"
    );
    assert_eq!(
        print_value_with_options(&Value::symbol(intern_uninterned(":foo")), options),
        "#::foo"
    );
    assert_eq!(
        print_value_with_options(&Value::symbol(intern_uninterned("")), options),
        "#:"
    );
}

#[test]
fn print_float_nan_preserves_sign() {
    assert_eq!(print_value(&Value::make_float(f64::NAN)), "0.0e+NaN");
    let neg_nan = f64::from_bits(f64::NAN.to_bits() | (1_u64 << 63));
    assert_eq!(print_value(&Value::make_float(neg_nan)), "-0.0e+NaN");
}

#[test]
fn print_float_nan_payload_tag_round_trip_shape() {
    let tagged = f64::from_bits((0x7ffu64 << 52) | (1u64 << 51) | 1u64);
    assert_eq!(print_value(&Value::make_float(tagged)), "1.0e+NaN");

    let neg_tagged = f64::from_bits((1u64 << 63) | (0x7ffu64 << 52) | (1u64 << 51) | 2u64);
    assert_eq!(print_value(&Value::make_float(neg_tagged)), "-2.0e+NaN");
}

#[test]
fn print_string() {
    assert_eq!(print_value(&Value::string("hello")), "\"hello\"");
}

#[test]
fn print_empty_char_table_uses_gnu_vector_shape() {
    let table = crate::emacs_core::chartable::make_char_table_with_extra_slots(
        Value::symbol("syntax-table"),
        Value::NIL,
        0,
    );
    let rendered = print_value(&table);
    assert!(rendered.starts_with("#^[nil nil syntax-table"));
}

#[test]
fn print_propertized_string_literal_shape() {
    let value = Value::string_with_text_properties(
        " ",
        vec![StringTextPropertyRun {
            start: 0,
            end: 1,
            plist: Value::list(vec![
                Value::symbol("display"),
                Value::list(vec![
                    Value::symbol("space"),
                    Value::keyword(":align-to"),
                    Value::list(vec![
                        Value::symbol("+"),
                        Value::symbol("header-line-indent-width"),
                        Value::fixnum(0),
                    ]),
                ]),
            ]),
        }],
    );
    assert_eq!(
        print_value(&value),
        r##"#(" " 0 1 (display (space :align-to (+ header-line-indent-width 0))))"##
    );
    assert_eq!(
        print_value_bytes(&value),
        br#"#(" " 0 1 (display (space :align-to (+ header-line-indent-width 0))))"#
    );
}

#[test]
fn print_string_keeps_non_bmp_visible() {
    assert_eq!(print_value(&Value::string("\u{10ffff}")), "\"\u{10ffff}\"");
}

#[test]
fn print_string_bytes_preserve_non_utf8_payloads() {
    let raw = char::from_u32(0xE0FF).expect("raw-byte sentinel");
    assert_eq!(
        print_value_bytes(&Value::string(raw.to_string())),
        b"\"\\377\""
    );
}

#[test]
fn print_list() {
    let lst = Value::list(vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
    assert_eq!(print_value(&lst), "(1 2 3)");
}

#[test]
fn print_hash_s_literal_shorthand() {
    let literal = Value::list(vec![
        Value::symbol("make-hash-table-from-literal"),
        Value::list(vec![
            Value::symbol("quote"),
            Value::list(vec![Value::symbol("x")]),
        ]),
    ]);
    assert_eq!(print_value(&literal), "#s(x)");
    assert_eq!(print_value_bytes(&literal), b"#s(x)");
}

#[test]
fn print_hash_table_object_uses_readable_hash_s_shape() {
    let table = Value::hash_table(HashTableTest::Equal);
    // GNU Emacs prints "test equal" for non-default test (default is eql).
    assert_eq!(print_value(&table), "#s(hash-table test equal)");
    assert_eq!(print_value_bytes(&table), b"#s(hash-table test equal)");
}

#[test]
fn print_quote_shorthand_lists() {
    let quoted = Value::list(vec![Value::symbol("quote"), Value::symbol("foo")]);
    let function = Value::list(vec![Value::symbol("function"), Value::symbol("car")]);
    let quasiquoted = Value::list(vec![
        Value::symbol("`"),
        Value::list(vec![Value::symbol("a"), Value::symbol("b")]),
    ]);
    let unquoted = Value::list(vec![Value::symbol(","), Value::symbol("x")]);
    let unquote_splice = Value::list(vec![Value::symbol(",@"), Value::symbol("xs")]);

    assert_eq!(print_value(&quoted), "'foo");
    assert_eq!(print_value(&function), "#'car");
    assert_eq!(print_value(&quasiquoted), "`(a b)");
    assert_eq!(print_value(&unquoted), "(\\, x)");
    assert_eq!(print_value(&unquote_splice), "(\\,@ xs)");
}

#[test]
fn print_backquote_preserves_nested_unquote_shorthand_only_in_context() {
    let nested = Value::list(vec![
        Value::symbol("`"),
        Value::list(vec![
            Value::symbol("a"),
            Value::list(vec![Value::symbol(","), Value::symbol("x")]),
        ]),
    ]);

    assert_eq!(print_value(&nested), "`(a ,x)");
}

#[test]
fn print_dotted_pair() {
    let pair = Value::cons(Value::fixnum(1), Value::fixnum(2));
    assert_eq!(print_value(&pair), "(1 . 2)");
}

#[test]
fn print_vector() {
    let v = Value::vector(vec![Value::fixnum(1), Value::fixnum(2)]);
    assert_eq!(print_value(&v), "[1 2]");
}

#[test]
fn print_lambda() {
    let lam = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![intern("x"), intern("y")]),
        body: vec![Expr::List(vec![
            Expr::Symbol(intern("+")),
            Expr::Symbol(intern("x")),
            Expr::Symbol(intern("y")),
        ])]
        .into(),
        env: None,
        docstring: None,
        doc_form: None,
        interactive: None,
    });
    assert_eq!(print_value(&lam), "(lambda (x y) (+ x y))");
}

#[test]
fn print_lexical_closure_uses_gnu_vector_syntax() {
    let closure = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![intern("a"), intern("b")]),
        body: vec![Expr::List(vec![
            Expr::Symbol(intern("+")),
            Expr::Symbol(intern("a")),
            Expr::Symbol(intern("b")),
            Expr::Symbol(intern("x")),
        ])]
        .into(),
        env: Some(Value::list(vec![Value::cons(
            Value::symbol("x"),
            Value::fixnum(42),
        )])),
        docstring: None,
        doc_form: None,
        interactive: None,
    });

    assert_eq!(print_value(&closure), "#[(a b) ((+ a b x)) ((x . 42))]");
    assert_eq!(
        String::from_utf8(print_value_bytes(&closure)).expect("utf8"),
        "#[(a b) ((+ a b x)) ((x . 42))]"
    );
}

#[test]
fn print_recursive_closure_uses_backreference() {
    let binding = Value::cons(Value::symbol("f"), Value::NIL);
    let env = Value::list(vec![binding]);
    let closure = Value::make_lambda(LambdaData {
        params: LambdaParams::simple(vec![]),
        body: vec![Expr::Symbol(intern("f"))].into(),
        env: Some(env),
        docstring: None,
        doc_form: None,
        interactive: None,
    });
    binding.set_cdr(closure);

    assert_eq!(print_value(&closure), "#[nil (f) ((f . #0))]");
    assert_eq!(
        String::from_utf8(print_value_bytes(&closure)).expect("utf8"),
        "#[nil (f) ((f . #0))]"
    );
}

#[test]
fn print_terminal_handle_special_form() {
    let list = super::super::terminal::pure::builtin_terminal_list(vec![]).unwrap();
    let items = list_to_vec(&list).expect("terminal-list should return a list");
    let handle = items
        .first()
        .expect("terminal-list should contain one handle");

    let printed = print_value(handle);
    assert!(printed.starts_with("#<terminal "));
    assert!(printed.contains("on initial_terminal>"));
}

#[test]
fn print_frame_handles_use_oracle_style_f_prefix() {
    let f1 = Value::make_frame(crate::window::FRAME_ID_BASE);
    let f2 = Value::make_frame(crate::window::FRAME_ID_BASE + 1);
    let legacy = Value::make_frame(7);

    assert_eq!(print_value(&f1), "#<frame F1 0x100000000>");
    assert_eq!(print_value_bytes(&f1), b"#<frame F1 0x100000000>");
    assert_eq!(print_value(&f2), "#<frame F2 0x100000001>");
    assert_eq!(print_value_bytes(&f2), b"#<frame F2 0x100000001>");
    assert_eq!(print_value(&legacy), "#<frame 7>");
}

#[test]
fn print_markers_use_gnu_style_handles() {
    let marker = make_marker_value(None, None, false);
    assert_eq!(print_value(&marker), "#<marker in no buffer>");

    let mut buffers = crate::buffer::BufferManager::new();
    let buffer_id = buffers
        .find_buffer_by_name("*scratch*")
        .expect("scratch buffer");
    let marker = make_marker_value(Some(buffer_id), Some(3), false);
    assert_eq!(
        print_value_with_buffers(&marker, &buffers),
        "#<marker at 3 in *scratch*>"
    );

    buffers.kill_buffer(buffer_id);
    assert_eq!(
        print_value_with_buffers(&marker, &buffers),
        "#<marker in no buffer>"
    );
}
