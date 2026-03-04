//! Oracle parity tests for `current-buffer`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap,
    eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_current_buffer_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(bufferp (current-buffer))");
    assert_ok_eq("t", &oracle, &neovm);

    assert_oracle_parity_with_bootstrap("(eq (current-buffer) (current-buffer))");
}

#[test]
fn oracle_prop_current_buffer_wrong_arity_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(current-buffer nil)");
    assert_err_kind(&oracle, &neovm, "wrong-number-of-arguments");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_current_buffer_name_after_set_buffer(
        suffix in proptest::string::string_regex(r"[a-z0-9-]{1,10}").expect("regex should compile"),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let name = format!("*neovm-oracle-current-buffer-{}*", suffix);
        let form = format!(
            "(let ((b (get-buffer-create {:?}))) (set-buffer b) (buffer-name (current-buffer)))",
            name
        );
        let expected = format!("{:?}", name);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
