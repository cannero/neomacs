//! Oracle parity tests for `regexp-quote`, `replace-regexp-in-string`,
//! `looking-at`, `replace-match`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{
    assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
    eval_oracle_and_neovm_with_bootstrap,
};

// ---------------------------------------------------------------------------
// regexp-quote
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_quote_special_chars() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // regexp-quote escapes special regex characters
    assert_oracle_parity_with_bootstrap(r#"(regexp-quote "foo.bar")"#);
    assert_oracle_parity_with_bootstrap(r#"(regexp-quote "a*b+c?")"#);
    assert_oracle_parity_with_bootstrap(r#"(regexp-quote "[test]")"#);
    assert_oracle_parity_with_bootstrap(r#"(regexp-quote "a\\b")"#);
    assert_oracle_parity_with_bootstrap(r#"(regexp-quote "^start$")"#);
    assert_oracle_parity_with_bootstrap(r#"(regexp-quote "(group)")"#);
}

#[test]
fn oracle_prop_regexp_quote_plain_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Plain strings should pass through unchanged
    let (o, n) = eval_oracle_and_neovm(r#"(regexp-quote "hello")"#);
    assert_ok_eq(r#""hello""#, &o, &n);
}

#[test]
fn oracle_prop_regexp_quote_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // After quoting, the string should match literally
    let form = r####"(let ((literal "foo.bar*baz"))
                    (string-match-p (regexp-quote literal) literal))"####;
    let (o, n) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_ok_eq("0", &o, &n);
}

#[test]
fn oracle_prop_regexp_quote_used_in_search() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Without quoting, "." matches any char; with quoting, only literal "."
    let form = r####"(list
                    (string-match-p "foo.bar" "fooXbar")
                    (string-match-p (regexp-quote "foo.bar") "fooXbar")
                    (string-match-p (regexp-quote "foo.bar") "foo.bar"))"####;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// replace-regexp-in-string (5 params)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_replace_regexp_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm_with_bootstrap(
        r#"(replace-regexp-in-string "[0-9]+" "NUM" "foo123bar456")"#,
    );
    assert_ok_eq(r#""fooNUMbarNUM""#, &o, &n);
}

#[test]
fn oracle_prop_replace_regexp_no_match() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm_with_bootstrap(
        r#"(replace-regexp-in-string "xyz" "ABC" "hello world")"#,
    );
    assert_ok_eq(r#""hello world""#, &o, &n);
}

#[test]
fn oracle_prop_replace_regexp_with_backreference() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use \1 backreference in replacement
    let form = r####"(replace-regexp-in-string
                    "\\([a-z]+\\)-\\([0-9]+\\)"
                    "\\2-\\1"
                    "foo-123 bar-456")"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_replace_regexp_with_function() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // REP can be a function that receives the matched string
    let form = r####"(replace-regexp-in-string
                    "[0-9]+"
                    (lambda (match)
                      (number-to-string (* 2 (string-to-number match))))
                    "price: 10, qty: 5")"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_replace_regexp_fixedcase() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // FIXEDCASE parameter (4th arg)
    let form = r####"(replace-regexp-in-string
                    "hello" "world" "Hello hello HELLO" t)"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_replace_regexp_literal() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // LITERAL parameter (5th arg) — don't interpret \ in replacement
    let form = r####"(replace-regexp-in-string
                    "foo" "\\&bar" "foo" nil t)"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_replace_regexp_start_pos() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // START parameter (6th arg)
    let form = r####"(replace-regexp-in-string
                    "[0-9]+" "X" "a1b2c3d4" nil nil 4)"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_replace_regexp_complex_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Complex: strip HTML-like tags
    let form = r####"(replace-regexp-in-string
                    "<[^>]+>" "" "<b>bold</b> and <i>italic</i>")"####;
    let (o, n) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_ok_eq(r#""bold and italic""#, &o, &n);
}

// ---------------------------------------------------------------------------
// looking-at (buffer regex)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_looking_at_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(with-temp-buffer
                    (insert "hello world")
                    (goto-char (point-min))
                    (looking-at "hello"))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_looking_at_at_middle() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(with-temp-buffer
                    (insert "hello world")
                    (goto-char 7)
                    (looking-at "world"))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_looking_at_no_match() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(with-temp-buffer
                    (insert "hello world")
                    (goto-char (point-min))
                    (looking-at "world"))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_looking_at_sets_match_data() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(with-temp-buffer
                    (insert "abc-123-def")
                    (goto-char (point-min))
                    (looking-at "\\([a-z]+\\)-\\([0-9]+\\)")
                    (list (match-string 0)
                          (match-string 1)
                          (match-string 2)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// replace-match (buffer modification)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_replace_match_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(with-temp-buffer
                    (insert "hello world")
                    (goto-char (point-min))
                    (re-search-forward "world")
                    (replace-match "emacs")
                    (buffer-string))"####;
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq(r#""hello emacs""#, &o, &n);
}

#[test]
fn oracle_prop_replace_match_with_backreference() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(with-temp-buffer
                    (insert "foo-123")
                    (goto-char (point-min))
                    (re-search-forward "\\([a-z]+\\)-\\([0-9]+\\)")
                    (replace-match "\\2-\\1")
                    (buffer-string))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_replace_match_fixedcase_and_literal() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // replace-match FIXEDCASE (2nd arg), LITERAL (3rd arg)
    let form = r####"(with-temp-buffer
                    (insert "Hello World")
                    (goto-char (point-min))
                    (re-search-forward "Hello")
                    (replace-match "goodbye" t)
                    (buffer-string))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_replace_match_on_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // replace-match can operate on a string (4th arg)
    let form = r####"(progn
                    (string-match "\\([a-z]+\\)" "hello world")
                    (replace-match "REPLACED" nil nil "hello world"))"####;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex combination: search-and-replace pipeline
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_regexp_search_replace_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Multi-pass search and replace in a buffer
    let form = r####"(with-temp-buffer
                    (insert "price: $10, discount: $3, total: $7")
                    (goto-char (point-min))
                    (let ((sum 0))
                      (while (re-search-forward "\\$\\([0-9]+\\)" nil t)
                        (setq sum (+ sum (string-to-number
                                          (match-string 1)))))
                      (goto-char (point-max))
                      (insert (format " [sum=$%d]" sum))
                      (buffer-string)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_regexp_global_replace_in_buffer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r####"(with-temp-buffer
                    (insert "cat sat on the mat with a cat")
                    (goto-char (point-min))
                    (let ((count 0))
                      (while (re-search-forward "cat" nil t)
                        (replace-match "dog")
                        (setq count (1+ count)))
                      (list (buffer-string) count)))"####;
    assert_oracle_parity_with_bootstrap(form);
}
