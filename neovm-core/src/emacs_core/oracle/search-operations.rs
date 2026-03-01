//! Oracle parity tests for search operations: `search-forward`,
//! `search-backward`, `re-search-backward`, `looking-at-p`,
//! `posix-string-match`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// search-forward
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_search_forward_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "hello world hello emacs")
                    (goto-char (point-min))
                    (list (search-forward "hello" nil t)
                          (point)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_search_forward_bound() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // BOUND parameter limits search range
    let form = r#"(with-temp-buffer
                    (insert "aaa bbb ccc ddd")
                    (goto-char (point-min))
                    (list (search-forward "ccc" 8 t)
                          (search-forward "ccc" nil t)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_search_forward_count() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // COUNT parameter: find Nth occurrence
    let form = r#"(with-temp-buffer
                    (insert "ab ab ab ab ab")
                    (goto-char (point-min))
                    (search-forward "ab" nil t 3)
                    (point))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_search_forward_not_found() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "hello world")
                    (goto-char (point-min))
                    (search-forward "xyz" nil t))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// search-backward
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_search_backward_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "alpha beta gamma beta delta")
                    (goto-char (point-max))
                    (list (search-backward "beta" nil t)
                          (point)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_search_backward_bound() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "aaa bbb ccc bbb ddd")
                    (goto-char (point-max))
                    ;; bound=10 means don't search before position 10
                    (list (search-backward "aaa" 10 t)
                          (search-backward "bbb" nil t)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_search_backward_count() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "xx xx xx xx xx")
                    (goto-char (point-max))
                    (search-backward "xx" nil t 2)
                    (point))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// re-search-backward
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_re_search_backward_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "foo-123 bar-456 baz-789")
                    (goto-char (point-max))
                    (re-search-backward "\\([a-z]+\\)-\\([0-9]+\\)" nil t)
                    (list (match-string 0)
                          (match-string 1)
                          (match-string 2)
                          (point)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_re_search_backward_bound() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "aaa-111 bbb-222 ccc-333")
                    (goto-char (point-max))
                    ;; bound prevents finding first match
                    (list (re-search-backward "[a-z]+-[0-9]+" 10 t)
                          (when (match-string 0)
                            (match-string 0))))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_re_search_backward_collect_all() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Collect all matches searching backward
    let form = r#"(with-temp-buffer
                    (insert "cat sat on the mat with a bat")
                    (goto-char (point-max))
                    (let ((matches nil))
                      (while (re-search-backward "\\b[a-z]at\\b" nil t)
                        (setq matches (cons (match-string 0) matches)))
                      matches))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// looking-at-p (non-match-data-modifying)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_looking_at_p_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "hello world")
                    (goto-char (point-min))
                    (list (looking-at-p "hello")
                          (looking-at-p "world")
                          (looking-at-p "hel.*")))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_looking_at_p_preserves_match_data() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // looking-at-p should NOT modify match data
    let form = r#"(progn
                    (string-match "\\(foo\\)" "foobar")
                    (let ((before (match-beginning 1)))
                      (with-temp-buffer
                        (insert "test")
                        (goto-char (point-min))
                        (looking-at-p "test"))
                      (= before (match-beginning 1))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: search-and-extract pipeline
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_search_extract_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Extract structured data using forward and backward search
    let form = r#"(with-temp-buffer
                    (insert "BEGIN name=Alice END\n")
                    (insert "BEGIN name=Bob age=25 END\n")
                    (insert "BEGIN name=Carol role=dev END\n")
                    (goto-char (point-min))
                    (let ((records nil))
                      (while (search-forward "BEGIN " nil t)
                        (let ((start (point)))
                          (when (search-forward " END" nil t)
                            (let ((content (buffer-substring
                                            start (match-beginning 0))))
                              (let ((pairs nil)
                                    (pos 0))
                                (while (string-match
                                        "\\([a-z]+\\)=\\([^ ]+\\)"
                                        content pos)
                                  (setq pairs
                                        (cons (cons (match-string 1 content)
                                                    (match-string 2 content))
                                              pairs)
                                        pos (match-end 0)))
                                (setq records
                                      (cons (nreverse pairs)
                                            records)))))))
                      (nreverse records)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_search_bidirectional_bracket_match() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Find matching brackets using forward/backward search
    let form = r#"(with-temp-buffer
                    (insert "(defun foo (x y) (+ x y))")
                    ;; Find the inner (x y) paren group
                    (goto-char (point-min))
                    (search-forward "(x y)" nil t)
                    (let ((end (point))
                          (start (match-beginning 0)))
                      ;; Now search backward from end for opening paren
                      (goto-char end)
                      (search-backward "(" start t)
                      (let ((inner-start (point)))
                        (list inner-start end
                              (buffer-substring inner-start end)))))"#;
    assert_oracle_parity(form);
}
