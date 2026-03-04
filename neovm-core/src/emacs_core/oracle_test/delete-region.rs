//! Oracle parity tests for `delete-region`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap,
    eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_delete_region_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(
        r#"(progn (erase-buffer) (insert "abcdef") (delete-region 2 5) (buffer-string))"#,
    );
    assert_ok_eq("\"aef\"", &oracle, &neovm);
}

#[test]
fn oracle_prop_delete_region_error_kinds() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (type_oracle, type_neovm) = eval_oracle_and_neovm(r#"(delete-region "x" 1)"#);
    assert_err_kind(&type_oracle, &type_neovm, "wrong-type-argument");

    let (range_oracle, range_neovm) =
        eval_oracle_and_neovm(r#"(progn (erase-buffer) (insert "abc") (delete-region 0 1))"#);
    assert_err_kind(&range_oracle, &range_neovm, "args-out-of-range");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_delete_region_valid_range_parity(
        start in 1usize..8usize,
        end in 1usize..8usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));
        prop_assume!(start <= end);

        let form = format!(
            r#"(progn (erase-buffer) (insert "abcdef") (delete-region {} {}) (buffer-string))"#,
            start, end
        );
        assert_oracle_parity_with_bootstrap(&form);
    }
}
