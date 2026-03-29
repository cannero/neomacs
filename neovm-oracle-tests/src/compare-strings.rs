//! Oracle parity tests for `compare-strings`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_compare_strings_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // identical strings
    let (o, n) = eval_oracle_and_neovm(r#"(compare-strings "foobar" nil nil "foobar" nil nil)"#);
    assert_ok_eq("t", &o, &n);

    // first < second
    let (o, n) = eval_oracle_and_neovm(r#"(compare-strings "abc" nil nil "xyz" nil nil)"#);
    assert_ok_eq("-1", &o, &n);

    // first > second
    let (o, n) = eval_oracle_and_neovm(r#"(compare-strings "xyz" nil nil "abc" nil nil)"#);
    assert_ok_eq("1", &o, &n);

    // case-insensitive
    let (o, n) = eval_oracle_and_neovm(r#"(compare-strings "HELLO" nil nil "hello" nil nil t)"#);
    assert_ok_eq("t", &o, &n);

    // subrange comparison
    let (o, n) = eval_oracle_and_neovm(r#"(compare-strings "xxabcyy" 2 5 "zzabcww" 2 5)"#);
    assert_ok_eq("t", &o, &n);

    // prefix shorter
    let (o, n) = eval_oracle_and_neovm(r#"(compare-strings "ab" nil nil "abcd" nil nil)"#);
    assert_ok_eq("-3", &o, &n);

    // empty strings
    let (o, n) = eval_oracle_and_neovm(r#"(compare-strings "" nil nil "" nil nil)"#);
    assert_ok_eq("t", &o, &n);
}
