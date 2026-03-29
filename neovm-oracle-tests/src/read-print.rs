//! Oracle parity tests for `read-from-string`, `prin1-to-string`,
//! `message`, `char-equal`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// read-from-string
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_from_string_integer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "42"))"#);
    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "-7"))"#);
    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "0"))"#);
}

#[test]
fn oracle_prop_read_from_string_symbol() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "hello"))"#);
    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "nil"))"#);
    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "t"))"#);
}

#[test]
fn oracle_prop_read_from_string_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "(1 2 3)"))"#);
    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "(a . b)"))"#);
    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "()"))"#);
}

#[test]
fn oracle_prop_read_from_string_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "\"hello\""))"#);
}

#[test]
fn oracle_prop_read_from_string_returns_position() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // read-from-string returns (VALUE . END-POS)
    assert_oracle_parity_with_bootstrap(r#"(cdr (read-from-string "42 rest"))"#);
    assert_oracle_parity_with_bootstrap(r#"(cdr (read-from-string "(1 2) rest"))"#);
}

#[test]
fn oracle_prop_read_from_string_start_position() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // START parameter (2nd arg)
    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "xxx 42" 4))"#);
}

#[test]
fn oracle_prop_read_from_string_vector() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(car (read-from-string "[1 2 3]"))"#);
}

#[test]
fn oracle_prop_read_from_string_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Read what prin1-to-string produces
    let form = r####"(let ((val '(1 "hello" (a b) [3 4])))
                    (equal val (car (read-from-string
                                     (prin1-to-string val)))))"####;
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("t", &o, &n);
}

// ---------------------------------------------------------------------------
// prin1-to-string
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_prin1_to_string_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string "hello")"#);
    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string 'foo)"#);
    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string nil)"#);
    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string t)"#);
}

#[test]
fn oracle_prop_prin1_to_string_complex_structures() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string '(1 2 3))"#);
    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string '(a . b))"#);
    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string [1 2 3])"#);
    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string '(1 "two" three (4 . 5)))"#);
}

#[test]
fn oracle_prop_prin1_to_string_strings_are_quoted() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // prin1-to-string quotes strings (unlike princ)
    let form = r####"(prin1-to-string "hello \"world\"")"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_prin1_to_string_noescape() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // NOESCAPE parameter (2nd arg) — like princ, don't quote
    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string "hello" t)"#);
    assert_oracle_parity_with_bootstrap(r#"(prin1-to-string 'foo t)"#);
}

// ---------------------------------------------------------------------------
// char-equal
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_char_equal_same() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(char-equal ?a ?a)");
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_char_equal_different() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(char-equal ?a ?b)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_char_equal_case_sensitive_by_default() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // By default (case-fold-search is t) char-equal is case-insensitive
    let form = "(let ((case-fold-search t))
                  (char-equal ?A ?a))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_char_equal_case_sensitive_explicit() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((case-fold-search nil))
                  (char-equal ?A ?a))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// message (returns the formatted string)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_message_returns_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // message returns its formatted string
    assert_oracle_parity_with_bootstrap(r#"(message "hello %s, %d" "world" 42)"#);
    assert_oracle_parity_with_bootstrap(r#"(message "plain")"#);
}

// ---------------------------------------------------------------------------
// Complex combination: read-eval-print loop
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_read_print_repl_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Implement a tiny REPL: read forms, eval them, collect results
    let form = r####"(let ((input "(+ 1 2) (* 3 4) (list 'a 'b)")
                        (pos 0)
                        (results nil))
                    (condition-case nil
                        (while t
                          (let ((parsed (read-from-string input pos)))
                            (setq pos (cdr parsed))
                            (setq results
                                  (cons (prin1-to-string
                                         (eval (car parsed)))
                                        results))))
                      (error nil))
                    (nreverse results))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_read_print_serialization_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Serialize and deserialize complex data
    let form = r####"(let ((data '((name . "Alice")
                                 (scores . (95 87 92))
                                 (active . t))))
                    (let ((serialized (prin1-to-string data)))
                      (let ((deserialized (car (read-from-string serialized))))
                        (list (equal data deserialized)
                              (cdr (assq 'name deserialized))
                              (cdr (assq 'scores deserialized))
                              (cdr (assq 'active deserialized))))))"####;
    assert_oracle_parity_with_bootstrap(form);
}
