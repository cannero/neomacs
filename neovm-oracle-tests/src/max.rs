//! Oracle parity tests for `max`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_max_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_int, neovm_int) = eval_oracle_and_neovm("(max 1 9 -3)");
    assert_ok_eq("9", &oracle_int, &neovm_int);

    let (oracle_mixed, neovm_mixed) = eval_oracle_and_neovm("(max 1 2.5)");
    assert_ok_eq("2.5", &oracle_mixed, &neovm_mixed);
}

#[test]
fn oracle_prop_max_error_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (arity_oracle, arity_neovm) = eval_oracle_and_neovm("(max)");
    assert_err_kind(&arity_oracle, &arity_neovm, "wrong-number-of-arguments");

    let (type_oracle, type_neovm) = eval_oracle_and_neovm(r#"(max 1 "x")"#);
    assert_err_kind(&type_oracle, &type_neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_max_operator(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(max {} {})", a, b);
        let expected = format!("OK {}", std::cmp::max(a, b));
        let (oracle, neovm) = eval_oracle_and_neovm(&form);

        prop_assert_eq!(oracle.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(neovm, oracle);
    }
}
