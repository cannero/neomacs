//! Oracle parity tests for anonymous lambda behavior.

use proptest::prelude::*;
use std::sync::OnceLock;

use super::common::{
    ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap,
    eval_oracle_and_neovm, return_if_neovm_enable_oracle_proptest_not_set,
};

fn oracle_lambda_anonymous_proptest_failure_path() -> &'static str {
    static PATH: OnceLock<&'static str> = OnceLock::new();
    PATH.get_or_init(|| {
        let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".to_string());
        Box::leak(
            format!("{target_dir}/proptest-regressions/emacs_core/oracle/lambda-anonymous.txt")
                .into_boxed_str(),
        )
    })
}

#[test]
fn oracle_prop_lambda_closure_mutates_captured_binding() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((x 1) (f nil)) (setq f (lambda () (setq x (+ x 1)))) (list (funcall f) (funcall f) x))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(2 3 3)", &oracle, &neovm);
}

#[test]
fn oracle_prop_lambda_multiple_closures_share_state() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((x 0) inc get) (setq inc (lambda () (setq x (1+ x)))) (setq get (lambda () x)) (funcall inc) (funcall inc) (funcall get))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("2", &oracle, &neovm);
}

#[test]
fn oracle_prop_lambda_returns_lambda_and_captures_parameter() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(
        "(let ((mk (lambda (n) (lambda (x) (+ x n))))) (let ((f (funcall mk 3))) (funcall f 4)))",
    );
}

#[test]
fn oracle_prop_lambda_self_application_recursion() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(funcall (lambda (self n) (if (= n 0) 0 (+ n (funcall self self (1- n))))) (lambda (self n) (if (= n 0) 0 (+ n (funcall self self (1- n))))) 5)";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("15", &oracle, &neovm);
}

#[test]
fn oracle_prop_lambda_in_list_selection_and_call() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((fns (list (lambda (x) (+ x 1)) (lambda (x) (+ x 10))))) (funcall (car (cdr fns)) 5))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("15", &oracle, &neovm);
}

#[test]
fn oracle_prop_lambda_apply_funcall_equivalence() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form =
        "(let ((f (lambda (a b c) (+ a (* b c))))) (list (funcall f 2 3 4) (apply f '(2 3 4))))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(14 14)", &oracle, &neovm);
}

#[test]
fn oracle_prop_lambda_parameter_shadowing_and_nested_call() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) =
        eval_oracle_and_neovm("(let ((x 1)) (funcall (lambda (x) (funcall (lambda () x))) 2))");
    assert_ok_eq("2", &oracle, &neovm);
}

#[test]
fn oracle_prop_lambda_wrong_arity_and_invalid_param_list_errors() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_arity, neovm_arity) = eval_oracle_and_neovm("(funcall (lambda (x y) (+ x y)) 1)");
    assert_err_kind(&oracle_arity, &neovm_arity, "wrong-number-of-arguments");

    let (oracle_invalid, neovm_invalid) =
        eval_oracle_and_neovm("(funcall (lambda ((x . y)) x) '(1 . 2))");
    assert_err_kind(&oracle_invalid, &neovm_invalid, "invalid-function");
}

#[test]
fn oracle_prop_lambda_function_form_callable() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(funcall (function (lambda (x) (+ x 2))) 5)");
    assert_ok_eq("7", &oracle, &neovm);
}

#[test]
fn oracle_prop_lambda_free_var_uses_dynamic_call_site_binding() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((f (lambda (x) (+ x y))) (y 9)) (funcall f 4))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("13", &oracle, &neovm);
}

#[test]
fn oracle_prop_lambda_free_var_without_dynamic_binding_errors() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(funcall (lambda () y))");
    assert_err_kind(&oracle, &neovm, "void-variable");
}

#[test]
fn oracle_prop_lambda_direct_call_form_is_callable() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("((lambda (x y) (+ x y)) 5 8)");
    assert_ok_eq("13", &oracle, &neovm);
}

#[test]
fn oracle_prop_lambda_mapcar_with_dynamic_variable_reference() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((k 3)) (mapcar (lambda (x) (+ x k)) '(1 2 3)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(4 5 6)", &oracle, &neovm);
}

proptest! {
    #![proptest_config({
        let mut config = proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES);
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(
                oracle_lambda_anonymous_proptest_failure_path(),
            ),
        ));
        config
    })]

    #[test]
    fn oracle_prop_lambda_higher_order_addition(
        n in -100_000i64..100_000i64,
        x in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(funcall (funcall (lambda (n) (lambda (x) (+ x n))) {}) {})",
            n, x
        );
        assert_oracle_parity_with_bootstrap(&form);
    }

    #[test]
    fn oracle_prop_lambda_optional_rest_shape(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
        c in -100_000i64..100_000i64,
        d in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(funcall (lambda (a &optional b &rest xs) (list a b (length xs) (car xs) (car (cdr xs)))) {} {} {} {})",
            a, b, c, d
        );
        let expected = format!("({} {} 2 {} {})", a, b, c, d);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_lambda_self_application_sum_n(
        n in 0i64..50i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(funcall (lambda (self n) (if (= n 0) 0 (+ n (funcall self self (1- n))))) (lambda (self n) (if (= n 0) 0 (+ n (funcall self self (1- n))))) {})",
            n
        );
        let expected = (n * (n + 1) / 2).to_string();
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
