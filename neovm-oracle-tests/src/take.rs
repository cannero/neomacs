//! Oracle parity tests for `take`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_take_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(take 3 '(10 20 30 40 50))");
    assert_ok_eq("(10 20 30)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(take 0 '(7 8 9))");
    assert_ok_eq("nil", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(take 5 '(1 2))");
    assert_ok_eq("(1 2)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(take 1 nil)");
    assert_ok_eq("nil", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(take 2 '(42))");
    assert_ok_eq("(42)", &o, &n);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_take_random_count(
        count in 0i64..8i64,
        a in -50_000i64..50_000i64,
        b in -50_000i64..50_000i64,
        c in -50_000i64..50_000i64,
        d in -50_000i64..50_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(take {} (list {} {} {} {}))", count, a, b, c, d);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_eq!(neovm, oracle, "take parity failed for: {form}");
    }
}
