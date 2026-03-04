//! Oracle parity tests for `signal` and error handling patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{
    assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
    run_neovm_eval, run_oracle_eval,
};

#[test]
fn oracle_prop_signal_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // signal raises an error that condition-case catches
    let form = "(condition-case err
                  (signal 'wrong-type-argument '(numberp \"hello\"))
                  (wrong-type-argument (car err)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("wrong-type-argument", &o, &n);
}

#[test]
fn oracle_prop_signal_with_data() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(condition-case err
                  (signal 'error '(\"custom message\"))
                  (error (cdr err)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_signal_void_variable() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(condition-case err
                  (signal 'void-variable '(undefined-var))
                  (void-variable (list (car err) (cadr err))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_signal_caught_by_generic_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Specific signals should be caught by generic error handler
    let form = "(condition-case err
                  (signal 'wrong-type-argument '(integerp nil))
                  (error (car err)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("wrong-type-argument", &o, &n);
}

#[test]
fn oracle_prop_signal_specific_beats_generic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Specific handler takes precedence
    let form = "(condition-case err
                  (signal 'wrong-type-argument '(stringp 42))
                  (wrong-type-argument 'specific)
                  (error 'generic))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("specific", &o, &n);
}

#[test]
fn oracle_prop_signal_arith_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(condition-case err
                  (/ 1 0)
                  (arith-error (car err)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("arith-error", &o, &n);
}

#[test]
fn oracle_prop_signal_chain_of_handlers() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Multiple handlers, only matching one fires
    let form = "(condition-case err
                  (signal 'void-function '(no-such-fn))
                  (wrong-type-argument 'wta)
                  (void-function 'vf)
                  (error 'generic))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("vf", &o, &n);
}

#[test]
fn oracle_prop_signal_nested_condition_case() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Inner handler doesn't match, outer one does
    let form = "(condition-case err
                  (condition-case inner-err
                      (signal 'void-variable '(x))
                    (wrong-type-argument 'inner))
                  (void-variable 'outer))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("outer", &o, &n);
}

#[test]
fn oracle_prop_signal_unwind_protect_runs_cleanup() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((cleaned nil))
                  (condition-case nil
                      (unwind-protect
                          (signal 'error '(\"boom\"))
                        (setq cleaned t))
                    (error nil))
                  cleaned)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_signal_user_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(condition-case err
                  (signal 'user-error '(\"User made a mistake\"))
                  (user-error (cadr err)))";
    assert_oracle_parity_with_bootstrap(form);
}
