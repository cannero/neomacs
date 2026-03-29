//! Oracle parity tests for `modify-syntax-entry`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

#[test]
fn oracle_prop_modify_syntax_entry_cons_pair_range() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((st (make-syntax-table))) (modify-syntax-entry '(?a . ?z) \"w\" st) (list (aref st ?a) (aref st ?m) (aref st ?z) (aref st ?A)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_modify_syntax_entry_digit_range() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((st (make-syntax-table))) (modify-syntax-entry '(?0 . ?9) \".\" st) (list (aref st ?0) (aref st ?5) (aref st ?9)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_modify_syntax_entry_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(modify-syntax-entry 1 \"w\")");
    assert_ok_eq("nil", &oracle, &neovm);
}

#[test]
fn oracle_prop_make_syntax_table_inherits_standard_entries() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((st (make-syntax-table))) (list (aref st ?A) (aref st ?0) (aref st ?\\n)))";
    assert_oracle_parity_with_bootstrap(form);
}
