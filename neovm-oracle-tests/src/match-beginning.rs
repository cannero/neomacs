//! Oracle parity tests for `match-beginning`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_match_beginning_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_hit, neovm_hit) =
        eval_oracle_and_neovm(r#"(progn (string-match "b+" "abbb") (match-beginning 0))"#);
    assert_ok_eq("1", &oracle_hit, &neovm_hit);
}

#[test]
fn oracle_prop_match_beginning_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(r#"(match-beginning "x")"#);
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_match_beginning_uses_character_positions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) =
        eval_oracle_and_neovm(r#"(progn (string-match "c" "αβc") (match-beginning 0))"#);
    assert_ok_eq("2", &oracle, &neovm);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_match_beginning_group0_index(
        n in 0usize..20usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let haystack = format!("{}a", "b".repeat(n));
        let form = format!(
            r#"(progn (string-match "a" "{}") (match-beginning 0))"#,
            haystack
        );
        let expected = n.to_string();
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
