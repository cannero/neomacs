use super::*;

#[test]
fn print_basic_exprs() {
    crate::test_utils::init_test_tracing();
    assert_eq!(print_expr(&Expr::Int(42)), "42");
    assert_eq!(print_expr(&Expr::Float(3.14)), "3.14");
    assert_eq!(print_expr(&Expr::Symbol(intern("foo"))), "foo");
    assert_eq!(print_expr(&Expr::Symbol(intern(".foo"))), "\\.foo");
    assert_eq!(print_expr(&Expr::Symbol(intern(""))), "##");
    assert_eq!(print_expr(&Expr::Str("hello".into())), "\"hello\"");
}

#[test]
fn print_symbol_escapes_reader_sensitive_chars() {
    crate::test_utils::init_test_tracing();
    assert_eq!(print_expr(&Expr::Symbol(intern("a b"))), "a\\ b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a,b"))), "a\\,b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a,@b"))), "a\\,@b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a#b"))), "a\\#b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a'b"))), "a\\'b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a`b"))), "a\\`b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a\\b"))), "a\\\\b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a\"b"))), "a\\\"b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a(b"))), "a\\(b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a)b"))), "a\\)b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a[b"))), "a\\[b");
    assert_eq!(print_expr(&Expr::Symbol(intern("a]b"))), "a\\]b");
    assert_eq!(print_expr(&Expr::Symbol(intern("##"))), "\\#\\#");
    assert_eq!(print_expr(&Expr::Symbol(intern("?a"))), "\\?a");
    assert_eq!(print_expr(&Expr::Symbol(intern("a?b"))), "a?b");
}

#[test]
fn print_list() {
    crate::test_utils::init_test_tracing();
    let expr = Expr::List(vec![Expr::Symbol(intern("+")), Expr::Int(1), Expr::Int(2)]);
    assert_eq!(print_expr(&expr), "(+ 1 2)");
}

#[test]
fn print_quote_shorthand() {
    crate::test_utils::init_test_tracing();
    let expr = Expr::List(vec![
        Expr::Symbol(intern("quote")),
        Expr::Symbol(intern("foo")),
    ]);
    assert_eq!(print_expr(&expr), "'foo");
}

#[test]
fn print_vector() {
    crate::test_utils::init_test_tracing();
    let expr = Expr::Vector(vec![Expr::Int(1), Expr::Int(2)]);
    assert_eq!(print_expr(&expr), "[1 2]");
}

#[test]
fn print_string_keeps_non_bmp_visible() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        print_expr(&Expr::Str("\u{10ffff}".into())),
        "\"\u{10ffff}\""
    );
}

#[test]
fn print_special_float_spellings_match_oracle_shape() {
    crate::test_utils::init_test_tracing();
    assert_eq!(print_expr(&Expr::Float(f64::NAN)), "0.0e+NaN");
    let neg_nan = -f64::NAN;
    assert_eq!(print_expr(&Expr::Float(neg_nan)), "-0.0e+NaN");
    assert_eq!(print_expr(&Expr::Float(f64::INFINITY)), "1.0e+INF");
    assert_eq!(print_expr(&Expr::Float(f64::NEG_INFINITY)), "-1.0e+INF");

    let tagged = f64::from_bits((0x7ffu64 << 52) | (1u64 << 51) | 1u64);
    assert_eq!(print_expr(&Expr::Float(tagged)), "1.0e+NaN");
    let neg_tagged = f64::from_bits((1u64 << 63) | (0x7ffu64 << 52) | (1u64 << 51) | 2u64);
    assert_eq!(print_expr(&Expr::Float(neg_tagged)), "-2.0e+NaN");
}
