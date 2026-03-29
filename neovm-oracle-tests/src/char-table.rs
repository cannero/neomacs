//! Oracle parity tests for char-table primitives.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_err_kind, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_char_table_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_ct, neovm_ct) = eval_oracle_and_neovm("(char-table-p (make-char-table 'generic))");
    assert_ok_eq("t", &oracle_ct, &neovm_ct);

    let (oracle_vec_or_ct, neovm_vec_or_ct) =
        eval_oracle_and_neovm("(vector-or-char-table-p (make-char-table 'generic))");
    assert_ok_eq("t", &oracle_vec_or_ct, &neovm_vec_or_ct);
}

#[test]
fn oracle_prop_char_table_set_range_cons_pair() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((ct (make-char-table 'generic nil))) (set-char-table-range ct '(?a . ?c) 'x) (list (char-table-range ct ?a) (char-table-range ct ?b) (char-table-range ct ?c) (char-table-range ct ?d)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(x x x nil)", &oracle, &neovm);
}

#[test]
fn oracle_prop_char_table_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(char-table-range 1 ?a)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}
