//! Oracle parity tests for `match-data`, `set-match-data`, `match-string`,
//! and `save-match-data`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

#[test]
fn oracle_prop_match_data_after_string_match() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
                    (string-match "\\(foo\\)\\(bar\\)" "foobar")
                    (match-data))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_match_data_groups() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
                    (string-match "\\([a-z]+\\)-\\([0-9]+\\)" "abc-123")
                    (list (match-beginning 0) (match-end 0)
                          (match-beginning 1) (match-end 1)
                          (match-beginning 2) (match-end 2)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_match_string_from_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
                    (string-match "\\([a-z]+\\)-\\([0-9]+\\)" "abc-123-def-456")
                    (list (match-string 0 "abc-123-def-456")
                          (match-string 1 "abc-123-def-456")
                          (match-string 2 "abc-123-def-456")))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_match_string_no_match_group() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Accessing a group that wasn't part of the match
    let form = r#"(progn
                    (string-match "foo" "foobar")
                    (match-string 1 "foobar"))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_set_match_data_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
                    (set-match-data '(0 3 1 2))
                    (list (match-beginning 0) (match-end 0)
                          (match-beginning 1) (match-end 1)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_save_match_data_restores() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
                    (string-match "\\(foo\\)" "foobar")
                    (let ((orig-begin (match-beginning 1)))
                      (save-match-data
                        (string-match "\\(xyz\\)" "xyzabc"))
                      (list orig-begin
                            (match-beginning 1)
                            (= orig-begin (match-beginning 1)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_save_match_data_with_error() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // save-match-data restores even on error
    let form = r#"(progn
                    (string-match "\\(hello\\)" "hello world")
                    (let ((orig (match-beginning 1)))
                      (condition-case nil
                          (save-match-data
                            (string-match "\\(x\\)" "xyz")
                            (signal 'error '("boom")))
                        (error nil))
                      (= orig (match-beginning 1))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_match_data_complex_regex() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Complex regex with multiple groups, optional groups
    let form = r#"(progn
                    (string-match
                     "\\([0-9]+\\)\\.\\([0-9]+\\)\\(\\.\\([0-9]+\\)\\)?"
                     "1.2.3")
                    (list (match-string 0 "1.2.3")
                          (match-string 1 "1.2.3")
                          (match-string 2 "1.2.3")
                          (match-string 4 "1.2.3")))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_match_data_successive_matches() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Each string-match overwrites match data
    let form = r#"(let ((results nil))
                    (string-match "\\([a-z]+\\)" "hello world")
                    (setq results (cons (match-string 1 "hello world") results))
                    (string-match "\\([a-z]+\\)" "hello world" 6)
                    (setq results (cons (match-string 1 "hello world") results))
                    (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_match_data_with_buffer_search() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "foo-123 bar-456 baz-789")
                    (goto-char (point-min))
                    (let ((results nil))
                      (while (re-search-forward
                              "\\([a-z]+\\)-\\([0-9]+\\)" nil t)
                        (setq results
                              (cons (list (match-string 1)
                                          (match-string 2))
                                    results)))
                      (nreverse results)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
