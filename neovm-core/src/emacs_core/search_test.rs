use super::*;
use crate::emacs_core::builtins::search::{
    builtin_match_data, builtin_set_match_data, builtin_string_match, builtin_string_match_p,
};
use crate::emacs_core::search::builtin_replace_regexp_in_string;
use crate::emacs_core::value::{ValueKind};

fn call_string_match(args: Vec<Value>) -> EvalResult {
    let mut eval = crate::emacs_core::eval::Context::new();
    builtin_string_match(&mut eval, args)
}

fn call_string_match_p(args: Vec<Value>) -> EvalResult {
    let mut eval = crate::emacs_core::eval::Context::new();
    builtin_string_match_p(&mut eval, args)
}

fn call_replace_regexp_in_string(args: Vec<Value>) -> EvalResult {
    let mut eval = crate::emacs_core::eval::Context::new();
    builtin_replace_regexp_in_string(&mut eval, args)
}

fn assert_int(val: Value, expected: i64) {
    match val.kind() {
        ValueKind::Fixnum(n) => assert_eq!(n, expected),
        other => panic!("Expected Int({}), got {:?}", expected, other),
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
            let s = crate::emacs_core::value::with_heap(|h| h.get_string(id).to_owned());
            assert_eq!(&*s, expected);
        }
        other => panic!("Expected string {:?}, got {:?}", expected, other),
    }
}

#[test]
fn string_match_basic() {
    let result = call_string_match(vec![Value::string("he..o"), Value::string("hello world")]);
    assert_int(result.unwrap(), 0);
}

#[test]
fn string_match_with_start() {
    let result = call_string_match(vec![
        Value::string("world"),
        Value::string("hello world"),
        Value::fixnum(6),
    ]);
    assert_int(result.unwrap(), 6);
}

#[test]
fn string_match_no_match() {
    let result = call_string_match(vec![Value::string("xyz"), Value::string("hello world")]);
    assert_nil(result.unwrap());
}

#[test]
fn string_match_defaults_to_case_fold() {
    let result = call_string_match(vec![Value::string("a"), Value::string("A")]);
    assert_int(result.unwrap(), 0);
}

#[test]
fn string_match_p_basic() {
    let result = call_string_match_p(vec![Value::string("[0-9]+"), Value::string("abc 123 def")]);
    assert_int(result.unwrap(), 4);
}

#[test]
fn string_match_p_no_match() {
    let result = call_string_match_p(vec![
        Value::string("[0-9]+"),
        Value::string("no digits here"),
    ]);
    assert_nil(result.unwrap());
}

#[test]
fn string_match_p_defaults_to_case_fold() {
    let result = call_string_match_p(vec![Value::string("a"), Value::string("A")]);
    assert_int(result.unwrap(), 0);
}

#[test]
fn regexp_quote_specials() {
    let result = builtin_regexp_quote(vec![Value::string("foo.bar*baz+qux")]);
    assert_str(result.unwrap(), "foo\\.bar\\*baz\\+qux");
}

#[test]
fn regexp_quote_no_specials() {
    let result = builtin_regexp_quote(vec![Value::string("hello")]);
    assert_str(result.unwrap(), "hello");
}

#[test]
fn regexp_quote_all_specials() {
    let result = builtin_regexp_quote(vec![Value::string(".*+?[]^$\\")]);
    // GNU regexp-quote does NOT escape ']' — only '[' is special.
    assert_str(result.unwrap(), "\\.\\*\\+\\?\\[]\\^\\$\\\\");
}

#[test]
fn match_data_nil_without_match_data() {
    let mut eval = crate::emacs_core::eval::Context::new();
    builtin_set_match_data(&mut eval, vec![Value::NIL]).unwrap();
    let result = builtin_match_data(&mut eval, vec![]);
    assert_nil(result.unwrap());
}

#[test]
fn set_match_data_nil_clears_state() {
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

    let out_of_range =
        call_string_match(vec![Value::string("a"), Value::string("ba"), Value::fixnum(3)]);
    assert!(out_of_range.is_err());
}

#[test]
fn replace_regexp_basic() {
    let result = call_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("NUM"),
        Value::string("abc 123 def 456"),
    ]);
    assert_str(result.unwrap(), "abc NUM def NUM");
}

#[test]
fn replace_regexp_literal() {
    let result = call_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("$0"),
        Value::string("abc 123 def"),
        Value::NIL,  // fixedcase
        Value::T, // literal
    ]);
    assert_str(result.unwrap(), "abc $0 def");
}

#[test]
fn replace_regexp_with_backref() {
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
    // Emacs: START omits the first START chars from the result.
    let result = call_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("X"),
        Value::string("111 222 333"),
        Value::NIL,    // fixedcase
        Value::NIL,    // literal
        Value::NIL,    // subexp
        Value::fixnum(4), // start
    ]);
    assert_str(result.unwrap(), "X X");
}

#[test]
fn replace_regexp_with_start_no_subexp() {
    // In Emacs, arg 6 is SUBEXP and arg 7 is START.
    // To pass START without SUBEXP, use nil for SUBEXP.
    let result = call_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("X"),
        Value::string("111 222 333"),
        Value::NIL,    // fixedcase
        Value::NIL,    // literal
        Value::NIL,    // subexp (default 0)
        Value::fixnum(4), // start
    ]);
    assert_str(result.unwrap(), "X X");
}

#[test]
fn replace_regexp_subexp() {
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
    let result = call_replace_regexp_in_string(vec![
        Value::string("a"),
        Value::string("x"),
        Value::string("A a"),
    ]);
    assert_str(result.unwrap(), "X x");
}

#[test]
fn replace_regexp_fixedcase_disables_case_preserve() {
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
    let result = call_string_match(vec![Value::fixnum(42), Value::string("hello")]);
    assert!(result.is_err());
}

#[test]
fn string_match_too_few_args() {
    let result = call_string_match(vec![Value::string("foo")]);
    assert!(result.is_err());
}

#[test]
fn regexp_quote_parens_not_escaped() {
    // In Emacs regex, literal ( ) are NOT special, so regexp-quote
    // should NOT escape them.
    let result = builtin_regexp_quote(vec![Value::string("(foo)")]);
    assert_str(result.unwrap(), "(foo)");
}

#[test]
fn regexp_quote_right_bracket_not_escaped() {
    let result = builtin_regexp_quote(vec![Value::string("]")]);
    assert_str(result.unwrap(), "]");
}

#[test]
fn string_match_emacs_groups() {
    // Emacs regex with groups: \(foo\|bar\) matching "test bar"
    let result = call_string_match(vec![
        Value::string("\\(foo\\|bar\\)"),
        Value::string("test bar"),
    ]);
    assert_int(result.unwrap(), 5);
}
