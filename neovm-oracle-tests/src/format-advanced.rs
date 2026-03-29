//! Oracle parity tests for advanced `format` patterns:
//! all format specs (`%d`, `%x`, `%o`, `%e`, `%f`, `%g`, `%s`, `%S`,
//! `%c`), width/padding/precision, and complex formatting pipelines.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// Integer format specs
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_integer_specs() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (format "%d" 42)
                        (format "%d" -42)
                        (format "%d" 0)
                        (format "%x" 255)
                        (format "%X" 255)
                        (format "%o" 8)
                        (format "%o" 255))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Width and padding
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_width_padding() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (format "%10d" 42)
                        (format "%-10d" 42)
                        (format "%010d" 42)
                        (format "%10s" "hello")
                        (format "%-10s" "hello")
                        (format "%5d" 123456))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Float format specs
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_float_specs() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (format "%f" 3.14159)
                        (format "%.2f" 3.14159)
                        (format "%.0f" 3.14159)
                        (format "%e" 12345.6789)
                        (format "%.2e" 12345.6789)
                        (format "%g" 0.00001)
                        (format "%g" 12345.0))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_format_float_width() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (format "%10.2f" 3.14)
                        (format "%-10.2f" 3.14)
                        (format "%010.2f" 3.14))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Character format
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_char() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (format "%c" 65)
                        (format "%c" ?a)
                        (format "%c" ?Z)
                        (format "%c%c%c" ?H ?i ?!))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// %s vs %S (prin1 vs princ)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_s_vs_S() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (format "%s" "hello")
                        (format "%S" "hello")
                        (format "%s" 42)
                        (format "%S" 42)
                        (format "%s" '(a b c))
                        (format "%S" '(a b c))
                        (format "%s" nil)
                        (format "%S" nil))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Multiple arguments
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_multiple_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
                    (format "%s is %d years old" "Alice" 30)
                    (format "[%5d] %-20s %6.2f" 1 "item" 9.99)
                    (format "%s + %s = %s" 1 2 (+ 1 2))
                    (format "0x%04X = %d = 0%o" 255 255 255))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// %% literal percent
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_literal_percent() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (format "100%%")
                        (format "%d%%" 42)
                        (format "%%s is not a format"))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: table formatter
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a formatted table
    let form = r#"(let ((header (format "%-12s %6s %10s" "Name" "Age" "Score"))
                        (sep (make-string 30 ?-))
                        (rows '(("Alice" 30 95.5)
                                ("Bob" 25 87.2)
                                ("Carol" 35 92.8))))
                    (let ((formatted-rows
                           (mapcar
                            (lambda (row)
                              (format "%-12s %6d %10.1f"
                                      (nth 0 row)
                                      (nth 1 row)
                                      (nth 2 row)))
                            rows)))
                      (mapconcat #'identity
                                 (append (list header sep)
                                         formatted-rows)
                                 "\n")))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: sprintf-like number formatter
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_number_formatter() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Format numbers in various human-readable ways
    let form = r#"(let ((format-bytes
                         (lambda (n)
                           (cond
                            ((>= n (* 1024 1024 1024))
                             (format "%.1f GB" (/ (float n) 1024 1024 1024)))
                            ((>= n (* 1024 1024))
                             (format "%.1f MB" (/ (float n) 1024 1024)))
                            ((>= n 1024)
                             (format "%.1f KB" (/ (float n) 1024)))
                            (t (format "%d B" n))))))
                    (mapcar format-bytes
                            '(42 1024 1536 1048576 1073741824
                              5368709120)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
