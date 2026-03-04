//! Oracle parity tests for complex `format` string patterns:
//! mixed numeric specs, width/padding, precision, %c with Unicode,
//! %S vs %s on structures, multi-line output, pretty printers, log builders.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// Mixed numeric format specs (%d, %o, %x, %X, %e, %f, %g)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_all_numeric_specs_mixed() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Combine every numeric format spec in a single format call
    let form = r####"(format "dec:%d oct:%o hex:%x HEX:%X float:%f sci:%e gen:%g"
                          255 255 255 255 3.14159 12345.6789 0.00042)"####;
    assert_oracle_parity_with_bootstrap(form);

    // Negative values across all integer specs
    let form2 = r#"(format "d:%d o:%o x:%x X:%X" -1 -1 -1 -1)"#;
    assert_oracle_parity_with_bootstrap(form2);

    // Zero across all specs
    let form3 = r#"(format "d:%d o:%o x:%x X:%X f:%f e:%e g:%g"
                           0 0 0 0 0.0 0.0 0.0)"#;
    assert_oracle_parity_with_bootstrap(form3);

    // Large values
    let form4 = r#"(format "d:%d x:%x f:%f e:%e"
                           1000000 1000000 1e10 1e-10)"#;
    assert_oracle_parity_with_bootstrap(form4);
}

// ---------------------------------------------------------------------------
// Width and padding (%10d, %-10s, %010d, %+d)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_width_and_padding_combinations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Right-aligned integer with width
    assert_oracle_parity_with_bootstrap(r#"(format "[%10d]" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%10d]" -42)"#);

    // Left-aligned string with width
    assert_oracle_parity_with_bootstrap(r#"(format "[%-10s]" "hi")"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-20s]" "hello world")"#);

    // Zero-padded integer
    assert_oracle_parity_with_bootstrap(r#"(format "[%010d]" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%010d]" -42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%06d]" 0)"#);

    // Plus sign for positive
    assert_oracle_parity_with_bootstrap(r#"(format "[%+d]" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%+d]" -42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%+d]" 0)"#);

    // Combined width, padding, alignment in one format string
    let form = r####"(format "|%8d|%-8d|%08d|%+8d|" 42 42 42 42)"####;
    assert_oracle_parity_with_bootstrap(form);

    // String padding combinations
    let form2 = r#"(format "|%15s|%-15s|" "right" "left")"#;
    assert_oracle_parity_with_bootstrap(form2);
}

// ---------------------------------------------------------------------------
// Precision for floats (%.2f, %.10e, %8.3f)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_float_precision() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Basic precision
    assert_oracle_parity_with_bootstrap(r#"(format "%.2f" 3.14159265)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.0f" 3.14159265)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.10f" 3.14159265)"#);

    // Scientific notation with precision
    assert_oracle_parity_with_bootstrap(r#"(format "%.2e" 12345.6789)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.10e" 1.0)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.0e" 12345.6789)"#);

    // Width + precision combined
    assert_oracle_parity_with_bootstrap(r#"(format "[%12.4f]" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%12.4f]" -3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-12.4f]" 3.14159)"#);

    // %g with precision
    assert_oracle_parity_with_bootstrap(r#"(format "%.2g" 0.00042)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.6g" 123456.789)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.2g" 12345.0)"#);

    // Multiple floats with different precisions in one format
    let form = r####"(format "a=%.1f b=%.3f c=%.5e d=%.2g"
                          1.23456 1.23456 1.23456 1.23456)"####;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// %c with various character codes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_char_codes() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ASCII printable range
    assert_oracle_parity_with_bootstrap(r#"(format "%c" 32)"#); // space
    assert_oracle_parity_with_bootstrap(r#"(format "%c" 65)"#); // A
    assert_oracle_parity_with_bootstrap(r#"(format "%c" 122)"#); // z
    assert_oracle_parity_with_bootstrap(r#"(format "%c" 126)"#); // ~

    // Character literals
    assert_oracle_parity_with_bootstrap(r#"(format "%c%c%c" ?H ?i ?!)"#);

    // Build a string from char codes using format
    let form = r####"(let ((codes '(72 101 108 108 111))
                        (result ""))
                    (dolist (c codes)
                      (setq result (concat result (format "%c" c))))
                    result)"####;
    assert_oracle_parity_with_bootstrap(form);

    // Mixed %c with other specs
    let form2 = r#"(format "char=%c code=%d hex=%x" ?A ?A ?A)"#;
    assert_oracle_parity_with_bootstrap(form2);
}

// ---------------------------------------------------------------------------
// %S (prin1) vs %s (princ) on complex structures
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_S_vs_s_complex() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // String quoting difference
    assert_oracle_parity_with_bootstrap(r#"(format "s=[%s] S=[%S]" "hello" "hello")"#);

    // Nested lists
    assert_oracle_parity_with_bootstrap(
        r#"(format "s=[%s] S=[%S]" '(1 "two" three) '(1 "two" three))"#,
    );

    // nil and t
    assert_oracle_parity_with_bootstrap(r#"(format "s=%s S=%S s=%s S=%S" nil nil t t)"#);

    // Dotted pairs
    assert_oracle_parity_with_bootstrap(r#"(format "s=%s S=%S" '(a . b) '(a . b))"#);

    // Nested alist with string values
    let form = r####"(format "%S"
                          '((name . "Alice")
                            (scores . (90 85 92))
                            (active . t)
                            (meta . nil)))"####;
    assert_oracle_parity_with_bootstrap(form);

    // Vectors
    assert_oracle_parity_with_bootstrap(r#"(format "s=%s S=%S" [1 2 3] [1 2 3])"#);

    // Deeply nested structure
    let form2 = r#"(format "%S" '((a (b (c (d . "deep"))))))"#;
    assert_oracle_parity_with_bootstrap(form2);
}

// ---------------------------------------------------------------------------
// Format producing multi-line table output
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_multiline_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a table row by row
    let form = r####"(let ((rows '(("Alice" 30 95.5)
                                 ("Bob" 25 87.3)
                                 ("Carol" 35 92.1)))
                        (header (format "%-10s %5s %8s" "Name" "Age" "Score"))
                        (sep (make-string 25 ?-))
                        (lines nil))
                    (setq lines (list header sep))
                    (dolist (row rows)
                      (setq lines
                            (append lines
                                    (list (format "%-10s %5d %8.1f"
                                                  (nth 0 row)
                                                  (nth 1 row)
                                                  (nth 2 row))))))
                    (mapconcat (lambda (l) l) lines "\n"))"####;
    assert_oracle_parity_with_bootstrap(form);

    // Format a multiplication table snippet
    let form2 = r#"(let ((result ""))
                     (dotimes (i 4)
                       (let ((row ""))
                         (dotimes (j 4)
                           (setq row (concat row (format "%4d" (* (1+ i) (1+ j))))))
                         (setq result (concat result row "\n"))))
                     result)"#;
    assert_oracle_parity_with_bootstrap(form2);
}

// ---------------------------------------------------------------------------
// Complex: format-based pretty printer for nested data
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_pretty_printer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Recursive pretty-printer that uses format to produce indented output
    let form = r####"(progn
  (fset 'neovm--test-pp
    (lambda (obj indent)
      (cond
        ((null obj) "nil")
        ((numberp obj) (format "%d" obj))
        ((stringp obj) (format "%S" obj))
        ((symbolp obj) (format "%s" obj))
        ((vectorp obj)
         (let ((parts nil))
           (dotimes (i (length obj))
             (setq parts
                   (append parts
                           (list (funcall 'neovm--test-pp
                                          (aref obj i)
                                          (+ indent 2))))))
           (format "[%s]" (mapconcat (lambda (p) p) parts " "))))
        ((consp obj)
         (if (and (consp (cdr obj)) (null (cddr obj)))
             ;; 2-element list on one line
             (format "(%s %s)"
                     (funcall 'neovm--test-pp (car obj) indent)
                     (funcall 'neovm--test-pp (cadr obj) indent))
           ;; Multi-element: indent children
           (let ((parts nil)
                 (remaining obj))
             (while (consp remaining)
               (setq parts
                     (append parts
                             (list (funcall 'neovm--test-pp
                                            (car remaining)
                                            (+ indent 2)))))
               (setq remaining (cdr remaining)))
             (let ((inner (mapconcat (lambda (p) p) parts " ")))
               (format "(%s)" inner)))))
        (t (format "%S" obj)))))
  (unwind-protect
      (funcall 'neovm--test-pp
               '(defun greet (name)
                  (message "Hello %s" name)
                  (list name 42))
               0)
    (fmakunbound 'neovm--test-pp)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: format-based log message builder with levels
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_log_builder() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Log system that formats messages with level, timestamp-like counter, context
    let form = r####"(let ((log-entries nil)
                        (log-counter 0)
                        (log-fn
                         (lambda (level component msg &rest args)
                           (setq log-counter (1+ log-counter))
                           (let ((formatted
                                  (format "[%04d] %-5s [%-10s] %s"
                                          log-counter
                                          (upcase (symbol-name level))
                                          component
                                          (apply #'format msg args))))
                             (setq log-entries
                                   (append log-entries (list formatted)))
                             formatted))))
                    ;; Emit various log messages
                    (funcall log-fn 'info "startup" "System starting v%d.%d" 2 1)
                    (funcall log-fn 'debug "config" "Loaded %d settings" 42)
                    (funcall log-fn 'warn "network" "Retrying connection %d/%d" 3 5)
                    (funcall log-fn 'error "auth" "Failed login for %S" "admin")
                    (funcall log-fn 'info "startup" "Ready in %.2f seconds" 1.337)
                    (funcall log-fn 'debug "cache" "Hit ratio: %d%%" 87)
                    ;; Return all formatted entries
                    (mapconcat (lambda (e) e) log-entries "\n"))"####;
    assert_oracle_parity_with_bootstrap(form);
}
