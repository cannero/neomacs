//! Oracle parity tests for `prog1`.
//!
//! Note: `prog2` is a Lisp macro defined in `subr.el` (not a C primitive),
//! so it is not available in the bare `Context::new()` used by oracle tests.
//! It is tested via full neomacs which loads `subr.el`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_prog1_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(prog1 10 20 30)");
    assert_ok_eq("10", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(prog1 'first)");
    assert_ok_eq("first", &o, &n);

    // side effects still happen
    let (o, n) = eval_oracle_and_neovm("(let ((x 0)) (prog1 x (setq x 99)) )");
    assert_ok_eq("0", &o, &n);

    // prog1 with no body forms
    let (o, n) = eval_oracle_and_neovm("(prog1 42)");
    assert_ok_eq("42", &o, &n);
}
