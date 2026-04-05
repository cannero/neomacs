use super::*;
use crate::emacs_core::intern::{intern, resolve_sym};
use crate::emacs_core::value::{Value, ValueKind};

/// Helper: read one form from a string, panic on error.
fn read1(input: &str) -> Value {
    let result = read_one(input, 0).expect("read_one failed");
    result.expect("no form found").0
}

/// Helper: read all forms from a string, panic on error.
fn read_all_ok(input: &str) -> Vec<Value> {
    read_all(input).expect("read_all failed")
}

// ---------------------------------------------------------------------------
// Integers
// ---------------------------------------------------------------------------

#[test]
fn integer_zero() {
    crate::test_utils::init_test_tracing();
    let v = read1("0");
    assert_eq!(v.as_fixnum(), Some(0));
}

#[test]
fn integer_positive() {
    crate::test_utils::init_test_tracing();
    let v = read1("42");
    assert_eq!(v.as_fixnum(), Some(42));
}

#[test]
fn integer_negative() {
    crate::test_utils::init_test_tracing();
    let v = read1("-17");
    assert_eq!(v.as_fixnum(), Some(-17));
}

#[test]
fn integer_hex() {
    crate::test_utils::init_test_tracing();
    let v = read1("0xFF");
    assert_eq!(v.as_fixnum(), Some(255));
}

#[test]
fn integer_hex_hash() {
    crate::test_utils::init_test_tracing();
    let v = read1("#xFF");
    assert_eq!(v.as_fixnum(), Some(255));
}

#[test]
fn integer_octal_hash() {
    crate::test_utils::init_test_tracing();
    let v = read1("#o77");
    assert_eq!(v.as_fixnum(), Some(63));
}

#[test]
fn integer_binary_hash() {
    crate::test_utils::init_test_tracing();
    let v = read1("#b1010");
    assert_eq!(v.as_fixnum(), Some(10));
}

// ---------------------------------------------------------------------------
// Floats
// ---------------------------------------------------------------------------

#[test]
fn float_simple() {
    crate::test_utils::init_test_tracing();
    let v = read1("3.14");
    assert_eq!(v.as_number_f64(), Some(3.14));
}

#[test]
fn float_exponent() {
    crate::test_utils::init_test_tracing();
    let v = read1("1e10");
    assert_eq!(v.as_number_f64(), Some(1e10));
}

#[test]
fn float_negative() {
    crate::test_utils::init_test_tracing();
    let v = read1("-2.5");
    assert_eq!(v.as_number_f64(), Some(-2.5));
}

#[test]
fn float_infinity() {
    crate::test_utils::init_test_tracing();
    let v = read1("1.0e+INF");
    assert_eq!(v.as_number_f64(), Some(f64::INFINITY));
}

#[test]
fn float_neg_infinity() {
    crate::test_utils::init_test_tracing();
    let v = read1("-1.0e+INF");
    assert_eq!(v.as_number_f64(), Some(f64::NEG_INFINITY));
}

#[test]
fn float_nan() {
    crate::test_utils::init_test_tracing();
    let v = read1("0.0e+NaN");
    assert!(v.as_number_f64().unwrap().is_nan());
}

// ---------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------

#[test]
fn symbol_simple() {
    crate::test_utils::init_test_tracing();
    let v = read1("foo");
    assert!(v.is_symbol_named("foo"));
}

#[test]
fn symbol_with_dashes() {
    crate::test_utils::init_test_tracing();
    let v = read1("some-symbol-name");
    assert!(v.is_symbol_named("some-symbol-name"));
}

#[test]
fn symbol_t() {
    crate::test_utils::init_test_tracing();
    let v = read1("t");
    assert_eq!(v, Value::T);
}

#[test]
fn symbol_nil() {
    crate::test_utils::init_test_tracing();
    let v = read1("nil");
    assert_eq!(v, Value::NIL);
}

#[test]
fn symbol_escaped() {
    crate::test_utils::init_test_tracing();
    let v = read1(r"a\ b");
    assert!(v.is_symbol_named("a b"));
}

// ---------------------------------------------------------------------------
// Keywords
// ---------------------------------------------------------------------------

#[test]
fn keyword_simple() {
    crate::test_utils::init_test_tracing();
    let v = read1(":foo");
    assert!(v.is_keyword());
    let id = v.as_keyword_id().unwrap();
    assert_eq!(resolve_sym(id), ":foo");
}

#[test]
fn keyword_bare_colon() {
    crate::test_utils::init_test_tracing();
    let v = read1(":");
    assert!(v.is_keyword());
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

#[test]
fn string_simple() {
    crate::test_utils::init_test_tracing();
    let v = read1(r#""hello""#);
    assert_eq!(v.as_str().unwrap(), "hello");
}

#[test]
fn string_escapes() {
    crate::test_utils::init_test_tracing();
    let v = read1(r#""a\nb\t""#);
    assert_eq!(v.as_str().unwrap(), "a\nb\t");
}

#[test]
fn string_hex_escape() {
    crate::test_utils::init_test_tracing();
    let v = read1(r#""\x41""#);
    assert_eq!(v.as_str().unwrap(), "A");
}

#[test]
fn string_unicode_escape() {
    crate::test_utils::init_test_tracing();
    let v = read1(r#""\u0041""#);
    assert_eq!(v.as_str().unwrap(), "A");
}

#[test]
fn string_octal_escape() {
    crate::test_utils::init_test_tracing();
    let v = read1(r#""\101""#);
    assert_eq!(v.as_str().unwrap(), "A");
}

// ---------------------------------------------------------------------------
// Character literals
// ---------------------------------------------------------------------------

#[test]
fn char_literal_simple() {
    crate::test_utils::init_test_tracing();
    let v = read1("?A");
    assert_eq!(v.as_fixnum(), Some(65));
}

#[test]
fn char_literal_space() {
    crate::test_utils::init_test_tracing();
    let v = read1("? ");
    assert_eq!(v.as_fixnum(), Some(32));
}

#[test]
fn char_literal_newline() {
    crate::test_utils::init_test_tracing();
    let v = read1("?\\n");
    assert_eq!(v.as_fixnum(), Some(10));
}

#[test]
fn char_literal_tab() {
    crate::test_utils::init_test_tracing();
    let v = read1("?\\t");
    assert_eq!(v.as_fixnum(), Some(9));
}

#[test]
fn char_literal_control() {
    crate::test_utils::init_test_tracing();
    // \C-a should be 1
    let v = read1("?\\C-a");
    assert_eq!(v.as_fixnum(), Some(1));
}

// ---------------------------------------------------------------------------
// Quote syntax
// ---------------------------------------------------------------------------

#[test]
fn quote_form() {
    crate::test_utils::init_test_tracing();
    let v = read1("'foo");
    // Should be (quote foo)
    assert!(v.is_cons());
    let car = v.cons_car();
    assert!(car.is_symbol_named("quote"));
    let cadr = v.cons_cdr().cons_car();
    assert!(cadr.is_symbol_named("foo"));
}

#[test]
fn backquote_form() {
    crate::test_utils::init_test_tracing();
    let v = read1("`foo");
    assert!(v.is_cons());
    let car = v.cons_car();
    assert!(car.is_symbol_named("`"));
}

#[test]
fn unquote_form() {
    crate::test_utils::init_test_tracing();
    let v = read1(",foo");
    assert!(v.is_cons());
    let car = v.cons_car();
    assert!(car.is_symbol_named(","));
}

#[test]
fn splice_form() {
    crate::test_utils::init_test_tracing();
    let v = read1(",@foo");
    assert!(v.is_cons());
    let car = v.cons_car();
    assert!(car.is_symbol_named(",@"));
}

#[test]
fn function_quote() {
    crate::test_utils::init_test_tracing();
    let v = read1("#'foo");
    assert!(v.is_cons());
    let car = v.cons_car();
    assert!(car.is_symbol_named("function"));
    let cadr = v.cons_cdr().cons_car();
    assert!(cadr.is_symbol_named("foo"));
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

#[test]
fn empty_list() {
    crate::test_utils::init_test_tracing();
    let v = read1("()");
    assert!(v.is_nil());
}

#[test]
fn proper_list() {
    crate::test_utils::init_test_tracing();
    let v = read1("(1 2 3)");
    // Should be a cons chain: (1 . (2 . (3 . nil)))
    assert!(v.is_cons());
    assert_eq!(v.cons_car().as_fixnum(), Some(1));
    let cdr1 = v.cons_cdr();
    assert_eq!(cdr1.cons_car().as_fixnum(), Some(2));
    let cdr2 = cdr1.cons_cdr();
    assert_eq!(cdr2.cons_car().as_fixnum(), Some(3));
    assert!(cdr2.cons_cdr().is_nil());
}

#[test]
fn dotted_pair() {
    crate::test_utils::init_test_tracing();
    let v = read1("(1 . 2)");
    assert!(v.is_cons());
    assert_eq!(v.cons_car().as_fixnum(), Some(1));
    assert_eq!(v.cons_cdr().as_fixnum(), Some(2));
}

#[test]
fn dotted_list() {
    crate::test_utils::init_test_tracing();
    let v = read1("(1 2 . 3)");
    assert!(v.is_cons());
    assert_eq!(v.cons_car().as_fixnum(), Some(1));
    let cdr1 = v.cons_cdr();
    assert_eq!(cdr1.cons_car().as_fixnum(), Some(2));
    assert_eq!(cdr1.cons_cdr().as_fixnum(), Some(3));
}

#[test]
fn nested_list() {
    crate::test_utils::init_test_tracing();
    let v = read1("(a (b c))");
    assert!(v.is_cons());
    let car = v.cons_car();
    assert!(car.is_symbol_named("a"));
    let inner = v.cons_cdr().cons_car();
    assert!(inner.is_cons());
    assert!(inner.cons_car().is_symbol_named("b"));
}

// ---------------------------------------------------------------------------
// Vectors
// ---------------------------------------------------------------------------

#[test]
fn empty_vector() {
    crate::test_utils::init_test_tracing();
    let v = read1("[]");
    assert!(v.is_vector());
    let data = v.as_vector_data().unwrap();
    assert_eq!(data.len(), 0);
}

#[test]
fn vector_with_elements() {
    crate::test_utils::init_test_tracing();
    let v = read1("[1 2 3]");
    assert!(v.is_vector());
    let data = v.as_vector_data().unwrap();
    assert_eq!(data.len(), 3);
    assert_eq!(data[0].as_fixnum(), Some(1));
    assert_eq!(data[1].as_fixnum(), Some(2));
    assert_eq!(data[2].as_fixnum(), Some(3));
}

// ---------------------------------------------------------------------------
// Hash syntax
// ---------------------------------------------------------------------------

#[test]
fn uninterned_symbol() {
    crate::test_utils::init_test_tracing();
    let v = read1("#:foo");
    // Should be a symbol (uninterned)
    let name = v.as_symbol_name().unwrap();
    assert_eq!(name, "foo");
}

#[test]
fn empty_symbol() {
    crate::test_utils::init_test_tracing();
    let v = read1("##");
    let name = v.as_symbol_name().unwrap();
    assert_eq!(name, "");
}

#[test]
fn byte_code_literal() {
    crate::test_utils::init_test_tracing();
    let v = read1("#[1 2 3]");
    // Should be (byte-code-literal [1 2 3])
    assert!(v.is_cons());
    let car = v.cons_car();
    assert!(car.is_symbol_named("byte-code-literal"));
    let vec = v.cons_cdr().cons_car();
    assert!(vec.is_vector());
}

#[test]
fn read_label_define_and_ref() {
    crate::test_utils::init_test_tracing();
    // #1=(a b) #1# should return the same list for both positions
    let forms = read_all_ok("#1=(1 2) #1#");
    assert_eq!(forms.len(), 2);
    // Both should be the same (1 2) list
    assert!(forms[0].is_cons());
    assert!(forms[1].is_cons());
    assert_eq!(forms[0].cons_car().as_fixnum(), Some(1));
    assert_eq!(forms[1].cons_car().as_fixnum(), Some(1));
}

// ---------------------------------------------------------------------------
// Propertized strings
// ---------------------------------------------------------------------------

#[test]
fn propertized_string() {
    crate::test_utils::init_test_tracing();
    let v = read1(r#"#("hello" 0 5 (face bold))"#);
    assert_eq!(v.as_str().unwrap(), "hello");
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

#[test]
fn line_comment() {
    crate::test_utils::init_test_tracing();
    let v = read1("; this is a comment\n42");
    assert_eq!(v.as_fixnum(), Some(42));
}

#[test]
fn block_comment() {
    crate::test_utils::init_test_tracing();
    let v = read1("#| block comment |# 42");
    assert_eq!(v.as_fixnum(), Some(42));
}

// ---------------------------------------------------------------------------
// read_all and read_one
// ---------------------------------------------------------------------------

#[test]
fn read_all_multiple_forms() {
    crate::test_utils::init_test_tracing();
    let forms = read_all_ok("1 2 3");
    assert_eq!(forms.len(), 3);
    assert_eq!(forms[0].as_fixnum(), Some(1));
    assert_eq!(forms[1].as_fixnum(), Some(2));
    assert_eq!(forms[2].as_fixnum(), Some(3));
}

#[test]
fn read_one_returns_position() {
    crate::test_utils::init_test_tracing();
    let (val, pos) = read_one("42 rest", 0).unwrap().unwrap();
    assert_eq!(val.as_fixnum(), Some(42));
    assert_eq!(pos, 2);
}

#[test]
fn read_one_empty() {
    crate::test_utils::init_test_tracing();
    let result = read_one("   ", 0).unwrap();
    assert!(result.is_none());
}

#[test]
fn read_one_with_offset() {
    crate::test_utils::init_test_tracing();
    let (val, pos) = read_one("42 99", 3).unwrap().unwrap();
    assert_eq!(val.as_fixnum(), Some(99));
    assert_eq!(pos, 5);
}

// ---------------------------------------------------------------------------
// Complex forms
// ---------------------------------------------------------------------------

#[test]
fn defun_form() {
    crate::test_utils::init_test_tracing();
    let v = read1("(defun my-fn (x) (+ x 1))");
    assert!(v.is_cons());
    assert!(v.cons_car().is_symbol_named("defun"));
}

#[test]
fn mixed_types() {
    crate::test_utils::init_test_tracing();
    let v = read1(r#"(42 3.14 "hello" :key nil t foo)"#);
    assert!(v.is_cons());
    // First: 42
    assert_eq!(v.cons_car().as_fixnum(), Some(42));
}

#[test]
fn dollar_hash_load_file_name() {
    crate::test_utils::init_test_tracing();
    let v = read1("#$");
    assert!(v.is_symbol_named("load-file-name"));
}
