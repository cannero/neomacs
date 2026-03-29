//! Oracle parity tests for `not`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_not_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_t, neovm_t) = eval_oracle_and_neovm("(not nil)");
    assert_ok_eq("t", &oracle_t, &neovm_t);

    let (oracle_nil, neovm_nil) = eval_oracle_and_neovm("(not 1)");
    assert_ok_eq("nil", &oracle_nil, &neovm_nil);
}

#[test]
fn oracle_prop_not_wrong_arity_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(not)");
    assert_err_kind(&oracle, &neovm, "wrong-number-of-arguments");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_not_boolean_behavior(
        cond in any::<bool>(),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let arg = if cond { "t" } else { "nil" };
        let expected = if cond { "nil" } else { "t" };
        let form = format!("(not {})", arg);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected, &oracle, &neovm);
    }
}
