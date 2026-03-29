//! Oracle parity tests for `type-of`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

#[test]
fn oracle_prop_type_of_integer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(type-of 42)");
    assert_ok_eq("integer", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(type-of 0)");
    assert_ok_eq("integer", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(type-of -1)");
    assert_ok_eq("integer", &o, &n);
}

#[test]
fn oracle_prop_type_of_float() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(type-of 3.14)");
    assert_ok_eq("float", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(type-of 0.0)");
    assert_ok_eq("float", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(type-of -1.5)");
    assert_ok_eq("float", &o, &n);
}

#[test]
fn oracle_prop_type_of_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(type-of "hello")"#);
    assert_ok_eq("string", &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(type-of "")"#);
    assert_ok_eq("string", &o, &n);
}

#[test]
fn oracle_prop_type_of_symbol() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(type-of 'foo)");
    assert_ok_eq("symbol", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(type-of t)");
    assert_ok_eq("symbol", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(type-of nil)");
    assert_ok_eq("symbol", &o, &n);
}

#[test]
fn oracle_prop_type_of_cons() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(type-of '(1 2 3))");
    assert_ok_eq("cons", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(type-of (cons 'a 'b))");
    assert_ok_eq("cons", &o, &n);
}

#[test]
fn oracle_prop_type_of_vector() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(type-of [1 2 3])");
    assert_ok_eq("vector", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(type-of [])");
    assert_ok_eq("vector", &o, &n);
}

#[test]
fn oracle_prop_type_of_hash_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(type-of (make-hash-table))");
}

#[test]
fn oracle_prop_type_of_char_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(type-of (make-char-table 'foo))");
}

#[test]
fn oracle_prop_type_of_in_conditional() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use type-of for dispatching
    let form = "(let ((val '(1 2 3)))
                  (cond
                    ((eq (type-of val) 'integer) 'int)
                    ((eq (type-of val) 'cons) 'list)
                    ((eq (type-of val) 'string) 'str)
                    (t 'other)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("list", &o, &n);
}

#[test]
fn oracle_prop_type_of_mapped_over_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(mapcar 'type-of (list 1 "s" 'sym '(a) [v] 3.0))"####;
    assert_oracle_parity_with_bootstrap(form);
}
