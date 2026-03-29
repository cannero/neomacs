//! Oracle parity tests for `make-list`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap,
    eval_oracle_and_neovm, eval_oracle_and_neovm_with_bootstrap, run_neovm_eval, run_oracle_eval,
};

#[test]
fn oracle_prop_make_list_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm_with_bootstrap("(make-list 5 0)");
    assert_ok_eq("(0 0 0 0 0)", &o, &n);

    let (o, n) = eval_oracle_and_neovm_with_bootstrap("(make-list 3 'x)");
    assert_ok_eq("(x x x)", &o, &n);
}

#[test]
fn oracle_prop_make_list_zero_length() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm_with_bootstrap("(make-list 0 'anything)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_make_list_negative_length() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Emacs signals error for negative length
    let form = "(make-list -1 'x)";
    let oracle = run_oracle_eval(form).expect("oracle eval should run");
    let neovm = run_neovm_eval(form).expect("neovm eval should run");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_make_list_with_nil() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm_with_bootstrap("(make-list 4 nil)");
    assert_ok_eq("(nil nil nil nil)", &o, &n);
}

#[test]
fn oracle_prop_make_list_with_complex_init() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Same object repeated — all elements eq
    let form = "(let ((lst (make-list 3 '(a b))))
                  (eq (car lst) (cadr lst)))";
    let (o, n) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_make_list_length_check() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm_with_bootstrap("(length (make-list 10 42))");
    assert_ok_eq("10", &o, &n);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_make_list_length(
        len in 0usize..20usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(length (make-list {} 'x))", len);
        let expected = format!("OK {}", len);
        let (oracle, neovm) = eval_oracle_and_neovm_with_bootstrap(&form);
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(oracle.as_str(), expected.as_str());
    }
}
