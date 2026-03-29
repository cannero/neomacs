//! Oracle parity tests for `make-symbol` (uninterned symbols).

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// make-symbol basics
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_make_symbol_creates_symbol() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(symbolp (make-symbol "test"))"#);
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_make_symbol_name() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm(r#"(symbol-name (make-symbol "my-sym"))"#);
    assert_ok_eq(r#""my-sym""#, &o, &n);
}

#[test]
fn oracle_prop_make_symbol_not_interned() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // make-symbol creates uninterned symbols - not eq to interned ones
    let form = r####"(let ((s (make-symbol "hello")))
                    (list (symbolp s)
                          (eq s 'hello)
                          (equal (symbol-name s) "hello")))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_make_symbol_each_unique() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Two calls with same name produce different symbols
    let form = r####"(let ((a (make-symbol "test"))
                        (b (make-symbol "test")))
                    (list (eq a b)
                          (equal (symbol-name a) (symbol-name b))))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_make_symbol_set_value() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Can set value on uninterned symbol
    let form = r####"(let ((s (make-symbol "counter")))
                    (set s 0)
                    (set s (1+ (symbol-value s)))
                    (set s (1+ (symbol-value s)))
                    (symbol-value s))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_make_symbol_plist() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Uninterned symbols can have plists
    let form = r####"(let ((s (make-symbol "tagged")))
                    (put s 'type 'integer)
                    (put s 'range '(0 100))
                    (list (get s 'type)
                          (get s 'range)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: gensym-like pattern with make-symbol
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_make_symbol_gensym_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Implement gensym-like counter with uninterned symbols
    let form = r####"(let ((counter 0))
                    (let ((gen (lambda (prefix)
                                 (setq counter (1+ counter))
                                 (make-symbol
                                  (concat prefix
                                          (number-to-string counter))))))
                      (let ((s1 (funcall gen "g"))
                            (s2 (funcall gen "g"))
                            (s3 (funcall gen "tmp")))
                        (list (symbol-name s1)
                              (symbol-name s2)
                              (symbol-name s3)
                              (eq s1 s2)))))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_make_symbol_as_unique_key() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use uninterned symbols as unique keys in an alist
    let form = r####"(let ((k1 (make-symbol "key"))
                        (k2 (make-symbol "key"))
                        (k3 (make-symbol "key")))
                    (let ((table (list (cons k1 "first")
                                       (cons k2 "second")
                                       (cons k3 "third"))))
                      ;; assq finds by identity (eq), not name
                      (list (cdr (assq k1 table))
                            (cdr (assq k2 table))
                            (cdr (assq k3 table))
                            ;; Interned 'key won't match any
                            (assq 'key table))))"####;
    assert_oracle_parity_with_bootstrap(form);
}
