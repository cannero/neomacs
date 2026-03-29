//! Oracle parity tests for `put`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_put_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_return, neovm_return) =
        eval_oracle_and_neovm("(let ((s 'oracle-prop-put)) (put s 'k 99))");
    assert_ok_eq("99", &oracle_return, &neovm_return);

    let (oracle_get, neovm_get) =
        eval_oracle_and_neovm("(let ((s 'oracle-prop-put)) (put s 'k 99) (get s 'k))");
    assert_ok_eq("99", &oracle_get, &neovm_get);
}

#[test]
fn oracle_prop_put_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(put 1 'k 2)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_put_returns_value(
        a in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(let ((s 'oracle-prop-put-rand)) (put s 'k {}))", a);
        let expected = a.to_string();
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
