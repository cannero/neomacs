//! Oracle parity tests for `goto-char`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_goto_char_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_ret, neovm_ret) =
        eval_oracle_and_neovm("(progn (erase-buffer) (insert \"abc\") (goto-char 2))");
    assert_ok_eq("2", &oracle_ret, &neovm_ret);

    let (oracle_point, neovm_point) =
        eval_oracle_and_neovm("(progn (erase-buffer) (insert \"abc\") (goto-char 2) (point))");
    assert_ok_eq("2", &oracle_point, &neovm_point);
}

#[test]
fn oracle_prop_goto_char_error_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (arity_oracle, arity_neovm) = eval_oracle_and_neovm("(goto-char)");
    assert_err_kind(&arity_oracle, &arity_neovm, "wrong-number-of-arguments");

    let (type_oracle, type_neovm) = eval_oracle_and_neovm(r#"(goto-char "x")"#);
    assert_err_kind(&type_oracle, &type_neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_goto_char_updates_point(
        pos in 1usize..5usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(progn (erase-buffer) (insert \"abcd\") (goto-char {}) (point))",
            pos
        );
        let expected = pos.to_string();
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
