//! Oracle parity tests for `member`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_member_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_found, neovm_found) = eval_oracle_and_neovm("(member 2 '(1 2 3))");
    assert_ok_eq("(2 3)", &oracle_found, &neovm_found);

    let (oracle_missing, neovm_missing) = eval_oracle_and_neovm("(member 9 '(1 2 3))");
    assert_ok_eq("nil", &oracle_missing, &neovm_missing);
}

#[test]
fn oracle_prop_member_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(member 1 2)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_member_returns_tail(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
        c in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));
        prop_assume!(a != b);

        let form = format!("(member {} (list {} {} {}))", a, b, a, c);
        let expected = format!("({} {})", a, c);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
