//! Oracle parity tests for `string-version-lessp`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_string_version_lessp_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // identical
    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "v1.0" "v1.0")"#);
    assert_ok_eq("nil", &o, &n);

    // numeric ordering within strings
    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "file2" "file10")"#);
    assert_ok_eq("t", &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "file10" "file2")"#);
    assert_ok_eq("nil", &o, &n);

    // pure numeric
    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "9" "10")"#);
    assert_ok_eq("t", &o, &n);

    // version-style
    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "1.9.3" "1.10.1")"#);
    assert_ok_eq("t", &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "1.10.1" "1.9.3")"#);
    assert_ok_eq("nil", &o, &n);

    // prefix relation
    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "pkg" "pkg1")"#);
    assert_ok_eq("t", &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "pkg1" "pkg")"#);
    assert_ok_eq("nil", &o, &n);

    // empty strings
    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "" "")"#);
    assert_ok_eq("nil", &o, &n);

    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "" "a")"#);
    assert_ok_eq("t", &o, &n);

    // leading zeros
    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "007" "7")"#);
    assert_ok_eq("nil", &o, &n);

    // mixed alpha-numeric with dots
    let (o, n) = eval_oracle_and_neovm(r#"(string-version-lessp "v2.0" "v10.0")"#);
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_string_version_lessp_symbol_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // GNU Emacs accepts symbols — neovm should too
    let (o, n) = eval_oracle_and_neovm("(string-version-lessp 'v2 'v10)");
    assert_ok_eq("t", &o, &n);
}
