//! Oracle parity tests for `assoc`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_assoc_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_found, neovm_found) =
        eval_oracle_and_neovm(r#"(assoc "b" '(("a" . 1) ("b" . 2)))"#);
    assert_ok_eq("(\"b\" . 2)", &oracle_found, &neovm_found);

    let (oracle_missing, neovm_missing) =
        eval_oracle_and_neovm(r#"(assoc "z" '(("a" . 1) ("b" . 2)))"#);
    assert_ok_eq("nil", &oracle_missing, &neovm_missing);
}

#[test]
fn oracle_prop_assoc_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(r#"(assoc "a" 1)"#);
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_assoc_equal_key(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(r#"(assoc "k" (list (cons "x" {}) (cons (concat "k") {})))"#, a, b);
        let expected = format!("(\"k\" . {})", b);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
