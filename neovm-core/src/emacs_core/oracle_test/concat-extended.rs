//! Oracle parity tests for `concat` extended patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_concat_strings() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(concat "hello" " " "world")"#);
    assert_ok_eq(r#""hello world""#, &o, &n);
}

#[test]
fn oracle_prop_concat_empty() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(concat)");
    assert_ok_eq(r#""""#, &o, &n);
}

#[test]
fn oracle_prop_concat_single() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(concat "only")"#);
    assert_ok_eq(r#""only""#, &o, &n);
}

#[test]
fn oracle_prop_concat_many() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(concat "a" "b" "c" "d" "e" "f")"#);
    assert_ok_eq(r#""abcdef""#, &o, &n);
}

#[test]
fn oracle_prop_concat_with_empty_strings() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(concat "" "hi" "" "!" "")"#);
    assert_ok_eq(r#""hi!""#, &o, &n);
}

#[test]
fn oracle_prop_concat_with_format() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(concat (format "%d" 42) "-" (format "%s" "hello"))"####;
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq(r#""42-hello""#, &o, &n);
}

#[test]
fn oracle_prop_concat_with_number_to_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(concat "[" (number-to-string 42) "]")"####;
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq(r#""[42]""#, &o, &n);
}

#[test]
fn oracle_prop_concat_in_loop() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(let ((result ""))
                    (dotimes (i 5)
                      (setq result (concat result (number-to-string i))))
                    result)"####;
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq(r#""01234""#, &o, &n);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_concat_lengths_add(
        a_len in 0usize..10usize,
        b_len in 0usize..10usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(length (concat (make-string {} ?a) (make-string {} ?b)))",
            a_len, b_len
        );
        let expected = format!("OK {}", a_len + b_len);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(oracle.as_str(), expected.as_str());
    }
}
