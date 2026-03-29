//! Oracle parity tests for `event-convert-list`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_event_convert_list_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(event-convert-list '(control ?x))");
    assert_oracle_parity_with_bootstrap("(event-convert-list '(meta control ?x))");
}

#[test]
fn oracle_prop_event_convert_list_lookup_key_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((m (make-sparse-keymap))) (define-key m (vector (event-convert-list '(control ?x))) 'foo) (lookup-key m (vector (event-convert-list '(control ?x)))))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("foo", &oracle, &neovm);
}

#[test]
fn oracle_prop_event_convert_list_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(event-convert-list 1)");
    assert_ok_eq("nil", &oracle, &neovm);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_event_convert_list_control_ascii_lower(
        ch in 97u32..123u32, // a-z
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!("(event-convert-list (list 'control {}))", ch);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm, oracle, "event-convert-list parity failed for: {}", form);
    }
}
