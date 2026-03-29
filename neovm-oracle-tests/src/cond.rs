//! Oracle parity tests for `cond`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_cond_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // first clause matches
    let (o, n) = eval_oracle_and_neovm("(cond (t 'yes))");
    assert_ok_eq("yes", &o, &n);

    // second clause matches
    let (o, n) = eval_oracle_and_neovm("(cond (nil 'no) (t 'yes))");
    assert_ok_eq("yes", &o, &n);

    // no match returns nil
    let (o, n) = eval_oracle_and_neovm("(cond (nil 'a) (nil 'b))");
    assert_ok_eq("nil", &o, &n);

    // empty cond
    let (o, n) = eval_oracle_and_neovm("(cond)");
    assert_ok_eq("nil", &o, &n);

    // clause with multiple body forms
    let (o, n) = eval_oracle_and_neovm("(cond (t 1 2 3))");
    assert_ok_eq("3", &o, &n);

    // test value returned when no body
    let (o, n) = eval_oracle_and_neovm("(cond (42))");
    assert_ok_eq("42", &o, &n);

    // numeric test
    let (o, n) = eval_oracle_and_neovm(
        "(let ((x 3)) (cond ((= x 1) 'one) ((= x 2) 'two) ((= x 3) 'three) (t 'other)))",
    );
    assert_ok_eq("three", &o, &n);

    // side effects only in matching clause
    let (o, n) = eval_oracle_and_neovm("(let ((v 0)) (cond (nil (setq v 1)) (t (setq v 2))) v)");
    assert_ok_eq("2", &o, &n);
}
