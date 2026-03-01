//! Oracle parity tests for `reverse`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm, ORACLE_PROP_CASES};

#[test]
fn oracle_prop_reverse_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(reverse '(1 2 3 4 5))");
    assert_ok_eq("(5 4 3 2 1)", &o, &n);
}

#[test]
fn oracle_prop_reverse_empty() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(reverse nil)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_reverse_single() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(reverse '(42))");
    assert_ok_eq("(42)", &o, &n);
}

#[test]
fn oracle_prop_reverse_does_not_mutate_original() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((lst '(1 2 3)))
                  (reverse lst)
                  lst)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 2 3)", &o, &n);
}

#[test]
fn oracle_prop_reverse_nested_lists() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // reverse should only reverse top-level, not recurse
    let (o, n) = eval_oracle_and_neovm("(reverse '((1 2) (3 4) (5 6)))");
    assert_ok_eq("((5 6) (3 4) (1 2))", &o, &n);
}

#[test]
fn oracle_prop_reverse_mixed_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(reverse (list 1 "two" 'three 4.0 '(five)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_reverse_double_reversal_identity() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(
        "(equal '(1 2 3 4 5) (reverse (reverse '(1 2 3 4 5))))",
    );
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_reverse_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // reverse also works on strings
    let (o, n) = eval_oracle_and_neovm(r#"(reverse "hello")"#);
    assert_ok_eq(r#""olleh""#, &o, &n);
}

#[test]
fn oracle_prop_reverse_vector() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // reverse works on vectors too
    let (o, n) = eval_oracle_and_neovm("(reverse [1 2 3 4])");
    assert_ok_eq("[4 3 2 1]", &o, &n);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_reverse_preserves_length(
        a in -100i64..100i64,
        b in -100i64..100i64,
        c in -100i64..100i64,
        d in -100i64..100i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(length (reverse '({} {} {} {})))",
            a, b, c, d
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
        prop_assert_eq!(neovm.as_str(), "OK 4");
    }
}
