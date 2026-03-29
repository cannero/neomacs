//! Oracle parity tests for `char-after`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_char_after_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_at_point, neovm_at_point) = eval_oracle_and_neovm(
        r#"(progn (erase-buffer) (insert "abc") (goto-char 1) (char-after))"#,
    );
    assert_ok_eq("97", &oracle_at_point, &neovm_at_point);

    let (oracle_pos, neovm_pos) =
        eval_oracle_and_neovm(r#"(progn (erase-buffer) (insert "abc") (char-after 2))"#);
    assert_ok_eq("98", &oracle_pos, &neovm_pos);
}

#[test]
fn oracle_prop_char_after_nil_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_nonpositive, neovm_nonpositive) =
        eval_oracle_and_neovm(r#"(progn (erase-buffer) (insert "abc") (char-after 0))"#);
    assert_ok_eq("nil", &oracle_nonpositive, &neovm_nonpositive);

    let (oracle_end, neovm_end) =
        eval_oracle_and_neovm(r#"(progn (erase-buffer) (insert "abc") (char-after 4))"#);
    assert_ok_eq("nil", &oracle_end, &neovm_end);
}

#[test]
fn oracle_prop_char_after_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(r#"(char-after "x")"#);
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_char_after_position_lookup(
        a in b'a'..=b'z',
        b in b'a'..=b'z',
        first in any::<bool>(),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let left = a as char;
        let right = b as char;
        let pos = if first { 1 } else { 2 };
        let expected_code = if first { a } else { b };
        let form = format!(
            r#"(progn (erase-buffer) (insert "{}{}") (char-after {}))"#,
            left, right, pos
        );
        let expected = expected_code.to_string();
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
