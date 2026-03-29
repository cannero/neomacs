//! Oracle parity tests for `sort`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_sort_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(sort (list 4 1 3 2) '<)");
    assert_ok_eq("(1 2 3 4)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(sort (list 9 7 5 3 1) '>)");
    assert_ok_eq("(9 7 5 3 1)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(sort nil '<)");
    assert_ok_eq("nil", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(sort (list 42) '<)");
    assert_ok_eq("(42)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(sort (list 2 2 2 2) '<)");
    assert_ok_eq("(2 2 2 2)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(sort (list 5 -3 0 8 -1) '<)");
    assert_ok_eq("(-3 -1 0 5 8)", &o, &n);
}

#[test]
fn oracle_prop_sort_strings() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(sort (list "cherry" "apple" "banana") 'string<)"#);
    assert_ok_eq(r#"("apple" "banana" "cherry")"#, &o, &n);
}

#[test]
fn oracle_prop_sort_keyword_arguments() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(
        "(sort (list (cons 2 'b) (cons 1 'a) (cons 3 'c)) :key 'car :lessp '<)",
    );
    assert_ok_eq("((1 . a) (2 . b) (3 . c))", &o, &n);

    let (o, n) = eval_oracle_and_neovm(
        "(sort (list (cons 2 'b) (cons 1 'a) (cons 3 'c)) :key 'car :lessp '< :reverse t)",
    );
    assert_ok_eq("((3 . c) (2 . b) (1 . a))", &o, &n);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_sort_ascending(
        a in -1000i64..1000i64,
        b in -1000i64..1000i64,
        c in -1000i64..1000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(sort (list {} {} {}) '<)", a, b, c);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_eq!(neovm, oracle, "sort parity failed for: {form}");
    }
}
