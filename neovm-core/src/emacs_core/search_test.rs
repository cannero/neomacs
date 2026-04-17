use super::*;
use crate::emacs_core::builtins::search::{
    builtin_looking_at, builtin_match_data, builtin_match_string, builtin_set_match_data,
    builtin_string_match, builtin_string_match_p,
};
use crate::emacs_core::search::builtin_replace_regexp_in_string;
use crate::emacs_core::value::ValueKind;
use crate::heap_types::LispString;

// Test helpers that keep the Context alive across the returned
// Value. Previously these created a bare Context inside a block
// and returned the result; the Context was dropped at end of
// block, destroying the tagged heap and leaving the returned
// Value pointing at freed memory. A thread_local Box<Context>
// holds it alive until the next call replaces it.
use std::cell::RefCell;
thread_local! {
    static SEARCH_TEST_CTX: RefCell<Option<Box<crate::emacs_core::eval::Context>>> =
        const { RefCell::new(None) };
}

fn call_string_match(args: Vec<Value>) -> EvalResult {
    SEARCH_TEST_CTX.with(|slot| {
        let mut new_ctx = Box::new(crate::emacs_core::eval::Context::new());
        let result = builtin_string_match(&mut new_ctx, args);
        *slot.borrow_mut() = Some(new_ctx);
        result
    })
}

fn call_string_match_p(args: Vec<Value>) -> EvalResult {
    SEARCH_TEST_CTX.with(|slot| {
        let mut new_ctx = Box::new(crate::emacs_core::eval::Context::new());
        let result = builtin_string_match_p(&mut new_ctx, args);
        *slot.borrow_mut() = Some(new_ctx);
        result
    })
}

fn call_replace_regexp_in_string(args: Vec<Value>) -> EvalResult {
    SEARCH_TEST_CTX.with(|slot| {
        let mut new_ctx = Box::new(crate::emacs_core::eval::Context::new());
        let result = builtin_replace_regexp_in_string(&mut new_ctx, args);
        *slot.borrow_mut() = Some(new_ctx);
        result
    })
}

fn call_looking_at_in_buffer(pattern: Value, buffer_text: &str) -> EvalResult {
    SEARCH_TEST_CTX.with(|slot| {
        let mut new_ctx = Box::new(crate::emacs_core::eval::Context::new());
        {
            let buf = new_ctx
                .buffers
                .current_buffer_mut()
                .expect("current buffer");
            buf.text.insert_str(0, buffer_text);
            buf.goto_byte(0);
        }
        let result = builtin_looking_at(&mut new_ctx, vec![pattern]);
        *slot.borrow_mut() = Some(new_ctx);
        result
    })
}

fn assert_int(val: Value, expected: i64) {
    match val.kind() {
        ValueKind::Fixnum(n) => assert_eq!(n, expected),
        other => panic!("Expected Int({}), got {:?}", expected, val),
    }
}

fn assert_nil(val: Value) {
    assert!(val.is_nil(), "Expected nil, got {:?}", val);
}

fn assert_true(val: Value) {
    assert!(val.is_truthy(), "Expected true, got {:?}", val);
}

fn assert_str(val: Value, expected: &str) {
    match val.kind() {
        ValueKind::String => {
            let s = val.as_utf8_str().unwrap().to_owned();
            assert_eq!(&*s, expected);
        }
        other => panic!("Expected string {:?}, got {:?}", expected, val),
    }
}

fn assert_lisp_string_bytes(val: Value, expected: &[u8], multibyte: bool) {
    match val.kind() {
        ValueKind::String => {
            let s = val
                .as_lisp_string()
                .expect("ValueKind::String must carry LispString payload");
            assert_eq!(s.as_bytes(), expected);
            assert_eq!(s.is_multibyte(), multibyte);
        }
        other => panic!("Expected string bytes {:?}, got {:?}", expected, val),
    }
}

#[test]
fn string_match_basic() {
    crate::test_utils::init_test_tracing();
    let result = call_string_match(vec![Value::string("he..o"), Value::string("hello world")]);
    assert_int(result.unwrap(), 0);
}

#[test]
fn string_match_with_start() {
    crate::test_utils::init_test_tracing();
    let result = call_string_match(vec![
        Value::string("world"),
        Value::string("hello world"),
        Value::fixnum(6),
    ]);
    assert_int(result.unwrap(), 6);
}

#[test]
fn string_match_no_match() {
    crate::test_utils::init_test_tracing();
    let result = call_string_match(vec![Value::string("xyz"), Value::string("hello world")]);
    assert_nil(result.unwrap());
}

#[test]
fn string_match_defaults_to_case_fold() {
    crate::test_utils::init_test_tracing();
    let result = call_string_match(vec![Value::string("a"), Value::string("A")]);
    assert_int(result.unwrap(), 0);
}

#[test]
fn string_match_p_basic() {
    crate::test_utils::init_test_tracing();
    let result = call_string_match_p(vec![Value::string("[0-9]+"), Value::string("abc 123 def")]);
    assert_int(result.unwrap(), 4);
}

#[test]
fn string_match_p_no_match() {
    crate::test_utils::init_test_tracing();
    let result = call_string_match_p(vec![
        Value::string("[0-9]+"),
        Value::string("no digits here"),
    ]);
    assert_nil(result.unwrap());
}

#[test]
fn string_match_p_defaults_to_case_fold() {
    crate::test_utils::init_test_tracing();
    let result = call_string_match_p(vec![Value::string("a"), Value::string("A")]);
    assert_int(result.unwrap(), 0);
}

#[test]
fn string_match_accepts_raw_unibyte_regexp_pattern() {
    crate::test_utils::init_test_tracing();
    let pattern = Value::heap_string(LispString::from_unibyte(vec![0xFF]));
    let haystack = Value::heap_string(LispString::from_unibyte(vec![0x80, 0xFF, 0x81]));
    let result = call_string_match(vec![pattern, haystack]).unwrap();
    assert_int(result, 1);
}

#[test]
fn string_match_accepts_start_on_raw_unibyte_haystack() {
    crate::test_utils::init_test_tracing();
    let pattern = Value::heap_string(LispString::from_unibyte(vec![0xFF]));
    let haystack = Value::heap_string(LispString::from_unibyte(vec![0x80, 0xFF, 0x81]));
    let result = call_string_match(vec![pattern, haystack, Value::fixnum(1)]).unwrap();
    assert_int(result, 1);
}

#[test]
fn match_string_preserves_raw_unibyte_bytes_with_explicit_string() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let pattern = Value::heap_string(LispString::from_unibyte(vec![0xFF]));
    let haystack = Value::heap_string(LispString::from_unibyte(vec![0x80, 0xFF, 0x81]));
    let matched = builtin_string_match(&mut eval, vec![pattern, haystack]).unwrap();
    assert_int(matched, 1);

    let slice = builtin_match_string(&mut eval, vec![Value::fixnum(0), haystack]).unwrap();
    let string = slice
        .as_lisp_string()
        .expect("match-string should return string");
    assert!(!string.is_multibyte());
    assert_eq!(string.as_bytes(), &[0xFF]);
}

#[test]
fn looking_at_accepts_raw_unibyte_regexp_pattern_in_buffer() {
    crate::test_utils::init_test_tracing();
    let pattern = Value::heap_string(LispString::from_unibyte(vec![0xFF]));
    let result = call_looking_at_in_buffer(pattern, "tool-bar").unwrap();
    assert_nil(result);
}

#[test]
fn regexp_quote_specials() {
    crate::test_utils::init_test_tracing();
    let result = builtin_regexp_quote(vec![Value::string("foo.bar*baz+qux")]);
    assert_str(result.unwrap(), "foo\\.bar\\*baz\\+qux");
}

#[test]
fn regexp_quote_no_specials() {
    crate::test_utils::init_test_tracing();
    let result = builtin_regexp_quote(vec![Value::string("hello")]);
    assert_str(result.unwrap(), "hello");
}

#[test]
fn regexp_quote_all_specials() {
    crate::test_utils::init_test_tracing();
    let result = builtin_regexp_quote(vec![Value::string(".*+?[]^$\\")]);
    // GNU regexp-quote does NOT escape ']' — only '[' is special.
    assert_str(result.unwrap(), "\\.\\*\\+\\?\\[]\\^\\$\\\\");
}

#[test]
fn regexp_quote_preserves_raw_unibyte_bytes() {
    crate::test_utils::init_test_tracing();
    let input = Value::heap_string(LispString::from_unibyte(vec![b'a', 0xFF, b'*', b'[', b'z']));
    let result = builtin_regexp_quote(vec![input]).unwrap();
    let string = result
        .as_lisp_string()
        .expect("regexp-quote should return string");
    assert!(!string.is_multibyte());
    assert_eq!(
        string.as_bytes(),
        &[b'a', 0xFF, b'\\', b'*', b'\\', b'[', b'z']
    );
    assert!(string.as_utf8_str().is_none());
}

#[test]
fn match_data_nil_without_match_data() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    builtin_set_match_data(&mut eval, vec![Value::NIL]).unwrap();
    let result = builtin_match_data(&mut eval, vec![]);
    assert_nil(result.unwrap());
}

#[test]
fn set_match_data_nil_clears_state() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    builtin_set_match_data(
        &mut eval,
        vec![Value::list(vec![Value::fixnum(1), Value::fixnum(2)])],
    )
    .unwrap();
    let result = builtin_set_match_data(&mut eval, vec![Value::NIL]);
    assert_nil(result.unwrap());
    let md = builtin_match_data(&mut eval, vec![]).unwrap();
    assert_nil(md);
}

#[test]
fn set_match_data_round_trip() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    builtin_set_match_data(
        &mut eval,
        vec![Value::list(vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::NIL,
            Value::NIL,
            Value::fixnum(5),
            Value::fixnum(7),
        ])],
    )
    .unwrap();
    let md = builtin_match_data(&mut eval, vec![]).unwrap();
    assert_eq!(
        md,
        Value::list(vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::NIL,
            Value::NIL,
            Value::fixnum(5),
            Value::fixnum(7)
        ])
    );
}

#[test]
fn string_match_start_nil_and_negative() {
    crate::test_utils::init_test_tracing();
    let with_nil =
        call_string_match(vec![Value::string("a"), Value::string("ba"), Value::NIL]).unwrap();
    assert_int(with_nil, 1);

    let with_negative = call_string_match(vec![
        Value::string("a"),
        Value::string("ba"),
        Value::fixnum(-1),
    ])
    .unwrap();
    assert_int(with_negative, 1);

    let out_of_range = call_string_match(vec![
        Value::string("a"),
        Value::string("ba"),
        Value::fixnum(3),
    ]);
    assert!(out_of_range.is_err());
}

#[test]
fn replace_regexp_basic() {
    crate::test_utils::init_test_tracing();
    let result = call_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("NUM"),
        Value::string("abc 123 def 456"),
    ]);
    assert_str(result.unwrap(), "abc NUM def NUM");
}

#[test]
fn replace_regexp_literal() {
    crate::test_utils::init_test_tracing();
    let result = call_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("$0"),
        Value::string("abc 123 def"),
        Value::NIL, // fixedcase
        Value::T,   // literal
    ]);
    assert_str(result.unwrap(), "abc $0 def");
}

#[test]
fn replace_regexp_with_backref() {
    crate::test_utils::init_test_tracing();
    // Use Emacs-style group: \(\w+\) and back-reference \1
    let result = call_replace_regexp_in_string(vec![
        Value::string("\\(\\w+\\)"),
        Value::string("[\\1]"),
        Value::string("hello world"),
    ]);
    assert_str(result.unwrap(), "[hello] [world]");
}

#[test]
fn replace_regexp_with_start() {
    crate::test_utils::init_test_tracing();
    // Emacs: START omits the first START chars from the result.
    let result = call_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("X"),
        Value::string("111 222 333"),
        Value::NIL,       // fixedcase
        Value::NIL,       // literal
        Value::NIL,       // subexp
        Value::fixnum(4), // start
    ]);
    assert_str(result.unwrap(), "X X");
}

#[test]
fn replace_regexp_with_start_no_subexp() {
    crate::test_utils::init_test_tracing();
    // In Emacs, arg 6 is SUBEXP and arg 7 is START.
    // To pass START without SUBEXP, use nil for SUBEXP.
    let result = call_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("X"),
        Value::string("111 222 333"),
        Value::NIL,       // fixedcase
        Value::NIL,       // literal
        Value::NIL,       // subexp (default 0)
        Value::fixnum(4), // start
    ]);
    assert_str(result.unwrap(), "X X");
}

#[test]
fn replace_regexp_subexp() {
    crate::test_utils::init_test_tracing();
    let result = call_replace_regexp_in_string(vec![
        Value::string("\\([a-z]+\\)-\\([0-9]+\\)"),
        Value::string("N"),
        Value::string("aaa-111 bbb-222"),
        Value::NIL, // fixedcase
        Value::NIL, // literal
        Value::fixnum(1),
        Value::NIL, // start
    ]);
    assert_str(result.unwrap(), "N-111 N-222");
}

#[test]
fn replace_regexp_subexp_unmatched_errors() {
    crate::test_utils::init_test_tracing();
    let result = call_replace_regexp_in_string(vec![
        Value::string("\\(a\\)?b"),
        Value::string("N"),
        Value::string("b"),
        Value::NIL,
        Value::NIL,
        Value::fixnum(1),
        Value::NIL,
    ]);
    assert!(result.is_err());
}

#[test]
fn replace_regexp_preserves_case_when_fixedcase_nil() {
    crate::test_utils::init_test_tracing();
    let result = call_replace_regexp_in_string(vec![
        Value::string("a"),
        Value::string("x"),
        Value::string("A a"),
    ]);
    assert_str(result.unwrap(), "X x");
}

#[test]
fn replace_regexp_fixedcase_disables_case_preserve() {
    crate::test_utils::init_test_tracing();
    let result = call_replace_regexp_in_string(vec![
        Value::string("a"),
        Value::string("x"),
        Value::string("A a"),
        Value::T, // fixedcase
    ]);
    assert_str(result.unwrap(), "x x");
}

#[test]
fn string_match_wrong_type() {
    crate::test_utils::init_test_tracing();
    let result = call_string_match(vec![Value::fixnum(42), Value::string("hello")]);
    assert!(result.is_err());
}

#[test]
fn string_match_too_few_args() {
    crate::test_utils::init_test_tracing();
    let result = call_string_match(vec![Value::string("foo")]);
    assert!(result.is_err());
}

#[test]
fn regexp_quote_parens_not_escaped() {
    crate::test_utils::init_test_tracing();
    // In Emacs regex, literal ( ) are NOT special, so regexp-quote
    // should NOT escape them.
    let result = builtin_regexp_quote(vec![Value::string("(foo)")]);
    assert_str(result.unwrap(), "(foo)");
}

#[test]
fn regexp_quote_right_bracket_not_escaped() {
    crate::test_utils::init_test_tracing();
    let result = builtin_regexp_quote(vec![Value::string("]")]);
    assert_str(result.unwrap(), "]");
}

#[test]
fn regexp_quote_preserves_raw_unibyte_bytes_compact_case() {
    crate::test_utils::init_test_tracing();
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
        b'a', 0xFF, b'[', b'.',
    ]));
    let result = builtin_regexp_quote(vec![raw]).unwrap();
    assert_lisp_string_bytes(result, &[b'a', 0xFF, b'\\', b'[', b'\\', b'.'], false);
}

#[test]
fn string_match_emacs_groups() {
    crate::test_utils::init_test_tracing();
    // Emacs regex with groups: \(foo\|bar\) matching "test bar"
    let result = call_string_match(vec![
        Value::string("\\(foo\\|bar\\)"),
        Value::string("test bar"),
    ]);
    assert_int(result.unwrap(), 5);
}

#[test]
fn replace_regexp_preserves_raw_unibyte_source() {
    crate::test_utils::init_test_tracing();
    let source = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
        b'a', 0xFF, b'b',
    ]));
    let result =
        call_replace_regexp_in_string(vec![Value::string("a"), Value::string("x"), source])
            .unwrap();
    assert_lisp_string_bytes(result, &[b'x', 0xFF, b'b'], false);
}

#[test]
fn replace_regexp_preserves_raw_unibyte_replacement() {
    crate::test_utils::init_test_tracing();
    let replacement = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFE]));
    let source = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
        b'a', 0xFF, b'b',
    ]));
    let result =
        call_replace_regexp_in_string(vec![Value::string("a"), replacement, source]).unwrap();
    assert_lisp_string_bytes(result, &[0xFE, 0xFF, b'b'], false);
}

#[test]
fn replace_regexp_function_can_return_raw_unibyte_string() {
    crate::test_utils::init_test_tracing();
    let source = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
        b'a', 0xFF, b'b',
    ]));
    let replacement = Value::list(vec![
        Value::symbol("lambda"),
        Value::list(vec![Value::symbol("_")]),
        Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFE])),
    ]);
    let result =
        call_replace_regexp_in_string(vec![Value::string("a"), replacement, source]).unwrap();
    assert_lisp_string_bytes(result, &[0xFE, 0xFF, b'b'], false);
}
