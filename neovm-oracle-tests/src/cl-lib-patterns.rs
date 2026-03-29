//! Oracle parity tests for cl-lib patterns commonly used in Elisp.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

#[test]
fn oracle_prop_cl_incf_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // cl-incf is a macro that expands to setq
    // Test the equivalent expansion
    let form = "(let ((x 5))
                  (setq x (+ x 3))
                  x)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("8", &o, &n);
}

#[test]
fn oracle_prop_cl_decf_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((x 10))
                  (setq x (- x 3))
                  x)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("7", &o, &n);
}

#[test]
fn oracle_prop_push_pop_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // push/pop are macros but we can test the expansion pattern
    let form = "(let ((stack nil))
                  (setq stack (cons 'a stack))
                  (setq stack (cons 'b stack))
                  (setq stack (cons 'c stack))
                  (let ((top (car stack)))
                    (setq stack (cdr stack))
                    (list top stack)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(c (b a))", &o, &n);
}

#[test]
fn oracle_prop_cl_loop_collect_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Manual implementation of cl-loop collect pattern
    let form = "(let ((result nil))
                  (dotimes (i 5)
                    (setq result (cons (* i i) result)))
                  (nreverse result))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(0 1 4 9 16)", &o, &n);
}

#[test]
fn oracle_prop_cl_loop_sum_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((total 0))
                  (dolist (x '(1 2 3 4 5))
                    (setq total (+ total x)))
                  total)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("15", &o, &n);
}

#[test]
fn oracle_prop_cl_loop_count_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((count 0))
                  (dolist (x '(1 -2 3 -4 5 -6))
                    (when (> x 0)
                      (setq count (1+ count))))
                  count)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("3", &o, &n);
}

#[test]
fn oracle_prop_cl_loop_maximize_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((best nil))
                  (dolist (x '(3 1 4 1 5 9 2 6))
                    (when (or (null best) (> x best))
                      (setq best x)))
                  best)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("9", &o, &n);
}

#[test]
fn oracle_prop_cl_loop_append_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((result nil))
                  (dolist (x '((1 2) (3 4) (5 6)))
                    (setq result (append result x)))
                  result)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 2 3 4 5 6)", &o, &n);
}

#[test]
fn oracle_prop_cl_every_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // cl-every: all elements satisfy predicate
    let form = "(let ((all-positive t))
                  (dolist (x '(1 2 3 4 5))
                    (unless (> x 0)
                      (setq all-positive nil)))
                  all-positive)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_cl_some_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // cl-some: at least one element satisfies predicate
    let form = "(catch 'found
                  (dolist (x '(1 -2 3 -4 5))
                    (when (< x 0)
                      (throw 'found x)))
                  nil)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("-2", &o, &n);
}

#[test]
fn oracle_prop_cl_remove_if_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((result nil))
                  (dolist (x '(1 -2 3 -4 5 -6))
                    (unless (< x 0)
                      (setq result (cons x result))))
                  (nreverse result))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 3 5)", &o, &n);
}
