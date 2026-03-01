//! Oracle parity tests for `sequencep` and sequence-related predicates.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_sequencep_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(sequencep '(1 2 3))");
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_sequencep_nil() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(sequencep nil)");
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_sequencep_vector() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(sequencep [1 2 3])");
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_sequencep_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(sequencep "hello")"#);
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_sequencep_integer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(sequencep 42)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_sequencep_symbol() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(sequencep 'foo)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_sequencep_hash_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(sequencep (make-hash-table))");
    assert_ok_eq("nil", &o, &n);
}
