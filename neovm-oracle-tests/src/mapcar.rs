//! Oracle parity tests for `mapcar`, `mapc`, and `mapconcat`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_mapcar_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(mapcar '1+ '(1 2 3 4 5))");
    assert_ok_eq("(2 3 4 5 6)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(mapcar 'car '((a b) (c d) (e f)))");
    assert_ok_eq("(a c e)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(mapcar 'cdr '((a b) (c d) (e f)))");
    assert_ok_eq("((b) (d) (f))", &o, &n);
}

#[test]
fn oracle_prop_mapcar_empty_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(mapcar '1+ nil)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_mapcar_lambda() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(mapcar (lambda (x) (* x x)) '(1 2 3 4 5))");
    assert_ok_eq("(1 4 9 16 25)", &o, &n);

    // lambda with multiple expressions
    let (o, n) =
        eval_oracle_and_neovm("(mapcar (lambda (x) (let ((y (+ x 10))) (* y 2))) '(1 2 3))");
    assert_ok_eq("(22 24 26)", &o, &n);
}

#[test]
fn oracle_prop_mapcar_nested() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // mapcar inside mapcar
    let (o, n) =
        eval_oracle_and_neovm("(mapcar (lambda (row) (mapcar '1+ row)) '((1 2) (3 4) (5 6)))");
    assert_ok_eq("((2 3) (4 5) (6 7))", &o, &n);
}

#[test]
fn oracle_prop_mapcar_with_condition_case() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // mapcar where lambda uses condition-case
    let form = "(mapcar (lambda (x)
                  (condition-case nil
                      (/ 100 x)
                    (arith-error 'inf)))
                '(10 5 0 2))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_mapcar_string_operations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(mapcar 'string-to-number '(\"1\" \"2\" \"42\"))");
    assert_ok_eq("(1 2 42)", &o, &n);
}

#[test]
fn oracle_prop_mapcar_preserves_order() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(
        "(let ((counter 0))
           (mapcar (lambda (x) (setq counter (1+ counter)) (cons counter x))
                   '(a b c d)))",
    );
    assert_ok_eq("((1 . a) (2 . b) (3 . c) (4 . d))", &o, &n);
}

#[test]
fn oracle_prop_mapcar_with_side_effects() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build accumulator through mapcar side effects
    let form = "(let ((acc nil))
                  (mapcar (lambda (x) (setq acc (cons x acc))) '(1 2 3))
                  acc)";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_mapc_returns_original_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // mapc returns its second argument, not mapped results
    let (o, n) = eval_oracle_and_neovm("(let ((lst '(1 2 3))) (eq lst (mapc (lambda (x) x) lst)))");
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_mapc_side_effects_only() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((sum 0))
                  (mapc (lambda (x) (setq sum (+ sum x))) '(10 20 30))
                  sum)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("60", &o, &n);
}

#[test]
fn oracle_prop_mapconcat_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(mapconcat 'number-to-string '(1 2 3) ", ")"#);
    assert_ok_eq(r#""1, 2, 3""#, &o, &n);

    // empty separator
    let (o, n) = eval_oracle_and_neovm(r#"(mapconcat 'number-to-string '(1 2 3) "")"#);
    assert_ok_eq(r#""123""#, &o, &n);
}

#[test]
fn oracle_prop_mapconcat_empty_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(mapconcat 'symbol-name nil "-")"#);
    assert_ok_eq(r#""""#, &o, &n);
}

#[test]
fn oracle_prop_mapconcat_with_lambda() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) =
        eval_oracle_and_neovm(r#"(mapconcat (lambda (x) (format "%d" (* x x))) '(1 2 3 4) "-")"#);
    assert_ok_eq(r#""1-4-9-16""#, &o, &n);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_mapcar_double(
        a in -1000i64..1000i64,
        b in -1000i64..1000i64,
        c in -1000i64..1000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(mapcar (lambda (x) (* x 2)) '({} {} {}))",
            a, b, c
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }
}
