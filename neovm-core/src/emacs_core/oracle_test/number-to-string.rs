//! Oracle parity tests for `number-to-string`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap,
    eval_oracle_and_neovm, run_neovm_eval, run_oracle_eval,
};

#[test]
fn oracle_prop_number_to_string_integers() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(number-to-string 42)");
    assert_ok_eq(r#""42""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm("(number-to-string 0)");
    assert_ok_eq(r#""0""#, &o, &n);

    let (o, n) = eval_oracle_and_neovm("(number-to-string -100)");
    assert_ok_eq(r#""-100""#, &o, &n);
}

#[test]
fn oracle_prop_number_to_string_floats() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(number-to-string 3.14)");
    assert_oracle_parity_with_bootstrap("(number-to-string 0.0)");
    assert_oracle_parity_with_bootstrap("(number-to-string -2.5)");
    assert_oracle_parity_with_bootstrap("(number-to-string 1.0e10)");
}

#[test]
fn oracle_prop_number_to_string_wrong_type() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(number-to-string "hello")"####;
    let oracle = run_oracle_eval(form).expect("oracle eval should run");
    let neovm = run_neovm_eval(form).expect("neovm eval should run");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_number_to_string_large_numbers() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(number-to-string 1000000)");
    assert_oracle_parity_with_bootstrap("(number-to-string -999999)");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_number_to_string_roundtrip(
        n in -100_000i64..100_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(string-to-number (number-to-string {}))", n);
        let expected = format!("OK {}", n);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(oracle.as_str(), expected.as_str());
    }
}
