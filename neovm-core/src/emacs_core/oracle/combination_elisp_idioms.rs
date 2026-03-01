//! Complex oracle tests for real-world Elisp idioms and patterns.
//!
//! Tests the kind of complex code you'd actually find in Emacs packages
//! and user configurations.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm, ORACLE_PROP_CASES};

// ---------------------------------------------------------------------------
// String parsing with regex + match-data
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_idiom_parse_key_value_pairs() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((input "name=Alice age=30 role=engineer")
                        (pos 0)
                        (result nil))
                    (while (string-match
                            "\\([a-z]+\\)=\\([^ ]+\\)" input pos)
                      (setq result
                            (cons (cons (match-string 1 input)
                                        (match-string 2 input))
                                  result)
                            pos (match-end 0)))
                    (nreverse result))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_idiom_parse_csv_line() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((line "Alice,30,engineer,true")
                        (fields nil)
                        (pos 0))
                    (while (string-match "\\([^,]*\\),?" line pos)
                      (when (< pos (length line))
                        (setq fields (cons (match-string 1 line) fields)
                              pos (match-end 0))
                        (when (= pos (match-beginning 0))
                          (setq pos (1+ pos)))))
                    (nreverse fields))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Buffer processing with save-excursion
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_idiom_extract_headings() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "# Title\nsome text\n## Section 1\nmore text\n## Section 2\nfinal")
                    (goto-char (point-min))
                    (let ((headings nil))
                      (while (re-search-forward "^#+ \\(.+\\)$" nil t)
                        (setq headings (cons (match-string 1) headings)))
                      (nreverse headings)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_idiom_count_words_in_region() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "  hello   world  foo   bar  baz  ")
                    (goto-char (point-min))
                    (let ((count 0))
                      (while (re-search-forward "\\b\\w+\\b" nil t)
                        (setq count (1+ count)))
                      count))"#;
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("5", &o, &n);
}

#[test]
fn oracle_prop_idiom_line_by_line_processing() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "3\n1\n4\n1\n5\n9\n2\n6")
                    (goto-char (point-min))
                    (let ((sum 0) (count 0))
                      (while (not (eobp))
                        (let ((line-start (point)))
                          (end-of-line)
                          (let ((num (string-to-number
                                      (buffer-substring line-start (point)))))
                            (setq sum (+ sum num)
                                  count (1+ count))))
                        (forward-line 1))
                      (list sum count)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Association list manipulation patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_idiom_alist_merge() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Merge two alists (second overrides first for duplicate keys)
    let form = "(let ((base '((a . 1) (b . 2) (c . 3)))
                      (override '((b . 20) (d . 40))))
                  (let ((merged (copy-sequence base)))
                    (dolist (pair override)
                      (let ((existing (assq (car pair) merged)))
                        (if existing
                            (setcdr existing (cdr pair))
                          (setq merged (cons pair merged)))))
                    (sort merged (lambda (a b)
                                   (string-lessp (symbol-name (car a))
                                                 (symbol-name (car b)))))))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_idiom_alist_select_keys() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((data '((name . \"Alice\") (age . 30)
                               (role . engineer) (active . t)
                               (score . 95))))
                  (let ((keys '(name score active))
                        (result nil))
                    (dolist (k keys)
                      (let ((pair (assq k data)))
                        (when pair
                          (setq result (cons pair result)))))
                    (nreverse result)))";
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Hash-table driven complex logic
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_idiom_dependency_resolver() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Resolve dependencies (detect if all deps satisfied)
    let form = "(let ((deps (make-hash-table))
                      (installed (make-hash-table)))
                  (puthash 'web-mode '(emacs-core) deps)
                  (puthash 'lsp-mode '(emacs-core json-mode) deps)
                  (puthash 'emacs-core nil deps)
                  (puthash 'json-mode '(emacs-core) deps)
                  (puthash 'emacs-core t installed)
                  (puthash 'json-mode t installed)
                  (let ((check-deps
                         (lambda (pkg)
                           (let ((pkg-deps (gethash pkg deps))
                                 (all-ok t))
                             (dolist (d (or pkg-deps nil))
                               (unless (gethash d installed)
                                 (setq all-ok nil)))
                             all-ok))))
                    (list (funcall check-deps 'web-mode)
                          (funcall check-deps 'lsp-mode)
                          (funcall check-deps 'emacs-core))))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(t t t)", &o, &n);
}

// ---------------------------------------------------------------------------
// String template engine
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_idiom_string_template() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simple template: replace {{key}} with values from alist
    let form = r#"(let ((template "Hello {{name}}, you scored {{score}}!")
                        (vars '(("name" . "Alice") ("score" . "95"))))
                    (let ((result template))
                      (dolist (pair vars)
                        (setq result
                              (replace-regexp-in-string
                               (concat "{{" (car pair) "}}")
                               (cdr pair) result t t)))
                      result))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Tree transformation with accumulator
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_idiom_tree_reduce() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Reduce over a tree: sum all numbers, count all symbols
    let form = "(progn
  (fset 'neovm--test-tree-stats
    (lambda (tree)
      (let ((nums 0) (syms 0))
        (fset 'neovm--test-walk
          (lambda (node)
            (cond
              ((numberp node) (setq nums (+ nums node)))
              ((and (symbolp node) node (not (eq node t)))
               (setq syms (1+ syms)))
              ((consp node)
               (funcall 'neovm--test-walk (car node))
               (funcall 'neovm--test-walk (cdr node))))))
        (funcall 'neovm--test-walk tree)
        (list (cons 'num-sum nums) (cons 'sym-count syms)))))
  (unwind-protect
      (funcall 'neovm--test-tree-stats
               '(+ 1 (* 2 x) (- y 3) (/ z 4)))
    (fmakunbound 'neovm--test-tree-stats)
    (fmakunbound 'neovm--test-walk)))";
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Zipper pattern for tree navigation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_idiom_list_zipper() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // List zipper: navigate with history for undo
    let form = "(let ((make-zipper
                       (lambda (lst)
                         (list nil lst)))
                      (zipper-current
                       (lambda (z) (car (cadr z))))
                      (zipper-forward
                       (lambda (z)
                         (let ((before (car z))
                               (after (cadr z)))
                           (if (null after) z
                             (list (cons (car after) before)
                                   (cdr after))))))
                      (zipper-back
                       (lambda (z)
                         (let ((before (car z))
                               (after (cadr z)))
                           (if (null before) z
                             (list (cdr before)
                                   (cons (car before) after)))))))
                  (let ((z (funcall make-zipper '(a b c d e))))
                    (setq z (funcall zipper-forward z))
                    (setq z (funcall zipper-forward z))
                    (setq z (funcall zipper-forward z))
                    (let ((at-d (funcall zipper-current z)))
                      (setq z (funcall zipper-back z))
                      (let ((back-to-c (funcall zipper-current z)))
                        (list at-d back-to-c)))))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(d c)", &o, &n);
}

// ---------------------------------------------------------------------------
// Complex proptest: buffer search and extract
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_idiom_string_match_count(
        n in 1usize..6usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        // Build a string with N occurrences of "XX", count them via regex
        let mut s = String::new();
        for i in 0..n {
            if i > 0 { s.push('-'); }
            s.push_str("XX");
        }
        let form = format!(
            r#"(let ((s "{}") (count 0) (pos 0))
                 (while (string-match "XX" s pos)
                   (setq count (1+ count) pos (match-end 0)))
                 count)"#,
            s
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        let expected = format!("OK {}", n);
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(oracle.as_str(), expected.as_str());
    }
}
