//! Oracle parity tests for `end-of-line`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap,
    eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_end_of_line_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(
        "(progn (erase-buffer) (insert \"abc\") (goto-char 1) (end-of-line) (point))",
    );
    assert_ok_eq("4", &oracle, &neovm);
}

#[test]
fn oracle_prop_end_of_line_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(r#"(end-of-line "x")"#);
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_end_of_line_optional_n_parity(
        n in 1i64..5i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(progn (erase-buffer) (insert \"a\\nb\\nc\\nd\\n\") (goto-char 5) (list (end-of-line {}) (point)))",
            n
        );
        assert_oracle_parity_with_bootstrap(&form);
    }
}
