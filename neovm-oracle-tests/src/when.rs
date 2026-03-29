//! Oracle parity tests for `when`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_when_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_t, neovm_t) = eval_oracle_and_neovm("(when t 7)");
    assert_ok_eq("7", &oracle_t, &neovm_t);

    let (oracle_nil, neovm_nil) = eval_oracle_and_neovm("(when nil 7)");
    assert_ok_eq("nil", &oracle_nil, &neovm_nil);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_when_branching(
        cond in any::<bool>(),
        a in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let cond_form = if cond { "t" } else { "nil" };
        let form = format!("(when {} {})", cond_form, a);
        let expected = if cond { a.to_string() } else { "nil".to_string() };
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
