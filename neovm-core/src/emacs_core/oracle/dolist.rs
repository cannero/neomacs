//! Oracle parity tests for `dolist`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

#[test]
fn oracle_prop_dolist_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((sum 0))
                  (dolist (x '(1 2 3 4 5))
                    (setq sum (+ sum x)))
                  sum)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("15", &o, &n);
}

#[test]
fn oracle_prop_dolist_empty() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((count 0))
                  (dolist (x nil)
                    (setq count (1+ count)))
                  count)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("0", &o, &n);
}

#[test]
fn oracle_prop_dolist_with_result() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((sum 0))
                  (dolist (x '(10 20 30) sum)
                    (setq sum (+ sum x))))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("60", &o, &n);
}

#[test]
fn oracle_prop_dolist_collect_reversed() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((result nil))
                  (dolist (x '(a b c d))
                    (setq result (cons x result)))
                  result)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(d c b a)", &o, &n);
}

#[test]
fn oracle_prop_dolist_filter_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Common pattern: filter + collect with dolist
    let form = "(let ((evens nil))
                  (dolist (x '(1 2 3 4 5 6 7 8))
                    (when (= 0 (% x 2))
                      (setq evens (cons x evens))))
                  (nreverse evens))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(2 4 6 8)", &o, &n);
}

#[test]
fn oracle_prop_dolist_map_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Map + collect
    let form = "(let ((result nil))
                  (dolist (x '(1 2 3 4 5))
                    (setq result (cons (* x x) result)))
                  (nreverse result))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 4 9 16 25)", &o, &n);
}

#[test]
fn oracle_prop_dolist_nested() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Nested dolist
    let form = "(let ((pairs nil))
                  (dolist (x '(a b))
                    (dolist (y '(1 2))
                      (setq pairs (cons (cons x y) pairs))))
                  (nreverse pairs))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_dolist_with_condition_case() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((results nil))
                  (dolist (x '(1 0 3 0 5))
                    (setq results
                          (cons (condition-case nil
                                    (/ 10 x)
                                  (arith-error 'inf))
                                results)))
                  (nreverse results))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_dolist_returns_nil_by_default() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(dolist (x '(1 2 3)))");
    assert_ok_eq("nil", &o, &n);
}
