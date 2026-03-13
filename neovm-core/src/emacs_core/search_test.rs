use super::*;

fn assert_int(val: Value, expected: i64) {
    match val {
        Value::Int(n) => assert_eq!(n, expected),
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
    match val {
        Value::Str(id) => {
            let s = crate::emacs_core::value::with_heap(|h| h.get_string(id).to_owned());
            assert_eq!(&*s, expected);
        }
        other => panic!("Expected string {:?}, got {:?}", expected, other),
    }
}

#[test]
fn string_match_basic() {
    let result = builtin_string_match(vec![Value::string("he..o"), Value::string("hello world")]);
    assert_int(result.unwrap(), 0);
}

#[test]
fn string_match_with_start() {
    let result = builtin_string_match(vec![
        Value::string("world"),
        Value::string("hello world"),
        Value::Int(6),
    ]);
    assert_int(result.unwrap(), 6);
}

#[test]
fn string_match_no_match() {
    let result = builtin_string_match(vec![Value::string("xyz"), Value::string("hello world")]);
    assert_nil(result.unwrap());
}

#[test]
fn string_match_defaults_to_case_fold() {
    let result = builtin_string_match(vec![Value::string("a"), Value::string("A")]);
    assert_int(result.unwrap(), 0);
}

#[test]
fn string_match_p_basic() {
    let result =
        builtin_string_match_p(vec![Value::string("[0-9]+"), Value::string("abc 123 def")]);
    assert_int(result.unwrap(), 4);
}

#[test]
fn string_match_p_no_match() {
    let result = builtin_string_match_p(vec![
        Value::string("[0-9]+"),
        Value::string("no digits here"),
    ]);
    assert_nil(result.unwrap());
}

#[test]
fn string_match_p_defaults_to_case_fold() {
    let result = builtin_string_match_p(vec![Value::string("a"), Value::string("A")]);
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
    assert_str(result.unwrap(), "\\.\\*\\+\\?\\[\\]\\^\\$\\\\");
}

#[test]
fn match_beginning_nil_without_match_data() {
    builtin_set_match_data(vec![Value::Nil]).unwrap();
    let result = builtin_match_beginning(vec![Value::Int(0)]);
    assert_nil(result.unwrap());
}

#[test]
fn match_end_nil_without_match_data() {
    builtin_set_match_data(vec![Value::Nil]).unwrap();
    let result = builtin_match_end(vec![Value::Int(0)]);
    assert_nil(result.unwrap());
}

#[test]
fn match_data_nil_without_match_data() {
    builtin_set_match_data(vec![Value::Nil]).unwrap();
    let result = builtin_match_data(vec![]);
    assert_nil(result.unwrap());
}

#[test]
fn set_match_data_nil_clears_state() {
    builtin_set_match_data(vec![Value::list(vec![Value::Int(1), Value::Int(2)])]).unwrap();
    let result = builtin_set_match_data(vec![Value::Nil]);
    assert_nil(result.unwrap());
    let md = builtin_match_data(vec![]).unwrap();
    assert_nil(md);
}

#[test]
fn set_match_data_round_trip() {
    builtin_set_match_data(vec![Value::list(vec![
        Value::Int(1),
        Value::Int(2),
        Value::Nil,
        Value::Nil,
        Value::Int(5),
        Value::Int(7),
    ])])
    .unwrap();
    let md = builtin_match_data(vec![]).unwrap();
    assert_eq!(
        md,
        Value::list(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Nil,
            Value::Nil,
            Value::Int(5),
            Value::Int(7)
        ])
    );
}

#[test]
fn string_match_updates_match_data() {
    builtin_set_match_data(vec![Value::Nil]).unwrap();
    let result = builtin_string_match(vec![
        Value::string("\\(foo\\|bar\\)"),
        Value::string("test bar"),
    ]);
    assert_int(result.unwrap(), 5);

    let begin = builtin_match_beginning(vec![Value::Int(0)]).unwrap();
    let end = builtin_match_end(vec![Value::Int(0)]).unwrap();
    assert_int(begin, 5);
    assert_int(end, 8);
}

#[test]
fn string_match_start_nil_and_negative() {
    let with_nil =
        builtin_string_match(vec![Value::string("a"), Value::string("ba"), Value::Nil]).unwrap();
    assert_int(with_nil, 1);

    let with_negative = builtin_string_match(vec![
        Value::string("a"),
        Value::string("ba"),
        Value::Int(-1),
    ])
    .unwrap();
    assert_int(with_negative, 1);

    let out_of_range =
        builtin_string_match(vec![Value::string("a"), Value::string("ba"), Value::Int(3)]);
    assert!(out_of_range.is_err());
}

#[test]
fn looking_at_default_at_point() {
    let result = builtin_looking_at(vec![Value::string("foo")]);
    assert_nil(result.unwrap());
}

#[test]
fn looking_at_with_text() {
    let result = builtin_looking_at(vec![Value::string("foo"), Value::string("foobar")]);
    assert_true(result.unwrap());
    let begin = builtin_match_beginning(vec![Value::Int(0)]).unwrap();
    let end = builtin_match_end(vec![Value::Int(0)]).unwrap();
    assert_int(begin, 0);
    assert_int(end, 3);
}

#[test]
fn looking_at_with_text_case_fold_default() {
    let result = builtin_looking_at(vec![Value::string("foo"), Value::string("FOO BAR")]);
    assert_true(result.unwrap());
}

#[test]
fn looking_at_with_text_requires_start_position() {
    let result = builtin_looking_at(vec![Value::string("foo"), Value::string("bar foo")]);
    assert_nil(result.unwrap());
}

#[test]
fn looking_at_with_text_no_match() {
    let result = builtin_looking_at(vec![Value::string("foo"), Value::string("bar")]);
    assert_nil(result.unwrap());

    let begin = builtin_match_beginning(vec![Value::Int(0)]).unwrap();
    assert_nil(begin);
}

#[test]
fn looking_at_invalid_regexp_signals() {
    let result = builtin_looking_at(vec![Value::string("[")]);
    assert!(result.is_err());
}

#[test]
fn looking_at_wrong_number_of_arguments() {
    let result = builtin_looking_at(vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn looking_at_with_limit_any_value() {
    let result = builtin_looking_at(vec![Value::string("foo"), Value::True]);
    assert_nil(result.unwrap());
}

#[test]
fn looking_at_with_limit_limit_nil() {
    let result = builtin_looking_at(vec![Value::string("foo"), Value::Nil]);
    assert_nil(result.unwrap());
}

#[test]
fn looking_at_with_limit_marker_like_char() {
    let result = builtin_looking_at(vec![Value::string("foo"), Value::Char('a')]);
    assert_nil(result.unwrap());
}

#[test]
fn looking_at_p_preserves_match_data() {
    let _ = builtin_looking_at(vec![Value::string("foo"), Value::string("foobar")]);
    let before = builtin_match_data(vec![]).unwrap();
    let result = builtin_looking_at_p(vec![Value::string("foo")]);
    assert_nil(result.unwrap());
    let after = builtin_match_data(vec![]).unwrap();
    assert_eq!(before, after);
}

#[test]
fn looking_at_p_does_not_signal_without_text() {
    let result = builtin_looking_at_p(vec![Value::string("foo")]);
    assert_nil(result.unwrap());
}

#[test]
fn looking_at_p_invalid_regexp_signals() {
    let result = builtin_looking_at_p(vec![Value::string("[")]);
    assert!(result.is_err());
}

#[test]
fn looking_at_p_wrong_number_of_arguments() {
    let result = builtin_looking_at_p(vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn replace_regexp_basic() {
    let result = builtin_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("NUM"),
        Value::string("abc 123 def 456"),
    ]);
    assert_str(result.unwrap(), "abc NUM def NUM");
}

#[test]
fn replace_regexp_literal() {
    let result = builtin_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("$0"),
        Value::string("abc 123 def"),
        Value::Nil,  // fixedcase
        Value::True, // literal
    ]);
    assert_str(result.unwrap(), "abc $0 def");
}

#[test]
fn replace_regexp_with_backref() {
    // Use Emacs-style group: \(\w+\) and back-reference \1
    let result = builtin_replace_regexp_in_string(vec![
        Value::string("\\(\\w+\\)"),
        Value::string("[\\1]"),
        Value::string("hello world"),
    ]);
    assert_str(result.unwrap(), "[hello] [world]");
}

#[test]
fn replace_regexp_with_start() {
    // Emacs: START omits the first START chars from the result.
    let result = builtin_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("X"),
        Value::string("111 222 333"),
        Value::Nil,    // fixedcase
        Value::Nil,    // literal
        Value::Nil,    // subexp
        Value::Int(4), // start
    ]);
    assert_str(result.unwrap(), "X X");
}

#[test]
fn replace_regexp_with_start_no_subexp() {
    // In Emacs, arg 6 is SUBEXP and arg 7 is START.
    // To pass START without SUBEXP, use nil for SUBEXP.
    let result = builtin_replace_regexp_in_string(vec![
        Value::string("[0-9]+"),
        Value::string("X"),
        Value::string("111 222 333"),
        Value::Nil,    // fixedcase
        Value::Nil,    // literal
        Value::Nil,    // subexp (default 0)
        Value::Int(4), // start
    ]);
    assert_str(result.unwrap(), "X X");
}

#[test]
fn replace_regexp_subexp() {
    let result = builtin_replace_regexp_in_string(vec![
        Value::string("\\([a-z]+\\)-\\([0-9]+\\)"),
        Value::string("N"),
        Value::string("aaa-111 bbb-222"),
        Value::Nil, // fixedcase
        Value::Nil, // literal
        Value::Int(1),
        Value::Nil, // start
    ]);
    assert_str(result.unwrap(), "N-111 N-222");
}

#[test]
fn replace_regexp_subexp_unmatched_errors() {
    let result = builtin_replace_regexp_in_string(vec![
        Value::string("\\(a\\)?b"),
        Value::string("N"),
        Value::string("b"),
        Value::Nil,
        Value::Nil,
        Value::Int(1),
        Value::Nil,
    ]);
    assert!(result.is_err());
}

#[test]
fn replace_regexp_preserves_case_when_fixedcase_nil() {
    let result = builtin_replace_regexp_in_string(vec![
        Value::string("a"),
        Value::string("x"),
        Value::string("A a"),
    ]);
    assert_str(result.unwrap(), "X x");
}

#[test]
fn replace_regexp_fixedcase_disables_case_preserve() {
    let result = builtin_replace_regexp_in_string(vec![
        Value::string("a"),
        Value::string("x"),
        Value::string("A a"),
        Value::True, // fixedcase
    ]);
    assert_str(result.unwrap(), "x x");
}

#[test]
fn string_match_wrong_type() {
    let result = builtin_string_match(vec![Value::Int(42), Value::string("hello")]);
    assert!(result.is_err());
}

#[test]
fn string_match_too_few_args() {
    let result = builtin_string_match(vec![Value::string("foo")]);
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
    let result = builtin_string_match(vec![
        Value::string("\\(foo\\|bar\\)"),
        Value::string("test bar"),
    ]);
    assert_int(result.unwrap(), 5);
}
