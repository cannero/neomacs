//! Oracle parity tests for `condition-case`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

#[test]
fn oracle_prop_condition_case_handles_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(condition-case nil (/ 1 0) (arith-error 42))");
    assert_ok_eq("42", &oracle, &neovm);
}

#[test]
fn oracle_prop_condition_case_no_error_passthrough() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(condition-case nil (+ 1 2) (error 0))");
    assert_ok_eq("3", &oracle, &neovm);
}

#[test]
fn oracle_prop_condition_case_error_symbol_binding() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(condition-case err (/ 1 0) (arith-error (car err)))");
}
