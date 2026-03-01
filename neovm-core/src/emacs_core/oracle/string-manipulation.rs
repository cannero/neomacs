//! Oracle parity tests for string manipulation primitives.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm, ORACLE_PROP_CASES};

#[test]
fn oracle_prop_string_width() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(string-width "hello")"#);
    assert_oracle_parity(r#"(string-width "")"#);
    assert_oracle_parity(r#"(string-width "abc")"#);
}

#[test]
fn oracle_prop_string_prefix_p() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-prefix-p "hel" "hello")"#,
    );
    assert_ok_eq("t", &o, &n);

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-prefix-p "xyz" "hello")"#,
    );
    assert_ok_eq("nil", &o, &n);

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-prefix-p "" "hello")"#,
    );
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_string_suffix_p() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-suffix-p "llo" "hello")"#,
    );
    assert_ok_eq("t", &o, &n);

    let (o, n) = eval_oracle_and_neovm(
        r#"(string-suffix-p "xyz" "hello")"#,
    );
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_string_trim() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(string-trim "  hello  ")"#);
    assert_oracle_parity(r#"(string-trim "\t\nhello\n\t")"#);
    assert_oracle_parity(r#"(string-trim "hello")"#);
    assert_oracle_parity(r#"(string-trim "")"#);
}

#[test]
fn oracle_prop_string_trim_left() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(string-trim-left "  hello  ")"#);
}

#[test]
fn oracle_prop_string_trim_right() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(string-trim-right "  hello  ")"#);
}

#[test]
fn oracle_prop_string_join() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(string-join '("a" "b" "c") "-")"#);
    assert_oracle_parity(r#"(string-join '("a" "b" "c") "")"#);
    assert_oracle_parity(r#"(string-join '("only") ",")"#);
    assert_oracle_parity(r#"(string-join nil ",")"#);
}

#[test]
fn oracle_prop_split_string_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(split-string "a-b-c" "-")"#);
    assert_oracle_parity(r#"(split-string "hello world" " ")"#);
    assert_oracle_parity(r#"(split-string "no-split" "X")"#);
}

#[test]
fn oracle_prop_string_replace() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(
        r#"(string-replace "world" "emacs" "hello world")"#,
    );
    assert_oracle_parity(
        r#"(string-replace "x" "y" "no match")"#,
    );
    assert_oracle_parity(
        r#"(string-replace "a" "bb" "banana")"#,
    );
}

#[test]
fn oracle_prop_string_search() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(string-search "world" "hello world")"#);
    assert_oracle_parity(r#"(string-search "xyz" "hello world")"#);
    assert_oracle_parity(r#"(string-search "l" "hello" 3)"#);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_string_length_concat(
        a_len in 0usize..20usize,
        b_len in 0usize..20usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            r#"(length (concat (make-string {} ?a) (make-string {} ?b)))"#,
            a_len, b_len
        );
        let expected = format!("OK {}", a_len + b_len);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(oracle.as_str(), expected.as_str());
    }
}
