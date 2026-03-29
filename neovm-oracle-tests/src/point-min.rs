//! Oracle parity tests for `point-min`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_point_min_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(point-min)");
    assert_ok_eq("1", &oracle, &neovm);
}

#[test]
fn oracle_prop_point_min_wrong_arity_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(point-min nil)");
    assert_err_kind(&oracle, &neovm, "wrong-number-of-arguments");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_point_min_is_stable(
        pos in 1usize..27usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(progn (erase-buffer) (insert \"abcdefghijklmnopqrstuvwxyz\") (goto-char {}) (point-min))",
            pos
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq("1", &oracle, &neovm);
    }
}
