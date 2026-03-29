//! Oracle parity tests for `upcase`, `downcase`, `capitalize`, and related.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

#[test]
fn oracle_prop_upcase_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(upcase "hello")"#);
    assert_ok_eq(r#""HELLO""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(upcase "Hello World")"#);
    assert_ok_eq(r#""HELLO WORLD""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(upcase "ALREADY")"#);
    assert_ok_eq(r#""ALREADY""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(upcase "")"#);
    assert_ok_eq(r#""""#, &o, &n);
}

#[test]
fn oracle_prop_downcase_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(downcase "HELLO")"#);
    assert_ok_eq(r#""hello""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(downcase "Hello World")"#);
    assert_ok_eq(r#""hello world""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(downcase "already")"#);
    assert_ok_eq(r#""already""#, &o, &n);
}

#[test]
fn oracle_prop_upcase_char() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(upcase ?a)");
    assert_ok_eq("65", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(upcase ?A)");
    assert_ok_eq("65", &o, &n);
}

#[test]
fn oracle_prop_downcase_char() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(downcase ?A)");
    assert_ok_eq("97", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(downcase ?a)");
    assert_ok_eq("97", &o, &n);
}

#[test]
fn oracle_prop_capitalize_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(capitalize "hello world")"#);
    assert_ok_eq(r#""Hello World""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(capitalize "HELLO WORLD")"#);
    assert_ok_eq(r#""Hello World""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(capitalize "hello")"#);
    assert_ok_eq(r#""Hello""#, &o, &n);
}

#[test]
fn oracle_prop_upcase_downcase_with_numbers() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(upcase "abc123def")"#);
    assert_ok_eq(r#""ABC123DEF""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(downcase "ABC123DEF")"#);
    assert_ok_eq(r#""abc123def""#, &o, &n);
}

#[test]
fn oracle_prop_upcase_downcase_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(string-equal (downcase (upcase "hello")) "hello")"####;
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_upcase_initials() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(upcase-initials "hello world")"#);
    assert_ok_eq(r#""Hello World""#, &o, &n);

    // upcase-initials only capitalizes first letter of each word, preserves rest
    let (o, n) = eval_oracle_and_neovm(r#"(upcase-initials "hELLO wORLD")"#);
    assert_ok_eq(r#""HELLO WORLD""#, &o, &n);
}

#[test]
fn oracle_prop_mapcar_upcase() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(mapcar 'upcase '("foo" "bar" "baz"))"####;
    assert_oracle_parity_with_bootstrap(form);
}
