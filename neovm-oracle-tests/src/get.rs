//! Oracle parity tests for `get`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_get_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_set, neovm_set) =
        eval_oracle_and_neovm("(let ((s 'oracle-prop-get)) (put s 'k 12) (get s 'k))");
    assert_ok_eq("12", &oracle_set, &neovm_set);

    let (oracle_missing, neovm_missing) =
        eval_oracle_and_neovm("(let ((s 'oracle-prop-get-missing)) (get s 'k))");
    assert_ok_eq("nil", &oracle_missing, &neovm_missing);
}

#[test]
fn oracle_prop_get_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(get 1 'k)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_get_latest_value(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(let ((s 'oracle-prop-get-rand)) (put s 'k {}) (put s 'k {}) (get s 'k))",
            a, b
        );
        let expected = b.to_string();
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
