//! Oracle parity tests for `caar`, `cadr`, `cdar`, `cddr`, `cdr-safe`,
//! and deeper car/cdr combinations.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{
    assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
    eval_oracle_and_neovm_with_bootstrap,
};

// ---------------------------------------------------------------------------
// 2-level: caar, cadr, cdar, cddr
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_caar_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(caar '((a b) (c d)))");
    assert_oracle_parity_with_bootstrap("(caar '((1 . 2) . (3 . 4)))");
    assert_oracle_parity_with_bootstrap("(caar '((nil)))");
}

#[test]
fn oracle_prop_cadr_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(cadr '(a b c))");
    assert_oracle_parity_with_bootstrap("(cadr '(1 2))");
    assert_oracle_parity_with_bootstrap("(cadr '(x))");
}

#[test]
fn oracle_prop_cdar_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(cdar '((a b c) d))");
    assert_oracle_parity_with_bootstrap("(cdar '((1 . 2) 3))");
}

#[test]
fn oracle_prop_cddr_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(cddr '(a b c d))");
    assert_oracle_parity_with_bootstrap("(cddr '(1 2))");
    assert_oracle_parity_with_bootstrap("(cddr '(a b . c))");
}

// ---------------------------------------------------------------------------
// cdr-safe
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cdr_safe() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(cdr-safe '(1 2 3))");
    assert_oracle_parity_with_bootstrap("(cdr-safe nil)");
    assert_oracle_parity_with_bootstrap("(cdr-safe 42)");
    assert_oracle_parity_with_bootstrap(r#"(cdr-safe "hello")"#);
    assert_oracle_parity_with_bootstrap("(cdr-safe '(a . b))");
}

// ---------------------------------------------------------------------------
// 3-level combinations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_caaar() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(caaar '(((deep) mid) top))");
}

#[test]
fn oracle_prop_caadr() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(caadr '(first (second-car rest) third))");
}

#[test]
fn oracle_prop_caddr() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // caddr = third element
    assert_oracle_parity_with_bootstrap("(caddr '(a b c d e))");
    assert_oracle_parity_with_bootstrap("(caddr '(1 2 3))");
}

#[test]
fn oracle_prop_cadddr() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // cadddr = fourth element
    assert_oracle_parity_with_bootstrap("(cadddr '(a b c d e))");
}

// ---------------------------------------------------------------------------
// Complex: destructuring with car/cdr combos
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_destructure_alist() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use car/cdr combos to destructure association list entries
    let form = "(let ((entries '((name . \"Alice\")
                                  (age . 30)
                                  (role . engineer))))
                  (list (caar entries)
                        (cdar entries)
                        (caadr entries)
                        (cdadr entries)
                        (car (caddr entries))
                        (cdr (caddr entries))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_car_cdr_tree_navigation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Navigate a tree structure using car/cdr combos
    let form = "(let ((tree '((a (b c)) (d (e f)) (g (h i)))))
                  (list
                    ;; First subtree
                    (caar tree)
                    (caadar tree)
                    (car (cdadar tree))
                    ;; Second subtree
                    (caadr tree)
                    ;; Third subtree
                    (caaddr tree)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_car_cdr_safe_chain() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // car-safe/cdr-safe for safe navigation of potentially non-list values
    let form = "(let ((data '((a 1) nil (c 3))))
                  (list (car-safe (car data))
                        (car-safe (cadr data))
                        (cdr-safe (car data))
                        (cdr-safe (cadr data))
                        (car-safe (caddr data))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_car_cdr_build_and_navigate() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a complex structure then navigate it
    let form = "(let ((s (cons (cons (cons 'deep nil)
                                    (cons 'mid nil))
                              (cons (cons 'right nil)
                                    'end))))
                  (list (caaar s)
                        (cdaar s)
                        (caadr s)
                        (cddr s)
                        (cdar s)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_nth_via_car_cdr() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Verify nth equivalence with car/cdr chains
    let form = "(let ((lst '(a b c d e f)))
                  (list (eq (nth 0 lst) (car lst))
                        (eq (nth 1 lst) (cadr lst))
                        (eq (nth 2 lst) (caddr lst))
                        (eq (nth 3 lst) (cadddr lst))
                        (equal (nthcdr 2 lst) (cddr lst))
                        (equal (nthcdr 3 lst) (cdddr lst))))";
    let (o, n) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_ok_eq("(t t t t t t)", &o, &n);
}

#[test]
fn oracle_prop_car_cdr_lambda_list_parsing() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simulate parsing a lambda-list: (name args body...)
    let form = "(let ((defn '(my-func (x y &optional z)
                               (+ x y (or z 0)))))
                  (let ((name (car defn))
                        (args (cadr defn))
                        (body (cddr defn))
                        (required-args
                         (let ((result nil)
                               (remaining (cadr defn)))
                           (while (and remaining
                                       (not (eq (car remaining)
                                                '&optional)))
                             (setq result (cons (car remaining) result)
                                   remaining (cdr remaining)))
                           (nreverse result)))
                        (optional-args
                         (let ((found nil)
                               (remaining (cadr defn)))
                           (while (and remaining
                                       (not (eq (car remaining)
                                                '&optional)))
                             (setq remaining (cdr remaining)))
                           (when remaining
                             (setq found (cdr remaining)))
                           found)))
                    (list name required-args optional-args
                          (length body))))";
    assert_oracle_parity_with_bootstrap(form);
}
