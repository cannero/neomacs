//! Oracle parity tests for list primitives.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_err_kind, run_neovm_eval, run_oracle_eval};

#[test]
fn oracle_prop_car_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(car 1)";
    let oracle = run_oracle_eval(form).expect("oracle eval should run");
    let neovm = run_neovm_eval(form).expect("neovm eval should run");

    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_nth_wrong_index_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(nth "x" (list 1 2))"#;
    let oracle = run_oracle_eval(form).expect("oracle eval should run");
    let neovm = run_neovm_eval(form).expect("neovm eval should run");

    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_append_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(append 1 (list 2))";
    let oracle = run_oracle_eval(form).expect("oracle eval should run");
    let neovm = run_neovm_eval(form).expect("neovm eval should run");

    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_list_empty() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(list)";
    let oracle = run_oracle_eval(form).expect("oracle eval should run");
    let neovm = run_neovm_eval(form).expect("neovm eval should run");

    assert_eq!(oracle.as_str(), "OK nil");
    assert_eq!(neovm.as_str(), "OK nil");
    assert_eq!(neovm, oracle);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_cons_operator(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(cons {} {})", a, b);
        let expected = format!("OK ({} . {})", a, b);

        let oracle = run_oracle_eval(&form).expect("oracle eval should succeed");
        let neovm = run_neovm_eval(&form).expect("neovm eval should succeed");

        prop_assert_eq!(oracle.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }

    #[test]
    fn oracle_prop_car_operator(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(car (cons {} {}))", a, b);
        let expected = format!("OK {}", a);

        let oracle = run_oracle_eval(&form).expect("oracle eval should succeed");
        let neovm = run_neovm_eval(&form).expect("neovm eval should succeed");

        prop_assert_eq!(oracle.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }

    #[test]
    fn oracle_prop_cdr_operator(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(cdr (cons {} {}))", a, b);
        let expected = format!("OK {}", b);

        let oracle = run_oracle_eval(&form).expect("oracle eval should succeed");
        let neovm = run_neovm_eval(&form).expect("neovm eval should succeed");

        prop_assert_eq!(oracle.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }

    #[test]
    fn oracle_prop_list_operator(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
        c in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(list {} {} {})", a, b, c);
        let expected = format!("OK ({} {} {})", a, b, c);

        let oracle = run_oracle_eval(&form).expect("oracle eval should succeed");
        let neovm = run_neovm_eval(&form).expect("neovm eval should succeed");

        prop_assert_eq!(oracle.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }

    #[test]
    fn oracle_prop_length_operator(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
        c in -100_000i64..100_000i64,
        d in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(length (list {} {} {} {}))", a, b, c, d);
        let expected = "OK 4";

        let oracle = run_oracle_eval(&form).expect("oracle eval should succeed");
        let neovm = run_neovm_eval(&form).expect("neovm eval should succeed");

        prop_assert_eq!(oracle.as_str(), expected);
        prop_assert_eq!(neovm.as_str(), expected);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }

    #[test]
    fn oracle_prop_nth_operator(
        n in 0usize..8usize,
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
        c in -100_000i64..100_000i64,
        d in -100_000i64..100_000i64,
        e in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let values = [a, b, c, d, e];
        let form = format!("(nth {} (list {} {} {} {} {}))", n, a, b, c, d, e);
        let expected = if let Some(value) = values.get(n) {
            format!("OK {}", value)
        } else {
            "OK nil".to_string()
        };

        let oracle = run_oracle_eval(&form).expect("oracle eval should succeed");
        let neovm = run_neovm_eval(&form).expect("neovm eval should succeed");

        prop_assert_eq!(oracle.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }

    #[test]
    fn oracle_prop_append_operator(
        a in -100_000i64..100_000i64,
        b in -100_000i64..100_000i64,
        c in -100_000i64..100_000i64,
        d in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(append (list {} {}) (list {} {}))", a, b, c, d);
        let expected = format!("OK ({} {} {} {})", a, b, c, d);

        let oracle = run_oracle_eval(&form).expect("oracle eval should succeed");
        let neovm = run_neovm_eval(&form).expect("neovm eval should succeed");

        prop_assert_eq!(oracle.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }
}
