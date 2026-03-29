//! Oracle parity tests for `safe-length`: proper lists, circular lists,
//! dotted/improper lists, comparison with `length`, and complex structural
//! variations.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// safe-length on proper lists of various sizes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_proper_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (safe-length nil)
                        (safe-length '(a))
                        (safe-length '(a b))
                        (safe-length '(a b c d e f g h i j))
                        (safe-length (make-list 100 'x)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// safe-length on circular list must return a number, not hang
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_circular_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a circular list of length 3 and verify safe-length returns
    // a finite integer (the exact value is implementation-defined but
    // must be an integer, and must match the oracle).
    let form = r#"(let ((lst (list 1 2 3)))
                    (setcdr (last lst) lst)
                    (integerp (safe-length lst)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_safe_length_circular_single() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Single-element circular list
    let form = r#"(let ((cell (cons 'x nil)))
                    (setcdr cell cell)
                    (integerp (safe-length cell)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// safe-length on dotted pair / improper lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_dotted_pair() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (safe-length '(a . b))
                        (safe-length '(a b . c))
                        (safe-length '(1 2 3 4 . 5))
                        (safe-length (cons 'only 42)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// safe-length vs length on normal lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_vs_length() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // For proper lists, safe-length and length must agree
    let form = r#"(let ((lists (list nil
                                     '(a)
                                     '(1 2 3)
                                     (make-list 50 t)
                                     '(x y z w))))
                    (mapcar (lambda (lst)
                              (list (length lst) (safe-length lst)
                                    (= (length lst) (safe-length lst))))
                            lists))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// safe-length on non-list types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_non_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (safe-length 42)
                        (safe-length "hello")
                        (safe-length 'sym)
                        (safe-length [1 2 3])
                        (safe-length t))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: build lists of varying structures and classify by safe-length
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_structural_classification() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a variety of structures and use safe-length to classify them
    // into proper, dotted, and non-list categories.
    let form = r#"(let ((structures
                         (list nil
                               '(a b c)
                               '(1 . 2)
                               '(x y . z)
                               42
                               "string"
                               '(p q r s t)
                               (cons 'head (cons 'mid 'tail))
                               '(single))))
                    (let ((proper nil)
                          (dotted nil)
                          (non-list nil))
                      (dolist (s structures)
                        (let ((sl (safe-length s)))
                          (cond
                           ((not (consp s))
                            (setq non-list
                                  (cons (list s sl) non-list)))
                           ((proper-list-p s)
                            (setq proper
                                  (cons (list s sl) proper)))
                           (t
                            (setq dotted
                                  (cons (list s sl) dotted))))))
                      (list (nreverse proper)
                            (nreverse dotted)
                            (nreverse non-list))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: safe-length as a guard in recursive flattening
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_guarded_flatten() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use safe-length to guard against improper/circular structures
    // during a flatten operation that only descends into proper lists.
    let form = r#"(let ((safe-flatten nil))
                    (setq safe-flatten
                          (lambda (tree)
                            (cond
                             ((null tree) nil)
                             ((not (consp tree)) (list tree))
                             ;; Only descend if it's a proper list
                             ((proper-list-p tree)
                              (let ((result nil))
                                (dolist (elt tree)
                                  (setq result
                                        (append result
                                                (funcall safe-flatten elt))))
                                result))
                             ;; Dotted pair: treat as two atoms
                             (t (list (car tree) (cdr tree))))))
                    (list
                     (funcall safe-flatten '(1 (2 (3 4) 5) 6))
                     (funcall safe-flatten '(a (b . c) (d (e . f))))
                     (funcall safe-flatten '(1 2 3))
                     (funcall safe-flatten nil)
                     (funcall safe-flatten '(x . y))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: safe-length with accumulator building progressively longer lists
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_safe_length_progressive_build() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((results nil)
                        (lst nil))
                    (dotimes (i 10)
                      (setq lst (cons i lst))
                      (setq results
                            (cons (list i (safe-length lst)
                                        (= (safe-length lst) (1+ i)))
                                  results)))
                    ;; Also test that safe-length on the reversed list matches
                    (let ((rev (reverse lst)))
                      (cons (list 'reversed (safe-length rev)
                                  (= (safe-length rev) (safe-length lst)))
                            (nreverse results))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
