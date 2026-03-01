//! Oracle parity tests for math functions: `floor`, `ceiling`, `round`,
//! `truncate`, `float`, `expt`, `sqrt`, `log`, `sin`, `cos`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm, ORACLE_PROP_CASES};

#[test]
fn oracle_prop_floor_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(floor 3.7)");
    assert_ok_eq("3", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(floor -3.7)");
    assert_ok_eq("-4", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(floor 4.0)");
    assert_ok_eq("4", &o, &n);
}

#[test]
fn oracle_prop_ceiling_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(ceiling 3.2)");
    assert_ok_eq("4", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(ceiling -3.2)");
    assert_ok_eq("-3", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(ceiling 4.0)");
    assert_ok_eq("4", &o, &n);
}

#[test]
fn oracle_prop_round_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(round 3.5)");
    assert_ok_eq("4", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(round 2.5)");
    assert_ok_eq("2", &o, &n);  // banker's rounding

    let (o, n) = eval_oracle_and_neovm("(round 3.3)");
    assert_ok_eq("3", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(round -3.7)");
    assert_ok_eq("-4", &o, &n);
}

#[test]
fn oracle_prop_truncate_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(truncate 3.9)");
    assert_ok_eq("3", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(truncate -3.9)");
    assert_ok_eq("-3", &o, &n);
}

#[test]
fn oracle_prop_float_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(float 42)");
    assert_oracle_parity("(float 0)");
    assert_oracle_parity("(float -7)");
    assert_oracle_parity("(float 3.14)");
}

#[test]
fn oracle_prop_expt_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(expt 2 10)");
    assert_ok_eq("1024", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(expt 3 0)");
    assert_ok_eq("1", &o, &n);

    assert_oracle_parity("(expt 2.0 0.5)");
}

#[test]
fn oracle_prop_sqrt_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(sqrt 4.0)");
    assert_oracle_parity("(sqrt 9.0)");
    assert_oracle_parity("(sqrt 2.0)");
}

#[test]
fn oracle_prop_log_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(log 1)");
    assert_oracle_parity("(log 10 10)");
}

#[test]
fn oracle_prop_sin_cos() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(sin 0)");
    assert_oracle_parity("(cos 0)");
    assert_oracle_parity("(sin 1.0)");
    assert_oracle_parity("(cos 1.0)");
}

#[test]
fn oracle_prop_floor_with_divisor() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(floor 7 2)");
    assert_ok_eq("3", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(floor 10 3)");
    assert_ok_eq("3", &o, &n);
}

#[test]
fn oracle_prop_isnan_and_special_floats() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(isnan 0.0)");
    assert_oracle_parity("(isnan 1.0)");
    assert_oracle_parity("(isnan 0.0e+NaN)");
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_floor_proptest(
        n in -1000i64..1000i64,
        d in 1i64..100i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(floor {} {})", n, d);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }

    #[test]
    fn oracle_prop_truncate_proptest(
        n in -1000i64..1000i64,
        d in 1i64..100i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(truncate {} {})", n, d);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }
}
