//! Oracle parity tests for `string-distance`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_string_distance_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // identical
    let (o, n) = eval_oracle_and_neovm(r#"(string-distance "kitten" "kitten")"#);
    assert_ok_eq("0", &o, &n);

    // single substitution
    let (o, n) = eval_oracle_and_neovm(r#"(string-distance "cat" "bat")"#);
    assert_ok_eq("1", &o, &n);

    // classic levenshtein example
    let (o, n) = eval_oracle_and_neovm(r#"(string-distance "kitten" "sitting")"#);
    assert_ok_eq("3", &o, &n);

    // insertion
    let (o, n) = eval_oracle_and_neovm(r#"(string-distance "abc" "abcd")"#);
    assert_ok_eq("1", &o, &n);

    // deletion
    let (o, n) = eval_oracle_and_neovm(r#"(string-distance "abcd" "abc")"#);
    assert_ok_eq("1", &o, &n);

    // empty vs non-empty
    let (o, n) = eval_oracle_and_neovm(r#"(string-distance "" "test")"#);
    assert_ok_eq("4", &o, &n);

    // both empty
    let (o, n) = eval_oracle_and_neovm(r#"(string-distance "" "")"#);
    assert_ok_eq("0", &o, &n);

    // completely different
    let (o, n) = eval_oracle_and_neovm(r#"(string-distance "abc" "xyz")"#);
    assert_ok_eq("3", &o, &n);

    // byte length mode
    let (o, n) = eval_oracle_and_neovm(r#"(string-distance "abc" "axc" t)"#);
    assert_ok_eq("1", &o, &n);
}
