//! Oracle parity tests for symbol primitives.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap,
    eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_symbol_name_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(symbol-name 1)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_intern_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(intern 1)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_fboundp_car() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(fboundp 'car)");
    assert_ok_eq("t", &oracle, &neovm);
}

#[test]
fn oracle_prop_boundp_nil() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(boundp 'nil)");
    assert_ok_eq("t", &oracle, &neovm);
}

#[test]
fn oracle_prop_symbolp_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(symbolp "x")"#);
    assert_oracle_parity_with_bootstrap("(symbolp 'x)");
}

#[test]
fn oracle_prop_bare_colon_keyword_self_evaluates() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) =
        eval_oracle_and_neovm("(let ((x :)) (list (eq x :) (keywordp x) (symbolp x)))");
    assert_ok_eq("(t t t)", &oracle, &neovm);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_intern_symbol_name_roundtrip(
        name in proptest::string::string_regex(r"[a-z][a-z0-9-]{0,12}").expect("regex should compile"),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(r#"(symbol-name (intern {:?}))"#, name);
        let expected = format!("{:?}", name);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_symbolp_interned_symbol(
        name in proptest::string::string_regex(r"[a-z][a-z0-9-]{0,12}").expect("regex should compile"),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(r#"(symbolp (intern {:?}))"#, name);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq("t", &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_intern_eq_idempotent(
        name in proptest::string::string_regex(r"[a-z][a-z0-9-]{0,12}").expect("regex should compile"),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(r#"(eq (intern {:?}) (intern {:?}))"#, name, name);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq("t", &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_fboundp_unknown_symbol(
        name in proptest::string::string_regex(r"[a-z][a-z0-9-]{0,10}").expect("regex should compile"),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let symbol_name = format!("neovm-oracle-unknown-fn-{name}");
        let form = format!(r#"(fboundp (intern {:?}))"#, symbol_name);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq("nil", &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_boundp_unknown_symbol(
        name in proptest::string::string_regex(r"[a-z][a-z0-9-]{0,10}").expect("regex should compile"),
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let symbol_name = format!("neovm-oracle-unknown-var-{name}");
        let form = format!(r#"(boundp (intern {:?}))"#, symbol_name);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq("nil", &oracle, &neovm);
    }
}
