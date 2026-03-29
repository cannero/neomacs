//! Oracle parity tests for `nthcdr`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_nthcdr_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(nthcdr 2 '(a b c d e))");
    assert_ok_eq("(c d e)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(nthcdr 0 '(10 20 30))");
    assert_ok_eq("(10 20 30)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(nthcdr 5 '(1 2))");
    assert_ok_eq("nil", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(nthcdr 0 nil)");
    assert_ok_eq("nil", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(nthcdr 1 '(solo))");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_nthcdr_wrong_type() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(nthcdr 'x '(1 2))");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_nthcdr_random_offset(
        offset in 0i64..6i64,
        a in -500i64..500i64,
        b in -500i64..500i64,
        c in -500i64..500i64,
        d in -500i64..500i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(nthcdr {} (list {} {} {} {}))", offset, a, b, c, d);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_eq!(neovm, oracle, "nthcdr parity failed for: {form}");
    }
}
