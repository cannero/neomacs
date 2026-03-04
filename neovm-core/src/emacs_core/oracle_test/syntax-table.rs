//! Oracle parity tests for syntax-table and related syntax primitives.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{
    assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
};

#[test]
fn oracle_prop_syntax_table_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(list (syntax-table-p (standard-syntax-table)) (syntax-table-p (copy-syntax-table)) (syntax-table-p (make-syntax-table)) (eq (char-table-subtype (standard-syntax-table)) 'syntax-table))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(t t t t)", &oracle, &neovm);
}

#[test]
fn oracle_prop_make_syntax_table_parent_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(
        "(let* ((p (make-syntax-table)) (c (make-syntax-table p))) (eq (char-table-parent c) p))",
    );
}

#[test]
fn oracle_prop_copy_syntax_table_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(copy-syntax-table 1)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_set_syntax_table_wrong_type_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(set-syntax-table 1)");
    assert_err_kind(&oracle, &neovm, "wrong-type-argument");
}

#[test]
fn oracle_prop_char_syntax_after_set_syntax_table_custom_entry() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(with-temp-buffer (let ((st (copy-syntax-table (standard-syntax-table)))) (modify-syntax-entry ?A \".\" st) (set-syntax-table st) (char-syntax ?A)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_syntax_after_observes_set_syntax_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(with-temp-buffer (insert \"A\") (goto-char (point-min)) (let ((st (copy-syntax-table (standard-syntax-table)))) (modify-syntax-entry ?A \".\" st) (set-syntax-table st) (syntax-after (point))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_string_to_syntax_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(list (string-to-syntax \"w\") (string-to-syntax \"_\") (string-to-syntax \"@\"))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("((2) (3) nil)", &oracle, &neovm);
}

#[test]
fn oracle_prop_syntax_class_to_char_boundaries() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(list (syntax-class-to-char 0) (syntax-class-to-char 2) (syntax-class-to-char 15))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(32 119 124)", &oracle, &neovm);
}

#[test]
fn oracle_prop_syntax_class_to_char_out_of_range_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(syntax-class-to-char 16)");
    assert_err_kind(&oracle, &neovm, "args-out-of-range");
}

#[test]
fn oracle_prop_matching_paren_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm(
        "(list (matching-paren ?\\() (matching-paren ?\\]) (matching-paren ?x))",
    );
    assert_ok_eq("(41 91 nil)", &oracle, &neovm);
}

#[test]
fn oracle_prop_forward_comment_whitespace_movement() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(
        "(with-temp-buffer (insert \"   x\") (goto-char 1) (list (forward-comment 1) (point)))",
    );
    assert_oracle_parity_with_bootstrap(
        "(with-temp-buffer (insert \"x   \") (goto-char (point-max)) (list (forward-comment -1) (point)))",
    );
}
