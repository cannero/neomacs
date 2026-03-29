//! Oracle parity tests for `funcall`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_funcall_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // simple lambda
    let (o, n) = eval_oracle_and_neovm("(funcall (lambda (x) (* x x)) 7)");
    assert_ok_eq("49", &o, &n);

    // no-arg lambda
    let (o, n) = eval_oracle_and_neovm("(funcall (lambda () 42))");
    assert_ok_eq("42", &o, &n);

    // multiple args
    let (o, n) = eval_oracle_and_neovm("(funcall (lambda (a b c) (+ a b c)) 10 20 30)");
    assert_ok_eq("60", &o, &n);

    // optional args
    let (o, n) = eval_oracle_and_neovm("(funcall (lambda (x &optional y) (if y (+ x y) x)) 5)");
    assert_ok_eq("5", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(funcall (lambda (x &optional y) (if y (+ x y) x)) 5 3)");
    assert_ok_eq("8", &o, &n);

    // rest args
    let (o, n) = eval_oracle_and_neovm("(funcall (lambda (&rest xs) (length xs)) 1 2 3 4)");
    assert_ok_eq("4", &o, &n);

    // funcall with named function
    let (o, n) = eval_oracle_and_neovm("(funcall '+ 100 200)");
    assert_ok_eq("300", &o, &n);

    // funcall with sharp-quote
    let (o, n) = eval_oracle_and_neovm("(funcall #'car '(first second))");
    assert_ok_eq("first", &o, &n);

    // closure capturing lexical binding
    let (o, n) = eval_oracle_and_neovm("(let ((offset 10)) (funcall (lambda (n) (+ n offset)) 5))");
    assert_ok_eq("15", &o, &n);
}
