//! Oracle parity tests for `forward-char`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap,
    eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_forward_char_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(
        "(progn (erase-buffer) (insert \"abcd\") (goto-char 1) (forward-char 2) (point))",
    );
    assert_ok_eq("3", &oracle, &neovm);
}

#[test]
fn oracle_prop_forward_char_error_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (type_oracle, type_neovm) = eval_oracle_and_neovm(r#"(forward-char "x")"#);
    assert_err_kind(&type_oracle, &type_neovm, "wrong-type-argument");

    let (eob_oracle, eob_neovm) = eval_oracle_and_neovm(
        "(progn (erase-buffer) (insert \"a\") (goto-char 1) (forward-char 10))",
    );
    assert_err_kind(&eob_oracle, &eob_neovm, "end-of-buffer");

    let (bob_oracle, bob_neovm) = eval_oracle_and_neovm(
        "(progn (erase-buffer) (insert \"a\") (goto-char 1) (forward-char -1))",
    );
    assert_err_kind(&bob_oracle, &bob_neovm, "beginning-of-buffer");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_forward_char_parity_with_normalized_error(
        n in -8i64..8i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(progn
               (erase-buffer)
               (insert \"abcd\")
               (goto-char 2)
               (condition-case err
                   (progn (forward-char {}) (list 'ok (point)))
                 (error (list 'err (car err) (point)))))",
            n
        );
        assert_oracle_parity_with_bootstrap(&form);
    }
}
