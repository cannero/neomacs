//! Oracle parity tests for backquote / quasiquote expansion.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_backquote_simple() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("`(1 2 3)");
    assert_ok_eq("(1 2 3)", &o, &n);
}

#[test]
fn oracle_prop_backquote_with_comma() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(let ((x 42)) `(a ,x c))");
    assert_ok_eq("(a 42 c)", &o, &n);
}

#[test]
fn oracle_prop_backquote_with_comma_at() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(let ((xs '(2 3 4))) `(1 ,@xs 5))");
    assert_ok_eq("(1 2 3 4 5)", &o, &n);
}

#[test]
fn oracle_prop_backquote_nested_comma() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((a 1) (b 2) (c 3))
                  `(,a (,b ,c)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 (2 3))", &o, &n);
}

#[test]
fn oracle_prop_backquote_splice_empty() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(let ((xs nil)) `(a ,@xs b))");
    assert_ok_eq("(a b)", &o, &n);
}

#[test]
fn oracle_prop_backquote_in_defmacro() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Common pattern: backquote in macro definition
    let form = "(progn
                  (defmacro neovm--test-bq-when (cond &rest body)
                    `(if ,cond (progn ,@body)))
                  (unwind-protect
                      (list (neovm--test-bq-when t 1 2 3)
                            (neovm--test-bq-when nil 'unreachable))
                    (fmakunbound 'neovm--test-bq-when)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_backquote_multiple_splices() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((a '(1 2)) (b '(3 4)) (c '(5 6)))
                  `(,@a ,@b ,@c))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 2 3 4 5 6)", &o, &n);
}

#[test]
fn oracle_prop_backquote_with_dot() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((x 42)) `(a . ,x))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(a . 42)", &o, &n);
}

#[test]
fn oracle_prop_backquote_vector() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((x 2)) `[1 ,x 3])";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_backquote_let_binding_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Common pattern: generating let bindings via backquote
    let form = "(progn
                  (defmacro neovm--test-bq-with-temp (var val &rest body)
                    `(let ((,var ,val)) ,@body))
                  (unwind-protect
                      (neovm--test-bq-with-temp x 42 (+ x 1))
                    (fmakunbound 'neovm--test-bq-with-temp)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("43", &o, &n);
}

#[test]
fn oracle_prop_backquote_condition_case_macro() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Macro that generates condition-case forms
    let form = "(progn
                  (defmacro neovm--test-bq-safe (expr fallback)
                    `(condition-case nil ,expr (error ,fallback)))
                  (unwind-protect
                      (list (neovm--test-bq-safe (+ 1 2) 'oops)
                            (neovm--test-bq-safe (car 1) 'oops))
                    (fmakunbound 'neovm--test-bq-safe)))";
    assert_oracle_parity_with_bootstrap(form);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_backquote_comma_eval(
        a in -1000i64..1000i64,
        b in -1000i64..1000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(let ((x {}) (y {})) `(,x ,y ,(+ x y)))", a, b);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }
}
