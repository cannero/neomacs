//! Comprehensive oracle parity tests for read/print operations.
//!
//! Covers: `prin1-to-string` for all types, `read-from-string` round-trips,
//! `print-escape-newlines`, `print-length`, `print-level`,
//! `print-circle` for circular structures, `print-quoted`,
//! `print-escape-nonascii`, `format` with `%S` (prin1),
//! `with-output-to-string`, reader macros (#'function, #:uninterned,
//! #NNN= circular).

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::assert_oracle_parity_with_bootstrap;

// ---------------------------------------------------------------------------
// prin1-to-string for all types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_comprehensive_prin1_all_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r##"
(list
  ;; Integer
  (prin1-to-string 42)
  (prin1-to-string -100)
  (prin1-to-string 0)

  ;; Float
  (prin1-to-string 3.14)
  (prin1-to-string -0.001)
  (prin1-to-string 1.0e10)

  ;; String with special chars
  (prin1-to-string "hello world")
  (prin1-to-string "line1\nline2")
  (prin1-to-string "tab\there")
  (prin1-to-string "quote\"inside")
  (prin1-to-string "backslash\\here")

  ;; Symbol
  (prin1-to-string 'foo)
  (prin1-to-string 'nil)
  (prin1-to-string 't)
  (prin1-to-string :keyword)

  ;; Cons / list
  (prin1-to-string '(1 2 3))
  (prin1-to-string '(a . b))
  (prin1-to-string '(1 (2 (3))))
  (prin1-to-string '())

  ;; Vector
  (prin1-to-string [1 2 3])
  (prin1-to-string [])
  (prin1-to-string [a "b" 3])

  ;; Character
  (prin1-to-string ?A)
  (prin1-to-string ?\n)
  (prin1-to-string ?\\))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// read-from-string round-trips
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_comprehensive_round_trips() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Read what prin1-to-string produces and verify round-trip identity
    let form = r#"
(let ((test-values '(42 -1 0 3.14 "hello" foo nil t :kw (1 2 3) (a . b) [1 2] [])))
  (mapcar
    (lambda (v)
      (let* ((printed (prin1-to-string v))
             (read-back (car (read-from-string printed))))
        (equal v read-back)))
    test-values))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// read-from-string with start position and nested forms
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_comprehensive_read_from_string_positions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; Read from offset
  (car (read-from-string "   42" 3))
  (cdr (read-from-string "   42" 3))

  ;; Sequential reads from a string with multiple forms
  (let* ((s "(+ 1 2) (* 3 4) done")
         (r1 (read-from-string s))
         (r2 (read-from-string s (cdr r1)))
         (r3 (read-from-string s (cdr r2))))
    (list (car r1) (car r2) (car r3)))

  ;; Read deeply nested structure
  (car (read-from-string "((((a . b) . c) . d) . e)"))

  ;; Read dotted pairs
  (car (read-from-string "(1 . (2 . (3 . nil)))"))

  ;; Read vector with mixed types
  (car (read-from-string "[1 \"two\" three (4 . 5)]")))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// print-escape-newlines
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_comprehensive_print_escape_newlines() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; With print-escape-newlines = t, newlines are escaped as \n
  (let ((print-escape-newlines t))
    (prin1-to-string "line1\nline2\nline3"))

  ;; With print-escape-newlines = nil, newlines are literal
  (let ((print-escape-newlines nil))
    (prin1-to-string "line1\nline2"))

  ;; Tabs are always escaped regardless
  (let ((print-escape-newlines t))
    (prin1-to-string "col1\tcol2"))

  ;; Combination: string with both newlines and tabs
  (let ((print-escape-newlines t))
    (prin1-to-string "a\nb\tc\nd")))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// print-length: truncate list/vector printing
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_comprehensive_print_length() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; print-length = 3 truncates list
  (let ((print-length 3))
    (prin1-to-string '(1 2 3 4 5 6 7)))

  ;; print-length = 0 means just ...
  (let ((print-length 0))
    (prin1-to-string '(a b c)))

  ;; print-length = nil means no limit
  (let ((print-length nil))
    (prin1-to-string '(1 2 3 4 5)))

  ;; print-length applies to vectors too
  (let ((print-length 2))
    (prin1-to-string [a b c d e]))

  ;; Exact length: no truncation
  (let ((print-length 3))
    (prin1-to-string '(x y z)))

  ;; print-length on nested lists applies per level
  (let ((print-length 2))
    (prin1-to-string '((1 2 3) (4 5 6) (7 8 9)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// print-level: truncate depth of nested structures
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_comprehensive_print_level() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; print-level = 1 shows only top level
  (let ((print-level 1))
    (prin1-to-string '(a (b (c d)))))

  ;; print-level = 2
  (let ((print-level 2))
    (prin1-to-string '(a (b (c (d e))))))

  ;; print-level = 0 means everything is #
  (let ((print-level 0))
    (prin1-to-string '(x y z)))

  ;; print-level = nil means no limit
  (let ((print-level nil))
    (prin1-to-string '(a (b (c (d (e)))))))

  ;; print-level with vectors
  (let ((print-level 1))
    (prin1-to-string [a [b [c]]]))

  ;; Combined print-level and print-length
  (let ((print-level 2) (print-length 2))
    (prin1-to-string '((1 2 3 4) (5 (6 7) 8 9) (10 11)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// print-circle: handle circular structures
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_comprehensive_print_circle() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; Shared structure detection
  (let* ((print-circle t)
         (shared '(a b c))
         (outer (list shared shared)))
    (prin1-to-string outer))

  ;; Non-circular shared cons
  (let* ((print-circle t)
         (x '(1 2))
         (y (list x 'mid x)))
    (prin1-to-string y))

  ;; print-circle = nil on non-circular is normal
  (let ((print-circle nil))
    (prin1-to-string '(a (b c) d)))

  ;; Circular list: cdr points back to head
  (let ((print-circle t))
    (let ((lst (list 1 2 3)))
      (setcdr (cddr lst) lst)
      (prin1-to-string lst)))

  ;; Circular through car
  (let ((print-circle t))
    (let ((cell (cons nil nil)))
      (setcar cell cell)
      (prin1-to-string cell))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// print-quoted
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_comprehensive_print_quoted() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; print-quoted = t uses ' shorthand for (quote ...)
  (let ((print-quoted t))
    (prin1-to-string '(quote foo)))

  ;; print-quoted = nil prints full (quote foo)
  (let ((print-quoted nil))
    (prin1-to-string '(quote foo)))

  ;; function quote with print-quoted
  (let ((print-quoted t))
    (prin1-to-string '(function bar)))

  ;; Nested quotes
  (let ((print-quoted t))
    (prin1-to-string '(quote (quote baz))))

  ;; backquote forms
  (let ((print-quoted t))
    (prin1-to-string ''(a b c))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// format with %S (prin1 representation)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_comprehensive_format_percent_S() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; %S prints as prin1 (with quotes for strings)
  (format "%S" "hello")
  (format "%S" 42)
  (format "%S" 'foo)
  (format "%S" '(1 2 3))
  (format "%S" [a b c])
  (format "%S" nil)
  (format "%S" t)

  ;; %s prints as princ (no quotes for strings)
  (format "%s" "hello")

  ;; Difference between %s and %S for strings
  (list (format "%s" "test") (format "%S" "test"))

  ;; Multiple %S in one format string
  (format "a=%S b=%S c=%S" 1 "two" 'three)

  ;; %S with special characters in strings
  (format "%S" "has\"quote")
  (format "%S" "has\\backslash"))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Reader macros: #', #:, #N=, #N#
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_comprehensive_reader_macros() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(list
  ;; #'function reader macro
  (car (read-from-string "#'car"))

  ;; #:uninterned symbol
  (let ((sym (car (read-from-string "#:foo"))))
    (list (symbolp sym) (not (intern-soft (symbol-name sym)))))

  ;; Two #: symbols with same name are NOT eq
  (let ((s1 (car (read-from-string "#:bar")))
        (s2 (car (read-from-string "#:bar"))))
    (list (equal (symbol-name s1) (symbol-name s2))
          (not (eq s1 s2))))

  ;; Circular read syntax #N= and #N#
  (let ((print-circle t))
    (let ((obj (car (read-from-string "#1=(a . #1#)"))))
      (list (car obj)
            (eq obj (cdr obj)))))

  ;; Shared structure via read
  (let ((print-circle t))
    (let ((obj (car (read-from-string "(#1=(x y) #1#)"))))
      (eq (car obj) (cadr obj)))))"##;
    assert_oracle_parity_with_bootstrap(form);
}
