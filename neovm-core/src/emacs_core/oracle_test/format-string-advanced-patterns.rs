//! Oracle parity tests for advanced format string edge cases:
//! all format directives with parameter variations, field width, padding,
//! precision, left-justify flags, nested format calls, format with special
//! chars/unicode, %S vs %s differences, float precision edge cases,
//! and format producing very long strings.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;
use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// %s (princ) vs %S (prin1) differences with various types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_s_vs_S_differences() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // String: %s strips quotes, %S keeps them
    assert_oracle_parity_with_bootstrap(r#"(format "%s" "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" "hello")"#);

    // String with special chars
    assert_oracle_parity_with_bootstrap(r#"(format "%s" "line1\nline2")"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" "line1\nline2")"#);

    // nil
    assert_oracle_parity_with_bootstrap(r#"(format "%s" nil)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" nil)"#);

    // t
    assert_oracle_parity_with_bootstrap(r#"(format "%s" t)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" t)"#);

    // Symbols
    assert_oracle_parity_with_bootstrap(r#"(format "%s" 'hello)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" 'hello)"#);

    // Lists
    assert_oracle_parity_with_bootstrap(r#"(format "%s" '(1 2 3))"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" '(1 2 3))"#);

    // Nested lists
    assert_oracle_parity_with_bootstrap(r#"(format "%s" '(a (b c) (d (e f))))"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" '(a (b c) (d (e f))))"#);

    // Vectors
    assert_oracle_parity_with_bootstrap(r#"(format "%s" [1 2 3])"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" [1 2 3])"#);

    // Cons pairs (dotted)
    assert_oracle_parity_with_bootstrap(r#"(format "%s" '(a . b))"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" '(a . b))"#);

    // String with embedded quotes
    assert_oracle_parity_with_bootstrap(r#"(format "%S" "he said \"hi\"")"#);

    // Characters
    assert_oracle_parity_with_bootstrap(r#"(format "%s" ?A)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" ?A)"#);
}

// ---------------------------------------------------------------------------
// %c with various character values including unicode
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_c_characters() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ASCII printable
    assert_oracle_parity_with_bootstrap(r#"(format "%c" 65)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%c" 122)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%c" 48)"#);

    // Space and special ASCII
    assert_oracle_parity_with_bootstrap(r#"(format "%c" 32)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%c" 126)"#);

    // Unicode characters
    assert_oracle_parity_with_bootstrap(r#"(format "%c" #x03B1)"#); // alpha
    assert_oracle_parity_with_bootstrap(r#"(format "%c" #x03B2)"#); // beta
    assert_oracle_parity_with_bootstrap(r#"(format "%c" #x4e16)"#); // CJK char

    // Multiple %c in one format
    assert_oracle_parity_with_bootstrap(r#"(format "%c%c%c%c%c" 72 101 108 108 111)"#);

    // %c with width
    assert_oracle_parity_with_bootstrap(r#"(format "[%5c]" 65)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-5c]" 65)"#);
}

// ---------------------------------------------------------------------------
// %d with all flag combinations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_d_all_flags() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Basic
    assert_oracle_parity_with_bootstrap(r#"(format "%d" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%d" -42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%d" 0)"#);

    // Width
    assert_oracle_parity_with_bootstrap(r#"(format "%10d" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%10d" -42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%3d" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%1d" 42)"#);

    // Zero-padding
    assert_oracle_parity_with_bootstrap(r#"(format "%010d" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%010d" -42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%05d" 0)"#);

    // Left-justify
    assert_oracle_parity_with_bootstrap(r#"(format "[%-10d]" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-10d]" -42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-3d]" 42)"#);

    // Plus sign
    assert_oracle_parity_with_bootstrap(r#"(format "%+d" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%+d" -42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%+d" 0)"#);

    // Combined flags
    assert_oracle_parity_with_bootstrap(r#"(format "[%+10d]" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-+10d]" 42)"#);

    // Large numbers
    assert_oracle_parity_with_bootstrap(r#"(format "%d" 1000000000)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%d" -1000000000)"#);
}

// ---------------------------------------------------------------------------
// %o (octal) and %x/%X (hex) with flags
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_octal_hex_flags() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Basic octal
    assert_oracle_parity_with_bootstrap(r#"(format "%o" 8)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%o" 255)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%o" 0)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%o" 511)"#);

    // Octal with width
    assert_oracle_parity_with_bootstrap(r#"(format "%10o" 255)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%010o" 255)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-10o]" 255)"#);

    // Hex lowercase
    assert_oracle_parity_with_bootstrap(r#"(format "%x" 255)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%x" 4096)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%x" 0)"#);

    // Hex uppercase
    assert_oracle_parity_with_bootstrap(r#"(format "%X" 255)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%X" 4096)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%X" 48879)"#); // 0xBEEF

    // Hex with width and zero-pad
    assert_oracle_parity_with_bootstrap(r#"(format "%08x" 255)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%08X" 255)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-10x]" 255)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-10X]" 255)"#);

    // All integer formats in one call
    assert_oracle_parity_with_bootstrap(r#"(format "d=%d o=%o x=%x X=%X" 42 42 42 42)"#);
}

// ---------------------------------------------------------------------------
// %e, %f, %g with precision variations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_float_precision_extensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // %f with various precisions
    assert_oracle_parity_with_bootstrap(r#"(format "%.0f" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.1f" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.2f" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.5f" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.10f" 3.14159)"#);

    // %f with width and precision
    assert_oracle_parity_with_bootstrap(r#"(format "%10.2f" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%10.2f" -3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%010.2f" 3.14)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-10.2f]" 3.14)"#);

    // %e with precisions
    assert_oracle_parity_with_bootstrap(r#"(format "%.0e" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.2e" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.5e" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.2e" 0.001)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.2e" 123456.789)"#);

    // %e with width
    assert_oracle_parity_with_bootstrap(r#"(format "%15.3e" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-15.3e]" 3.14159)"#);

    // %g chooses between %f and %e style
    assert_oracle_parity_with_bootstrap(r#"(format "%g" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%g" 100000.0)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%g" 0.0001)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.2g" 3.14159)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.10g" 3.14159)"#);

    // Negative floats
    assert_oracle_parity_with_bootstrap(r#"(format "%.3f" -0.0)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.3e" -1.5)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%+.2f" 3.14)"#);

    // Very small / very large
    assert_oracle_parity_with_bootstrap(r#"(format "%e" 1e-15)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%e" 1e15)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%f" 1e10)"#);
}

// ---------------------------------------------------------------------------
// Nested format calls
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_nested_calls() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Format producing a format string that is then used
    assert_oracle_parity_with_bootstrap(r#"(format (format "%%0%dd" 5) 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format (format "%%-%ds" 10) "hello")"#);

    // Nested format as argument
    assert_oracle_parity_with_bootstrap(r#"(format "result: %s" (format "%d + %d = %d" 3 4 7))"#);
    assert_oracle_parity_with_bootstrap(
        r#"(format "[%s] [%s]"
                                    (format "%05d" 42)
                                    (format "%-10s" "hi"))"#,
    );

    // Triple nesting
    assert_oracle_parity_with_bootstrap(
        r#"(format "outer(%s)"
                                    (format "mid(%s)"
                                            (format "inner(%d)" 42)))"#,
    );

    // Format in a loop building a string
    let form = r#"(let ((parts nil) (i 0))
  (while (< i 5)
    (setq parts (cons (format "[%02d:%s]" i (make-string (1+ i) ?*)) parts))
    (setq i (1+ i)))
  (mapconcat #'identity (nreverse parts) "-"))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Format with nil, t, symbols, lists, vectors as arguments
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_diverse_arg_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // nil and t
    assert_oracle_parity_with_bootstrap(r#"(format "%s %s %S %S" nil t nil t)"#);

    // Symbols
    assert_oracle_parity_with_bootstrap(r#"(format "sym: %s %S" 'hello 'hello)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%s" 'with-special-chars)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" 'with-special-chars)"#);

    // Lists
    assert_oracle_parity_with_bootstrap(r#"(format "%s" '(1 2 3))"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" '(1 "two" three))"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%s" '(a . b))"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" '(a b . c))"#);

    // Vectors
    assert_oracle_parity_with_bootstrap(r#"(format "%s" [1 2 3])"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" [1 "two" three])"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%s" [])"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" [])"#);

    // Integers as %s
    assert_oracle_parity_with_bootstrap(r#"(format "%s" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" 42)"#);

    // Floats as %s
    assert_oracle_parity_with_bootstrap(r#"(format "%s" 3.14)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" 3.14)"#);

    // Mixed in one call
    assert_oracle_parity_with_bootstrap(
        r#"(format "%s|%S|%s|%S|%s"
                                    nil '(a b) [1 2] "text" 42)"#,
    );
}

// ---------------------------------------------------------------------------
// Format with %% literal percent
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_literal_percent() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(format "100%%")"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%d%%" 50)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%%d = %d" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%%%%")"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%s is %d%% complete" "task" 75)"#);
}

// ---------------------------------------------------------------------------
// Format with unicode strings
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_unicode_strings() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(format "%s" "cafe\u0301")"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%s %s" "hello" "world")"#);

    // Width with unicode (interesting because char width != byte width)
    assert_oracle_parity_with_bootstrap(r#"(format "%s" "\u03b1\u03b2\u03b3")"#);

    // Mixed ASCII and unicode
    assert_oracle_parity_with_bootstrap(r#"(format "Greek: %s, Number: %d" "\u03c0" 314)"#);

    // CJK characters
    assert_oracle_parity_with_bootstrap(r#"(format "%s" "\u4e16\u754c")"#);
}

// ---------------------------------------------------------------------------
// Format producing very long strings
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_long_output() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Wide field with short content
    assert_oracle_parity_with_bootstrap(r#"(format "%100s" "x")"#);
    assert_oracle_parity_with_bootstrap(r#"(format "[%-100s]" "x")"#);

    // Multiple wide fields
    assert_oracle_parity_with_bootstrap(r#"(length (format "%50s%50s%50s" "a" "b" "c"))"#);

    // Repeated format in accumulation
    let form = r#"(let ((parts nil) (i 0))
  (while (< i 20)
    (setq parts (cons (format "%05d" (* i i)) parts))
    (setq i (1+ i)))
  (mapconcat #'identity (nreverse parts) ","))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Format edge cases: empty string, no args, excess args
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_edge_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // No format specs
    assert_oracle_parity_with_bootstrap(r#"(format "hello world")"#);
    assert_oracle_parity_with_bootstrap(r#"(format "")"#);

    // Width of 0
    assert_oracle_parity_with_bootstrap(r#"(format "%0d" 42)"#);

    // String arg wider than field
    assert_oracle_parity_with_bootstrap(r#"(format "%3s" "hello world")"#);

    // Integer arg wider than field
    assert_oracle_parity_with_bootstrap(r#"(format "%1d" 123456789)"#);

    // Float with precision 0
    assert_oracle_parity_with_bootstrap(r#"(format "%.0f" 3.5)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.0f" 3.4)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%.0f" -2.5)"#);

    // Very large precision
    assert_oracle_parity_with_bootstrap(r#"(format "%.15f" 1.0)"#);

    // Format with special chars in literal parts
    assert_oracle_parity_with_bootstrap(r#"(format "tab:\there" )"#);
    assert_oracle_parity_with_bootstrap(r#"(format "newline:\n%d" 42)"#);

    // Multiple format specs with same value types
    assert_oracle_parity_with_bootstrap(r#"(format "%d %d %d %d %d" 1 2 3 4 5)"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%s %s %s" "a" "b" "c")"#);
}

// ---------------------------------------------------------------------------
// Format table-building patterns (aligned columns)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_table_alignment() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a formatted table
    let form = r#"(let ((header (format "%-12s %8s %8s %10s" "Name" "Age" "Score" "Status"))
      (rows '(("Alice" 30 95 "pass")
              ("Bob" 25 67 "fail")
              ("Carol" 35 88 "pass")
              ("Dave" 28 72 "pass")))
      (lines nil))
  (setq lines (list header))
  (setq lines (cons (make-string (length header) ?-) lines))
  (dolist (row rows)
    (setq lines
          (cons (format "%-12s %8d %8d %10s"
                        (nth 0 row) (nth 1 row) (nth 2 row) (nth 3 row))
                lines)))
  (mapconcat #'identity (nreverse lines) "\n"))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Format with %s on list structures of varying depth
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_nested_structures() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(format "%S" '((1 2) (3 4) (5 6)))"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" '((a . 1) (b . 2) (c . 3)))"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" '(lambda (x) (* x x)))"#);
    assert_oracle_parity_with_bootstrap(r#"(format "%S" (list 'quote (list 1 2 3)))"#);

    // Deep nesting
    assert_oracle_parity_with_bootstrap(r#"(format "%S" '(a (b (c (d (e))))))"#);

    // Mixed types in deep structure
    assert_oracle_parity_with_bootstrap(r#"(format "%S" (list 1 "two" 'three [4 5] '(6 . 7)))"#);

    // Empty collections
    assert_oracle_parity_with_bootstrap(r#"(format "%S %S %S" nil '() [])"#);
}

// ---------------------------------------------------------------------------
// Format with min-width and various directives combined
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_format_combined_directives() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // All directives in one format string
    assert_oracle_parity_with_bootstrap(
        r#"(format "s=%s S=%S d=%d o=%o x=%x X=%X c=%c f=%.2f e=%.2e g=%g %%"
                  "hi" 'sym 42 42 42 42 65 3.14 3.14 0.0042)"#,
    );

    // Repeated same directive
    assert_oracle_parity_with_bootstrap(r#"(format "%d+%d+%d=%d" 1 2 3 6)"#);

    // Width and precision with every numeric type
    let form = r#"(format "%8d %8o %8x %8X %10.3f %12.3e %10g" 255 255 255 255 3.14 3.14 3.14)"#;
    assert_oracle_parity_with_bootstrap(form);

    // Zero-pad with every integer type
    assert_oracle_parity_with_bootstrap(r#"(format "%08d %08o %08x %08X" 42 42 42 42)"#);

    // Left-justify with every type
    assert_oracle_parity_with_bootstrap(
        r#"(format "[%-8d][%-8o][%-8x][%-8s][%-8.2f]" 42 42 42 "hi" 3.14)"#,
    );
}
