//! Oracle parity tests for `plist-get`, `plist-put`, `plist-member`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

#[test]
fn oracle_prop_plist_get_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(plist-get '(:a 1 :b 2 :c 3) :b)");
    assert_ok_eq("2", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(plist-get '(:a 1 :b 2 :c 3) :a)");
    assert_ok_eq("1", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(plist-get '(:a 1 :b 2 :c 3) :c)");
    assert_ok_eq("3", &o, &n);
}

#[test]
fn oracle_prop_plist_get_missing_key() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(plist-get '(:a 1 :b 2) :z)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_plist_get_empty() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(plist-get nil :a)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_plist_put_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((pl '(:a 1 :b 2)))
                  (plist-get (plist-put pl :c 3) :c))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("3", &o, &n);
}

#[test]
fn oracle_prop_plist_put_overwrite() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((pl '(:a 1 :b 2)))
                  (plist-get (plist-put pl :a 99) :a))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("99", &o, &n);
}

#[test]
fn oracle_prop_plist_member_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // plist-member returns the tail starting from the matching key
    assert_oracle_parity("(plist-member '(:a 1 :b 2 :c 3) :b)");
}

#[test]
fn oracle_prop_plist_member_missing() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(plist-member '(:a 1 :b 2) :z)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_plist_chained_puts() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let* ((pl nil)
                       (pl (plist-put pl :x 10))
                       (pl (plist-put pl :y 20))
                       (pl (plist-put pl :z 30)))
                  (list (plist-get pl :x)
                        (plist-get pl :y)
                        (plist-get pl :z)))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_plist_with_non_keyword_keys() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // plist-get/plist-put work with any eq-comparable keys
    let (o, n) = eval_oracle_and_neovm("(plist-get '(a 1 b 2 c 3) 'b)");
    assert_ok_eq("2", &o, &n);
}

#[test]
fn oracle_prop_plist_complex_values() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(plist-get '(:data (1 2 3) :name \"test\" :flag t) :data)";
    assert_oracle_parity(form);
}
