//! Oracle parity tests for closure/oclosure-related behavior.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_closure_primitives_are_consistent() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(
        "(list (fboundp 'closurep) (fboundp 'make-closure) (fboundp 'make-interpreted-closure))",
    );
}

#[test]
fn oracle_prop_closurep_on_common_values() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(
        "(list (closurep 1) (closurep 'x) (closurep '(lambda (x) x)))",
    );
}

#[test]
fn oracle_prop_make_interpreted_closure_basic_callable() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((f (make-interpreted-closure '(x) '((+ x 1)) nil))) (list (closurep f) (funcall f 41)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(t 42)", &oracle, &neovm);
}

#[test]
fn oracle_prop_make_interpreted_closure_lexenv_binding() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // LEXENV argument should provide a lexical binding for `x`.
    assert_oracle_parity_with_bootstrap(
        "(let ((f (make-interpreted-closure '() '(x) '((x . 9))))) (funcall f))",
    );
}

#[test]
fn oracle_prop_make_closure_invalid_argument_shape_errors() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(
        "(condition-case err (make-closure nil) (error (car err)))",
    );
    assert_oracle_parity_with_bootstrap(
        "(condition-case err (make-closure 1 2 3) (error (car err)))",
    );
}

#[test]
fn oracle_prop_oclosure_macros_presence_matches_oracle() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(
        "(list (fboundp 'oclosure-define) (fboundp 'oclosure-lambda))",
    );
}

#[test]
fn oracle_prop_oclosure_define_creates_callable_type_and_accessor() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
      (oclosure-define neovm-oracle-oclosure-define-test slot)
      (list
        (fboundp 'neovm-oracle-oclosure-define-test--slot)
        (fboundp 'neovm-oracle-oclosure-define-test--internal-p)
        (condition-case nil
            (not (null (cl--find-class 'neovm-oracle-oclosure-define-test)))
          (error nil))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_oclosure_macroexpand_when_available() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // In this minimal harness these may be unavailable; when available,
    // macroexpand should still match between oracle and neovm.
    let form = "(if (and (fboundp 'oclosure-lambda) (fboundp 'macroexpand)) (macroexpand '(oclosure-lambda neovm--oc-test (self) self)) 'oclosure-unavailable)";
    assert_oracle_parity_with_bootstrap(form);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_make_interpreted_closure_arithmetic_payload(
        base in -100_000i64..100_000i64,
        arg in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(let ((f (make-interpreted-closure '(x) '((+ x {})) nil))) (funcall f {}))",
            base, arg
        );
        let expected = (base + arg).to_string();
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
