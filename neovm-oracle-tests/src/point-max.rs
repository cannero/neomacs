//! Oracle parity tests for `point-max`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_point_max_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(point-max)");
    assert_ok_eq("1", &oracle, &neovm);
}

#[test]
fn oracle_prop_point_max_wrong_arity_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(point-max nil)");
    assert_err_kind(&oracle, &neovm, "wrong-number-of-arguments");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_point_max_tracks_buffer_end(
        len in 0usize..20usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let content = "x".repeat(len);
        let form = format!(
            "(progn (erase-buffer) (insert \"{}\") (point-max))",
            content
        );
        let expected = (len + 1).to_string();
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
