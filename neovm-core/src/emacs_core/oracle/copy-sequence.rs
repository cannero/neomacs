//! Oracle parity tests for `copy-sequence`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

#[test]
fn oracle_prop_copy_sequence_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(copy-sequence '(1 2 3))");
    assert_ok_eq("(1 2 3)", &o, &n);
}

#[test]
fn oracle_prop_copy_sequence_not_eq() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((lst '(1 2 3)))
                  (eq lst (copy-sequence lst)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_copy_sequence_equal() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((lst '(1 2 3)))
                  (equal lst (copy-sequence lst)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_copy_sequence_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(copy-sequence "hello")"#);
    assert_ok_eq(r#""hello""#, &o, &n);
}

#[test]
fn oracle_prop_copy_sequence_vector() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(copy-sequence [1 2 3])");
    assert_ok_eq("[1 2 3]", &o, &n);
}

#[test]
fn oracle_prop_copy_sequence_nil() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(copy-sequence nil)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_copy_sequence_empty_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(copy-sequence "")"#);
    assert_ok_eq(r#""""#, &o, &n);
}

#[test]
fn oracle_prop_copy_sequence_mutation_independence() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Mutating the copy should not affect the original
    let form = "(let* ((orig '(1 2 3))
                       (copy (copy-sequence orig)))
                  (setcar copy 99)
                  (list orig copy))";
    assert_oracle_parity(form);
}
