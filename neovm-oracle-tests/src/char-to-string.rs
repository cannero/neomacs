//! Oracle parity tests for `char-to-string` and `string-to-char`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

#[test]
fn oracle_prop_char_to_string_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(char-to-string ?A)");
    assert_ok_eq(r#""A""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm("(char-to-string ?z)");
    assert_ok_eq(r#""z""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm("(char-to-string ?0)");
    assert_ok_eq(r#""0""#, &o, &n);
}

#[test]
fn oracle_prop_char_to_string_space() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(char-to-string ?\\s)");
    assert_ok_eq(r#"" ""#, &o, &n);
}

#[test]
fn oracle_prop_string_to_char_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(string-to-char "A")"#);
    assert_ok_eq("65", &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(string-to-char "hello")"#);
    assert_ok_eq("104", &o, &n);
}

#[test]
fn oracle_prop_string_to_char_empty() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(string-to-char "")"#);
    assert_ok_eq("0", &o, &n);
}

#[test]
fn oracle_prop_char_to_string_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(string-to-char (char-to-string ?X))"#);
    assert_ok_eq("88", &o, &n);
}

#[test]
fn oracle_prop_char_to_string_in_concat() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(concat (char-to-string ?H) (char-to-string ?i) (char-to-string ?!))"####;
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq(r#""Hi!""#, &o, &n);
}
