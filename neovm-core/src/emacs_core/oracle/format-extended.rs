//! Oracle parity tests for `format` with thorough parameter coverage.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm, ORACLE_PROP_CASES};

#[test]
fn oracle_prop_format_percent_d_integers() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(format "%d" 42)"#);
    assert_oracle_parity(r#"(format "%d" -42)"#);
    assert_oracle_parity(r#"(format "%d" 0)"#);
}

#[test]
fn oracle_prop_format_percent_s_various_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(format "%s" "hello")"#);
    assert_oracle_parity(r#"(format "%s" 42)"#);
    assert_oracle_parity(r#"(format "%s" 'symbol)"#);
    assert_oracle_parity(r#"(format "%s" nil)"#);
    assert_oracle_parity(r#"(format "%s" t)"#);
    assert_oracle_parity(r#"(format "%s" '(1 2 3))"#);
}

#[test]
fn oracle_prop_format_percent_S_prin1_style() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(format "%S" "hello")"#);
    assert_oracle_parity(r#"(format "%S" 42)"#);
    assert_oracle_parity(r#"(format "%S" '(1 "two" three))"#);
}

#[test]
fn oracle_prop_format_padding() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(format "%10d" 42)"#);
    assert_oracle_parity(r#"(format "%-10d|" 42)"#);
    assert_oracle_parity(r#"(format "%010d" 42)"#);
    assert_oracle_parity(r#"(format "%10s" "hi")"#);
    assert_oracle_parity(r#"(format "%-10s|" "hi")"#);
}

#[test]
fn oracle_prop_format_float() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(format "%f" 3.14)"#);
    assert_oracle_parity(r#"(format "%.2f" 3.14159)"#);
    assert_oracle_parity(r#"(format "%e" 12345.6789)"#);
    assert_oracle_parity(r#"(format "%g" 0.00001)"#);
    assert_oracle_parity(r#"(format "%g" 12345.0)"#);
}

#[test]
fn oracle_prop_format_hex_octal() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(format "%x" 255)"#);
    assert_oracle_parity(r#"(format "%X" 255)"#);
    assert_oracle_parity(r#"(format "%o" 255)"#);
    assert_oracle_parity(r#"(format "%#x" 255)"#);
    assert_oracle_parity(r#"(format "%#o" 255)"#);
}

#[test]
fn oracle_prop_format_char() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(format "%c" 65)"#);
    assert_oracle_parity(r#"(format "%c" ?A)"#);
    assert_oracle_parity(r#"(format "%c" ?z)"#);
}

#[test]
fn oracle_prop_format_multiple_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(
        r#"(format "Name: %s, Age: %d, Score: %.1f" "Alice" 30 95.5)"#,
    );
    assert_oracle_parity(
        r#"(format "%s=%s&%s=%s" "key1" "val1" "key2" "val2")"#,
    );
}

#[test]
fn oracle_prop_format_percent_literal() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity(r#"(format "100%%")"#);
    assert_oracle_parity(r#"(format "%d%%" 50)"#);
}

#[test]
fn oracle_prop_format_no_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(format "plain text")"#);
    assert_ok_eq(r#""plain text""#, &o, &n);
}

#[test]
fn oracle_prop_format_complex_template() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(format "[%04d] %-15s %+8.2f (%s)"
                          7 "transaction" -42.5 "pending")"#;
    assert_oracle_parity(form);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_format_d_proptest(
        n in -10000i64..10000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(r#"(format "%d" {})"#, n);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }

    #[test]
    fn oracle_prop_format_x_proptest(
        n in 0u32..65536u32,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(r#"(format "%x" {})"#, n);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }
}
