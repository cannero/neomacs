//! Oracle parity tests for `alist-get` and association list patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

#[test]
fn oracle_prop_alist_get_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(alist-get 'b '((a . 1) (b . 2) (c . 3)))");
}

#[test]
fn oracle_prop_alist_get_missing() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(alist-get 'z '((a . 1) (b . 2)))");
}

#[test]
fn oracle_prop_alist_get_with_default() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(alist-get 'z '((a . 1)) 'default)");
}

#[test]
fn oracle_prop_alist_get_first_match_wins() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(alist-get 'a '((a . 1) (a . 2) (a . 3)))");
}

#[test]
fn oracle_prop_alist_get_with_equal_test() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(alist-get "key" '(("key" . "val") ("other" . "x"))
                              nil nil 'equal)"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_assoc_vs_assq() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // assq uses eq, assoc uses equal
    let form = r#"(list
                    (assq 'a '((a . 1) (b . 2)))
                    (assoc "hello" '(("hello" . 1) ("world" . 2)))
                    (assq "hello" '(("hello" . 1) ("world" . 2))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_assoc_with_test_fn() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(assoc "HELLO" '(("hello" . 1) ("world" . 2))
                         'string-equal)"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_rassq_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(rassq 2 '((a . 1) (b . 2) (c . 3)))");
    assert_ok_eq("(b . 2)", &o, &n);
}

#[test]
fn oracle_prop_rassq_missing() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(rassq 99 '((a . 1) (b . 2)))");
    assert_ok_eq("nil", &o, &n);
}
