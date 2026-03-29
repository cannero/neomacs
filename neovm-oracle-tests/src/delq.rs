//! Oracle parity tests for `delq`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_delq_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(delq 3 '(1 3 5 3 7))");
    assert_ok_eq("(1 5 7)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(delq 'x '(x x x))");
    assert_ok_eq("nil", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(delq 99 '(10 20 30))");
    assert_ok_eq("(10 20 30)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(delq 5 nil)");
    assert_ok_eq("nil", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(delq 'a '(b c a d a e))");
    assert_ok_eq("(b c d e)", &o, &n);
}

#[test]
fn oracle_prop_delq_wrong_type() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(delq 1 42)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_delq_integer_removal(
        target in -200i64..200i64,
        a in -200i64..200i64,
        b in -200i64..200i64,
        c in -200i64..200i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(delq {} (list {} {} {}))", target, a, b, c);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_eq!(neovm, oracle, "delq parity failed for: {form}");
    }
}
