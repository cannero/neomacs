//! Advanced oracle parity tests for `string-to-number`.
//!
//! Tests: base 10/16/8/2 parsing, leading whitespace, trailing garbage,
//! edge cases (empty string, "0", negative numbers), float parsing,
//! very large numbers, and roundtrip with `number-to-string`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// Multi-base parsing: base 10, 16, 8, 2 with various inputs
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_to_number_multi_base() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Base 10 (default)
  (string-to-number "0")
  (string-to-number "1")
  (string-to-number "-1")
  (string-to-number "42")
  (string-to-number "-42")
  (string-to-number "999999")
  (string-to-number "-999999")
  ;; Base 16
  (string-to-number "0" 16)
  (string-to-number "a" 16)
  (string-to-number "A" 16)
  (string-to-number "ff" 16)
  (string-to-number "FF" 16)
  (string-to-number "DeAdBeEf" 16)
  (string-to-number "-ff" 16)
  (string-to-number "10" 16)
  (string-to-number "100" 16)
  ;; Base 8
  (string-to-number "0" 8)
  (string-to-number "7" 8)
  (string-to-number "10" 8)
  (string-to-number "77" 8)
  (string-to-number "777" 8)
  (string-to-number "-77" 8)
  (string-to-number "100" 8)
  ;; Base 2
  (string-to-number "0" 2)
  (string-to-number "1" 2)
  (string-to-number "10" 2)
  (string-to-number "101" 2)
  (string-to-number "11111111" 2)
  (string-to-number "-101" 2)
  (string-to-number "10000000" 2))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Leading whitespace and trailing garbage behavior
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_to_number_whitespace_and_garbage() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Leading whitespace is accepted
  (string-to-number "  42")
  (string-to-number "  -7")
  (string-to-number "   0")
  (string-to-number " 100")
  ;; Trailing garbage: parsing stops at first non-numeric char
  (string-to-number "42abc")
  (string-to-number "123 456")
  (string-to-number "99.5xyz")
  (string-to-number "0xDEAD")
  (string-to-number "-5rest")
  ;; Leading whitespace + trailing garbage
  (string-to-number "  42abc")
  (string-to-number "  -7xyz")
  ;; Hex with trailing garbage
  (string-to-number "ffgg" 16)
  (string-to-number "a0zz" 16)
  ;; Octal with digits out of range
  (string-to-number "89" 8)
  (string-to-number "78" 8)
  ;; Binary with out-of-range digits
  (string-to-number "102" 2)
  (string-to-number "12" 2)
  ;; Tabs and mixed whitespace
  (string-to-number "	42")
  (string-to-number "
42"))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Edge cases: empty string, zero variants, sign-only, special strings
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_to_number_edge_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Empty string
  (string-to-number "")
  ;; Zero in various forms
  (string-to-number "0")
  (string-to-number "-0")
  (string-to-number "+0")
  (string-to-number "00000")
  ;; Sign only
  (string-to-number "+")
  (string-to-number "-")
  ;; Just whitespace
  (string-to-number "   ")
  ;; Non-numeric strings
  (string-to-number "abc")
  (string-to-number "hello")
  (string-to-number "nil")
  (string-to-number "t")
  ;; Plus sign prefix
  (string-to-number "+42")
  (string-to-number "+100")
  ;; Leading zeros
  (string-to-number "007")
  (string-to-number "0042")
  (string-to-number "-007")
  ;; Single digit
  (string-to-number "0")
  (string-to-number "5")
  (string-to-number "9")
  ;; Max-ish integers
  (string-to-number "536870911")
  (string-to-number "-536870912"))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Float parsing
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_to_number_floats() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Basic floats
  (string-to-number "3.14")
  (string-to-number "-2.718")
  (string-to-number "0.0")
  (string-to-number "-0.0")
  (string-to-number "1.0")
  (string-to-number ".5")
  (string-to-number "-.5")
  ;; Scientific notation
  (string-to-number "1e10")
  (string-to-number "1E10")
  (string-to-number "2.5e3")
  (string-to-number "-1.5e2")
  (string-to-number "1e-3")
  (string-to-number "1.0e0")
  (string-to-number "1e+5")
  ;; Float-like edge cases
  (string-to-number "0.0e0")
  (string-to-number "100.")
  ;; Integer vs float distinction
  (integerp (string-to-number "42"))
  (floatp (string-to-number "42.0"))
  (floatp (string-to-number "1e10"))
  ;; Float with trailing garbage
  (string-to-number "3.14abc")
  (string-to-number "1e10xyz")
  ;; Comparison of parsed float
  (= (string-to-number "0.1") 0.1)
  (= (string-to-number "1.0") 1.0))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Roundtrip: number-to-string -> string-to-number and back
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_to_number_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Integer roundtrips
  (string-to-number (number-to-string 0))
  (string-to-number (number-to-string 42))
  (string-to-number (number-to-string -42))
  (string-to-number (number-to-string 1000000))
  (string-to-number (number-to-string -1000000))
  ;; number-to-string -> string-to-number -> number-to-string
  (number-to-string (string-to-number "12345"))
  (number-to-string (string-to-number "-12345"))
  (number-to-string (string-to-number "0"))
  ;; Float roundtrips
  (= (string-to-number (number-to-string 3.14)) 3.14)
  (= (string-to-number (number-to-string -2.5)) -2.5)
  (= (string-to-number (number-to-string 0.0)) 0.0)
  ;; Chained roundtrips: start with string
  (string-to-number (number-to-string (string-to-number "99")))
  (string-to-number (number-to-string (string-to-number "-99")))
  ;; Verify identity property for integers
  (let ((nums '(0 1 -1 42 -42 100 -100 9999 -9999)))
    (mapcar (lambda (n)
              (= n (string-to-number (number-to-string n))))
            nums)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Combination: string-to-number in arithmetic and conditional pipelines
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_to_number_in_pipelines() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Arithmetic on parsed numbers
  (+ (string-to-number "10") (string-to-number "20"))
  (* (string-to-number "6") (string-to-number "7"))
  (- (string-to-number "100") (string-to-number "42"))
  ;; Mixing bases in arithmetic
  (+ (string-to-number "ff" 16) (string-to-number "1"))
  (+ (string-to-number "77" 8) (string-to-number "10" 2))
  ;; Sum of decimal strings
  (let ((strings '("10" "20" "30" "40")))
    (apply '+ (mapcar 'string-to-number strings)))
  ;; Parse and compare
  (< (string-to-number "10") (string-to-number "20"))
  (> (string-to-number "100") (string-to-number "99"))
  (= (string-to-number "42") (string-to-number "  42"))
  ;; Parse in let bindings with computation
  (let* ((a (string-to-number "12"))
         (b (string-to-number "5"))
         (sum (+ a b))
         (product (* a b))
         (diff (- a b)))
    (list sum product diff))
  ;; Format-parse pipeline
  (string-to-number (format "%d" 255))
  (string-to-number (format "%x" 255) 16)
  (string-to-number (format "%o" 255) 8)
  ;; Conditional on parsed number
  (let ((val (string-to-number "0")))
    (if (= val 0) 'zero 'nonzero))
  (let ((val (string-to-number "42")))
    (if (= val 0) 'zero 'nonzero)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Large numbers and boundary values
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_string_to_number_large_and_boundary() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Large positive integers
  (string-to-number "1000000000")
  (string-to-number "999999999")
  ;; Large negative integers
  (string-to-number "-1000000000")
  (string-to-number "-999999999")
  ;; Very large floats
  (string-to-number "1e100")
  (string-to-number "-1e100")
  (string-to-number "9.99e99")
  ;; Very small floats
  (string-to-number "1e-100")
  (string-to-number "-1e-100")
  ;; Powers of 2 in base 2
  (string-to-number "100000000" 2)
  (string-to-number "1000000000" 2)
  ;; Large hex values
  (string-to-number "FFFFFF" 16)
  (string-to-number "100000" 16)
  ;; Boundary: where fixnum overflows to float
  ;; (behavior depends on Emacs integer width)
  (integerp (string-to-number "536870911"))
  (integerp (string-to-number "-536870912"))
  ;; Multiple zeros
  (string-to-number "0000000000")
  (string-to-number "-0000000000")
  ;; Type checks on parsed results
  (numberp (string-to-number "42"))
  (numberp (string-to-number "3.14"))
  (numberp (string-to-number "")))"#;
    assert_oracle_parity(form);
}
