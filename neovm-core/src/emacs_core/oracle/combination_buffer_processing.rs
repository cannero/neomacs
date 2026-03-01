//! Complex oracle tests for buffer processing patterns.
//!
//! Tests patterns found in real Emacs packages: buffer parsers,
//! font-lock simulators, region processors, multi-buffer operations.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// INI-file parser
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_bp_ini_parser() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "[general]\nname=test\nverbose=true\n")
                    (insert "[database]\nhost=localhost\nport=5432\n")
                    (goto-char (point-min))
                    (let ((sections nil)
                          (current-section nil)
                          (current-pairs nil))
                      (while (not (eobp))
                        (let ((line-start (point)))
                          (end-of-line)
                          (let ((line (buffer-substring
                                       line-start (point))))
                            (cond
                              ;; Section header
                              ((string-match
                                "^\\[\\([^]]+\\)\\]$" line)
                               (when current-section
                                 (setq sections
                                       (cons (cons current-section
                                                   (nreverse current-pairs))
                                             sections)))
                               (setq current-section
                                     (match-string 1 line)
                                     current-pairs nil))
                              ;; Key=value
                              ((string-match
                                "^\\([^=]+\\)=\\(.*\\)$" line)
                               (setq current-pairs
                                     (cons (cons (match-string 1 line)
                                                 (match-string 2 line))
                                           current-pairs))))))
                        (forward-line 1))
                      ;; Flush last section
                      (when current-section
                        (setq sections
                              (cons (cons current-section
                                         (nreverse current-pairs))
                                    sections)))
                      (nreverse sections)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Log file analyzer
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_bp_log_analyzer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "INFO: Server started\n")
                    (insert "WARN: Low memory\n")
                    (insert "ERROR: Connection failed\n")
                    (insert "INFO: Request received\n")
                    (insert "ERROR: Timeout\n")
                    (insert "INFO: Request completed\n")
                    (goto-char (point-min))
                    (let ((counts (make-hash-table :test 'equal))
                          (errors nil))
                      (while (re-search-forward
                              "^\\(INFO\\|WARN\\|ERROR\\): \\(.*\\)$"
                              nil t)
                        (let ((level (match-string 1))
                              (msg (match-string 2)))
                          (puthash level
                                   (1+ (gethash level counts 0))
                                   counts)
                          (when (string= level "ERROR")
                            (setq errors (cons msg errors)))))
                      (list (gethash "INFO" counts 0)
                            (gethash "WARN" counts 0)
                            (gethash "ERROR" counts 0)
                            (nreverse errors))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Org-mode-like heading extractor
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_bp_org_heading_tree() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Extract headings with levels and build flat structure
    let form = r#"(with-temp-buffer
                    (insert "* Top Level\n")
                    (insert "Some text\n")
                    (insert "** Sub Level A\n")
                    (insert "** Sub Level B\n")
                    (insert "*** Deep Level\n")
                    (insert "* Another Top\n")
                    (goto-char (point-min))
                    (let ((headings nil))
                      (while (re-search-forward
                              "^\\(\\*+\\) \\(.+\\)$" nil t)
                        (let ((level (length (match-string 1)))
                              (title (match-string 2)))
                          (setq headings
                                (cons (list level title) headings))))
                      (nreverse headings)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Multi-region processing with save-excursion
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_bp_multi_region() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "abc 123 def 456 ghi 789")
                    ;; Collect all number regions with their positions
                    (goto-char (point-min))
                    (let ((regions nil))
                      (while (re-search-forward "[0-9]+" nil t)
                        (setq regions
                              (cons (list (match-beginning 0)
                                          (match-end 0)
                                          (match-string 0))
                                    regions)))
                      ;; Now double each number using saved positions
                      (dolist (r (nreverse regions))
                        (save-excursion
                          (goto-char (car r))
                          (delete-region (car r) (cadr r))
                          (insert (number-to-string
                                   (* 2 (string-to-number
                                          (caddr r)))))))
                      (buffer-string)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Buffer diff / comparison
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_bp_buffer_line_diff() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Compare two "buffers" line by line, report differences
    let form = r#"(let ((text-a "line1\nline2\nline3\nline4")
                        (text-b "line1\nLINE2\nline3\nline5"))
                    (let ((lines-a (split-string text-a "\n"))
                          (lines-b (split-string text-b "\n"))
                          (diffs nil)
                          (i 1))
                      (while (or lines-a lines-b)
                        (let ((a (car lines-a))
                              (b (car lines-b)))
                          (cond
                            ((and a b (not (string= a b)))
                             (setq diffs
                                   (cons (list 'changed i a b) diffs)))
                            ((and a (not b))
                             (setq diffs
                                   (cons (list 'removed i a) diffs)))
                            ((and (not a) b)
                             (setq diffs
                                   (cons (list 'added i b) diffs)))))
                        (setq lines-a (cdr lines-a)
                              lines-b (cdr lines-b)
                              i (1+ i)))
                      (nreverse diffs)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Indentation analyzer
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_bp_indentation_analysis() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "def foo():\n")
                    (insert "    if True:\n")
                    (insert "        return 1\n")
                    (insert "    else:\n")
                    (insert "        return 2\n")
                    (insert "def bar():\n")
                    (insert "    pass\n")
                    (goto-char (point-min))
                    (let ((indent-info nil))
                      (while (not (eobp))
                        (let ((line-start (point)))
                          (end-of-line)
                          (let ((line (buffer-substring
                                       line-start (point))))
                            (when (> (length line) 0)
                              (let ((indent 0))
                                (dotimes (i (length line))
                                  (if (= (aref line i) ?\ )
                                      (setq indent (1+ indent))
                                    (setq i (length line))))
                                (setq indent-info
                                      (cons (cons indent
                                                  (string-trim line))
                                            indent-info))))))
                        (forward-line 1))
                      (nreverse indent-info)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Buffer as accumulator with narrowing
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_bp_narrow_accumulate() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Process sections independently using narrowing
    let form = r#"(with-temp-buffer
                    (insert "---\nsection A content\n")
                    (insert "---\nsection B content\n")
                    (insert "---\nsection C content\n")
                    (goto-char (point-min))
                    (let ((sections nil))
                      ;; Find each section delimiter
                      (let ((boundaries nil))
                        (while (re-search-forward "^---$" nil t)
                          (setq boundaries
                                (cons (match-end 0) boundaries)))
                        (setq boundaries (nreverse boundaries))
                        ;; Process each section via narrowing
                        (let ((starts boundaries))
                          (while starts
                            (let ((start (car starts))
                                  (end (or (cadr starts) (point-max))))
                              (save-restriction
                                (narrow-to-region start end)
                                (goto-char (point-min))
                                (let ((content
                                       (string-trim
                                        (buffer-substring
                                         (point-min) (point-max)))))
                                  (setq sections
                                        (cons content sections)))))
                            (setq starts (cdr starts)))))
                      (nreverse sections)))"#;
    assert_oracle_parity(form);
}
