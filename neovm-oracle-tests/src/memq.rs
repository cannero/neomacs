//! Oracle parity tests for `memq`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_memq_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_found, neovm_found) = eval_oracle_and_neovm("(memq 'b '(a b c))");
    assert_ok_eq("(b c)", &oracle_found, &neovm_found);

    let (oracle_missing, neovm_missing) = eval_oracle_and_neovm("(memq 'z '(a b c))");
    assert_ok_eq("nil", &oracle_missing, &neovm_missing);
}

#[test]
fn oracle_prop_memq_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(memq 'a 1)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_memq_float_uses_eq_identity() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // `memq` uses `eq`, so a separately read float literal is not identical.
    let (oracle, neovm) = eval_oracle_and_neovm("(memq 1.0 '(1.0 2.0))");
    assert_ok_eq("nil", &oracle, &neovm);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_memq_head_match(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(memq {} (list {} {}))", a, a, b);
        let expected = format!("({} {})", a, b);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
