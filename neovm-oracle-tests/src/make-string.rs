//! Oracle parity tests for `make-string`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_make_string_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(make-string 5 ?x)");
    assert_ok_eq(r#""xxxxx""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm("(make-string 3 ?A)");
    assert_ok_eq(r#""AAA""#, &o, &n);
}

#[test]
fn oracle_prop_make_string_zero_length() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(make-string 0 ?x)");
    assert_ok_eq(r#""""#, &o, &n);
}

#[test]
fn oracle_prop_make_string_space() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(make-string 4 ?\\s)");
    assert_ok_eq(r#""    ""#, &o, &n);
}

#[test]
fn oracle_prop_make_string_newline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(length (make-string 3 ?\\n))");
}

#[test]
fn oracle_prop_make_string_length_check() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(length (make-string 10 ?z))");
    assert_ok_eq("10", &o, &n);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_make_string_length(
        len in 0usize..50usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(length (make-string {} ?a))", len);
        let expected = format!("OK {}", len);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(oracle.as_str(), expected.as_str());
    }
}
