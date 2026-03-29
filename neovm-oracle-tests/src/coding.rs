//! Oracle parity tests for coding-system primitives.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_coding_system_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(list (coding-system-p 'utf-8-unix) (coding-system-p 'utf-8) (coding-system-base 'utf-8-unix) (coding-system-eol-type 'utf-8-unix) (check-coding-system 'utf-8-unix))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(t t utf-8 0 utf-8-unix)", &oracle, &neovm);
}

#[test]
fn oracle_prop_check_coding_system_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(check-coding-system 1)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_coding_system_unknown_symbol_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(coding-system-base 'neovm-no-such-coding)");
    assert_err_kind(&oracle, &neovm, "coding-system-error");
}
