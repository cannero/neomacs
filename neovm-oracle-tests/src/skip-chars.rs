//! Oracle parity tests for `skip-chars-forward`, `skip-chars-backward`,
//! and `skip-syntax-forward`, `skip-syntax-backward`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// skip-chars-forward basic
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_skip_chars_forward_alpha() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "abcdef 12345")
                    (goto-char (point-min))
                    (let ((skipped (skip-chars-forward "a-z")))
                      (list skipped (point)
                            (char-after (point)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_skip_chars_forward_digits() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "12345abc")
                    (goto-char (point-min))
                    (let ((skipped (skip-chars-forward "0-9")))
                      (list skipped (point))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_skip_chars_forward_complement() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ^negates the character set
    let form = r#"(with-temp-buffer
                    (insert "hello world!")
                    (goto-char (point-min))
                    (let ((skipped (skip-chars-forward "^ ")))
                      (list skipped (point)
                            (buffer-substring (point-min) (point)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_skip_chars_forward_limit() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // LIM parameter bounds the skip
    let form = r#"(with-temp-buffer
                    (insert "aaaaaabbbbbbb")
                    (goto-char (point-min))
                    (let ((skipped (skip-chars-forward "a-z" 5)))
                      (list skipped (point))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_skip_chars_forward_mixed_charset() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Multiple ranges and individual chars
    let form = r#"(with-temp-buffer
                    (insert "abc123XYZ_-!@#")
                    (goto-char (point-min))
                    (let ((skipped (skip-chars-forward "a-zA-Z0-9_-")))
                      (list skipped (point)
                            (buffer-substring (point-min) (point)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_skip_chars_forward_no_match() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Nothing to skip — returns 0
    let form = r#"(with-temp-buffer
                    (insert "!@#$%")
                    (goto-char (point-min))
                    (let ((skipped (skip-chars-forward "a-z")))
                      (list skipped (point))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// skip-chars-backward
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_skip_chars_backward_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "hello world")
                    (goto-char (point-max))
                    (let ((skipped (skip-chars-backward "a-z")))
                      (list skipped (point)
                            (buffer-substring (point) (point-max)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_skip_chars_backward_limit() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "aaa bbb ccc")
                    (goto-char (point-max))
                    ;; LIM = 8, so don't skip past position 8
                    (let ((skipped (skip-chars-backward "a-z " 8)))
                      (list skipped (point))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_skip_chars_backward_complement() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "hello world!")
                    (goto-char (point-max))
                    (let ((skipped (skip-chars-backward "^h")))
                      (list skipped (point))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// skip-syntax-forward / skip-syntax-backward
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_skip_syntax_forward_word() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // "w" = word constituent
    let form = r#"(with-temp-buffer
                    (insert "hello world")
                    (goto-char (point-min))
                    (let ((skipped (skip-syntax-forward "w")))
                      (list skipped (point)
                            (buffer-substring (point-min) (point)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_skip_syntax_forward_whitespace() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // " " = whitespace
    let form = r#"(with-temp-buffer
                    (insert "   \t\t  hello")
                    (goto-char (point-min))
                    (let ((skipped (skip-syntax-forward " ")))
                      (list skipped (point))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_skip_syntax_forward_limit() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "hello world")
                    (goto-char (point-min))
                    (let ((skipped (skip-syntax-forward "w" 4)))
                      (list skipped (point))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_skip_syntax_backward_word() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "hello world")
                    (goto-char (point-max))
                    (let ((skipped (skip-syntax-backward "w")))
                      (list skipped (point)
                            (buffer-substring (point) (point-max)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: tokenizer using skip-chars
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_skip_chars_tokenizer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simple tokenizer: split into words, numbers, and punctuation
    let form = r#"(with-temp-buffer
                    (insert "foo = 42 + bar * 3;")
                    (goto-char (point-min))
                    (let ((tokens nil))
                      (while (< (point) (point-max))
                        ;; Skip whitespace
                        (skip-chars-forward " \t\n")
                        (when (< (point) (point-max))
                          (let ((start (point))
                                (c (char-after (point))))
                            (cond
                             ;; Word
                             ((and (>= c ?a) (<= c ?z))
                              (skip-chars-forward "a-zA-Z_")
                              (setq tokens
                                    (cons (cons 'word
                                                (buffer-substring start (point)))
                                          tokens)))
                             ;; Number
                             ((and (>= c ?0) (<= c ?9))
                              (skip-chars-forward "0-9")
                              (setq tokens
                                    (cons (cons 'num
                                                (buffer-substring start (point)))
                                          tokens)))
                             ;; Punctuation
                             (t
                              (forward-char 1)
                              (setq tokens
                                    (cons (cons 'punct
                                                (buffer-substring start (point)))
                                          tokens)))))))
                      (nreverse tokens)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: balanced expression finder using skip
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_skip_chars_word_boundaries() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Extract all words and their positions
    let form = r#"(with-temp-buffer
                    (insert "the quick brown fox jumps over the lazy dog")
                    (goto-char (point-min))
                    (let ((words nil))
                      (while (< (point) (point-max))
                        (skip-chars-forward "^a-zA-Z")
                        (when (< (point) (point-max))
                          (let ((start (point)))
                            (skip-chars-forward "a-zA-Z")
                            (setq words
                                  (cons (list (buffer-substring start (point))
                                              start (point))
                                        words)))))
                      (nreverse words)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: CSV field parser using skip-chars
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_skip_chars_csv_parser() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Parse CSV with quoted fields
    let form = r#"(with-temp-buffer
                    (insert "name,age,city\nAlice,30,Boston\nBob,25,\"New York\"")
                    (goto-char (point-min))
                    (let ((rows nil))
                      (while (< (point) (point-max))
                        (let ((fields nil)
                              (eol (save-excursion
                                     (end-of-line)
                                     (point))))
                          (while (< (point) eol)
                            (let ((start (point)))
                              (if (= (char-after (point)) ?\")
                                  ;; Quoted field
                                  (progn
                                    (forward-char 1)
                                    (let ((fstart (point)))
                                      (skip-chars-forward "^\"")
                                      (setq fields
                                            (cons (buffer-substring
                                                   fstart (point))
                                                  fields))
                                      (when (< (point) (point-max))
                                        (forward-char 1))))
                                ;; Unquoted field
                                (skip-chars-forward "^,\n")
                                (setq fields
                                      (cons (buffer-substring start (point))
                                            fields)))
                              ;; Skip comma
                              (when (and (< (point) (point-max))
                                         (= (char-after (point)) ?,))
                                (forward-char 1))))
                          (setq rows (cons (nreverse fields) rows))
                          ;; Skip newline
                          (when (< (point) (point-max))
                            (forward-char 1))))
                      (nreverse rows)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: identifier extraction using skip-syntax
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_skip_syntax_extract_identifiers() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Extract all word-syntax tokens from a code-like string
    let form = r#"(with-temp-buffer
                    (insert "(defun calculate (x y) (+ (* x x) (* y y)))")
                    (goto-char (point-min))
                    (let ((ids nil))
                      (while (< (point) (point-max))
                        (skip-syntax-forward "^w_")
                        (when (< (point) (point-max))
                          (let ((start (point)))
                            (skip-syntax-forward "w_")
                            (when (> (point) start)
                              (setq ids
                                    (cons (buffer-substring start (point))
                                          ids))))))
                      (nreverse ids)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
