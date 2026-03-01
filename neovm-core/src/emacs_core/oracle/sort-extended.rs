//! Oracle parity tests for `sort` with complex predicates and patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{assert_oracle_parity, eval_oracle_and_neovm, ORACLE_PROP_CASES};

#[test]
fn oracle_prop_sort_by_abs() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(sort (list 3 -1 4 -1 5 -9 2 -6)
                      (lambda (a b) (< (abs a) (abs b))))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_sort_strings_by_length() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(sort (list "cc" "aaa" "b" "dddd" "")
                        (lambda (a b)
                          (< (length a) (length b))))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_sort_alist_by_cdr() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(sort (list '(a . 3) '(b . 1) '(c . 4) '(d . 1) '(e . 5))
                      (lambda (x y) (< (cdr x) (cdr y))))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_sort_stable_for_equal_elements() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Sort pairs by first element; second element shows original order
    let form = "(sort (list '(1 . a) '(2 . b) '(1 . c) '(2 . d) '(1 . e))
                      (lambda (x y) (< (car x) (car y))))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_sort_reverse_order() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(sort (list 1 5 3 2 4) '>)";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_sort_with_string_lessp() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(sort (list "banana" "apple" "cherry" "date")
                        'string-lessp)"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_sort_single_element() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(sort (list 42) '<)";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_sort_empty() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(sort nil '<)";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_sort_with_multi_key() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Sort by first key, then second key
    let form = "(sort (list '(1 b) '(2 a) '(1 a) '(2 b) '(1 c))
                      (lambda (x y)
                        (or (< (car x) (car y))
                            (and (= (car x) (car y))
                                 (string-lessp
                                  (symbol-name (cadr x))
                                  (symbol-name (cadr y)))))))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_sort_is_destructive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // sort is destructive — result may differ from input pointer
    let form = "(let ((lst (list 3 1 4 1 5)))
                  (let ((sorted (sort lst '<)))
                    (equal sorted '(1 1 3 4 5))))";
    assert_oracle_parity(form);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_sort_ascending(
        a in -100i64..100i64,
        b in -100i64..100i64,
        c in -100i64..100i64,
        d in -100i64..100i64,
        e in -100i64..100i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(sort (list {} {} {} {} {}) '<)",
            a, b, c, d, e
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }
}
