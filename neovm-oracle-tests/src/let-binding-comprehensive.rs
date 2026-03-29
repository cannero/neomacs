//! Comprehensive oracle parity tests for `let` and `let*` binding forms:
//! parallel vs sequential binding, nested mixing, shadowing, complex
//! expressions, `pcase-let`/`pcase-let*`, closure variable capture,
//! tail-position let, `let-alist`, and very deep nesting.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// let: parallel binding semantics
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_let_parallel_binding() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // In `let`, all init forms are evaluated before any binding takes effect
    let form = r#"(let ((x 10))
                     (let ((x 20)
                           (y x))
                       (list x y)))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Parallel: x is still 1 when y is computed
    let form2 = r#"(let ((x 1))
                      (let ((x (+ x 100))
                            (y (* x 2)))
                        (list x y)))"#;
    assert_oracle_parity_with_bootstrap(form2);

    // Multiple interdependent bindings—all see outer scope
    let form3 = r#"(let ((a 5) (b 10))
                      (let ((a (+ a b))
                            (b (- b a))
                            (c (* a b)))
                        (list a b c)))"#;
    assert_oracle_parity_with_bootstrap(form3);

    // Binding to nil by default
    assert_oracle_parity_with_bootstrap("(let ((x)) x)");
    assert_oracle_parity_with_bootstrap("(let (x) x)");
    assert_oracle_parity_with_bootstrap("(let (x y z) (list x y z))");
}

// ---------------------------------------------------------------------------
// let*: sequential binding semantics
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_let_star_sequential_binding() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // In `let*`, each binding can see previously-bound variables
    let form = r#"(let* ((x 10)
                          (y (* x 2))
                          (z (+ x y)))
                     (list x y z))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Contrast with let: same form but sequential
    let form2 = r#"(let ((x 1))
                      (let* ((x (+ x 100))
                             (y (* x 2)))
                        (list x y)))"#;
    assert_oracle_parity_with_bootstrap(form2);

    // Chain of dependent computations
    let form3 = r#"(let* ((a 2)
                           (b (* a a))
                           (c (* b b))
                           (d (* c c))
                           (e (* d d)))
                      (list a b c d e))"#;
    assert_oracle_parity_with_bootstrap(form3);
}

// ---------------------------------------------------------------------------
// Nested let/let* mixing
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_let_nested_mixing() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((x 1))
                     (let* ((y (+ x 10))
                            (z (* y 2)))
                       (let ((x z)
                             (w (+ x y)))
                         (list x w y z))))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Triple nesting with shadowing at each level
    let form2 = r#"(let ((a 1) (b 2))
                      (let* ((a (+ a b))
                             (c (* a 3)))
                        (let ((b c)
                              (d (+ a b)))
                          (let* ((e (+ a b c d)))
                            (list a b c d e)))))"#;
    assert_oracle_parity_with_bootstrap(form2);

    // let inside let* init form
    let form3 = r#"(let* ((x 5)
                           (y (let ((z (* x 3)))
                                (+ z 1)))
                           (w (let* ((p y) (q (* p 2)))
                                (- q x))))
                      (list x y w))"#;
    assert_oracle_parity_with_bootstrap(form3);
}

// ---------------------------------------------------------------------------
// Shadowing outer variables
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_let_shadowing() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Shadow and verify outer value is restored
    let form = r#"(let ((x 'outer))
                     (let ((result-inner
                            (let ((x 'inner))
                              x)))
                       (list x result-inner)))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Multiple levels of shadowing
    let form2 = r#"(let ((n 1))
                      (let ((n (+ n 10)))
                        (let ((n (+ n 100)))
                          (let ((n (+ n 1000)))
                            n))))"#;
    assert_oracle_parity_with_bootstrap(form2);

    // Shadow function-like binding
    let form3 = r#"(progn
                      (defvar neovm--let-shadow-test 'global)
                      (unwind-protect
                          (let ((neovm--let-shadow-test 'local-1))
                            (let ((neovm--let-shadow-test 'local-2))
                              (list neovm--let-shadow-test))
                            )
                        (makunbound 'neovm--let-shadow-test)))"#;
    assert_oracle_parity_with_bootstrap(form3);
}

// ---------------------------------------------------------------------------
// Binding to complex expressions
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_let_complex_expressions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Binding to progn, condition-case, if, mapcar results
    let form = r#"(let ((a (progn 1 2 3))
                         (b (if t 'yes 'no))
                         (c (condition-case nil
                                (/ 10 2)
                              (error 'err)))
                         (d (mapcar '1+ '(1 2 3)))
                         (e (apply '+ '(10 20 30))))
                     (list a b c d e))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Binding to lambda invocation
    let form2 = r#"(let ((result (funcall (lambda (x y) (* x y)) 6 7)))
                      result)"#;
    assert_oracle_parity_with_bootstrap(form2);

    // Binding to recursive computation via named closure
    let form3 = r#"(progn
                      (fset 'neovm--let-test-fact
                        (lambda (n)
                          (let ((acc 1) (i n))
                            (while (> i 1)
                              (setq acc (* acc i))
                              (setq i (1- i)))
                            acc)))
                      (unwind-protect
                          (let ((f5 (funcall 'neovm--let-test-fact 5))
                                (f10 (funcall 'neovm--let-test-fact 10)))
                            (list f5 f10))
                        (fmakunbound 'neovm--let-test-fact)))"#;
    assert_oracle_parity_with_bootstrap(form3);
}

// ---------------------------------------------------------------------------
// pcase-let and pcase-let* (pattern matching destructuring)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_pcase_let_destructuring() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // pcase-let with backquote patterns
    let form = r#"(progn
                     (require 'pcase)
                     (pcase-let ((`(,a ,b ,c) '(1 2 3)))
                       (list a b c)))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Nested destructuring
    let form2 = r#"(progn
                      (require 'pcase)
                      (pcase-let ((`(,x (,y ,z)) '(10 (20 30))))
                        (+ x y z)))"#;
    assert_oracle_parity_with_bootstrap(form2);

    // pcase-let* with sequential patterns
    let form3 = r#"(progn
                      (require 'pcase)
                      (pcase-let* ((`(,a . ,rest) '(1 2 3 4))
                                   (`(,b . ,rest2) rest))
                        (list a b rest2)))"#;
    assert_oracle_parity_with_bootstrap(form3);

    // pcase-let with _ wildcard
    let form4 = r#"(progn
                      (require 'pcase)
                      (pcase-let ((`(,first _ ,third) '(a b c)))
                        (list first third)))"#;
    assert_oracle_parity_with_bootstrap(form4);
}

// ---------------------------------------------------------------------------
// Closure variable capture
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_let_closure_capture() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Closure captures let-bound variable
    let form = r#"(let ((x 10))
                     (let ((f (lambda () x)))
                       (funcall f)))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Multiple closures sharing captured environment
    let form2 = r#"(let ((count 0))
                      (let ((inc (lambda () (setq count (1+ count))))
                            (get (lambda () count)))
                        (funcall inc)
                        (funcall inc)
                        (funcall inc)
                        (funcall get)))"#;
    assert_oracle_parity_with_bootstrap(form2);

    // Closure captures different let levels
    let form3 = r#"(let ((a 1))
                      (let ((b 2))
                        (let ((f (lambda () (+ a b))))
                          (let ((a 100) (b 200))
                            (list (funcall f) a b)))))"#;
    assert_oracle_parity_with_bootstrap(form3);

    // Generate list of closures capturing loop variable
    let form4 = r#"(let ((fns nil))
                      (dotimes (i 5)
                        (let ((captured i))
                          (push (lambda () captured) fns)))
                      (mapcar #'funcall (nreverse fns)))"#;
    assert_oracle_parity_with_bootstrap(form4);
}

// ---------------------------------------------------------------------------
// let in tail position
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_let_tail_position() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // let as last expression in various forms
    let form = r#"(progn
                     (fset 'neovm--let-tail-test
                       (lambda (n)
                         (if (<= n 0)
                             0
                           (let ((result (* n n)))
                             result))))
                     (unwind-protect
                         (list (funcall 'neovm--let-tail-test 5)
                               (funcall 'neovm--let-tail-test 0)
                               (funcall 'neovm--let-tail-test -1))
                       (fmakunbound 'neovm--let-tail-test)))"#;
    assert_oracle_parity_with_bootstrap(form);

    // let in cond clause tail
    let form2 = r#"(let ((x 3))
                      (cond
                       ((= x 1) (let ((r 'one)) r))
                       ((= x 2) (let ((r 'two)) r))
                       ((= x 3) (let ((r 'three)) r))
                       (t (let ((r 'other)) r))))"#;
    assert_oracle_parity_with_bootstrap(form2);
}

// ---------------------------------------------------------------------------
// let-alist (association list destructuring)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_let_alist() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // let-alist binds dotted-pair values from an alist
    let form = r#"(progn
                     (require 'subr-x)
                     (let-alist '((name . "Alice") (age . 30) (active . t))
                       (list .name .age .active)))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Nested let-alist
    let form2 = r#"(progn
                      (require 'subr-x)
                      (let-alist '((x . 10) (y . 20))
                        (let-alist '((x . 100) (z . 300))
                          (list .x .z))))"#;
    assert_oracle_parity_with_bootstrap(form2);

    // let-alist with computation on values
    let form3 = r#"(progn
                      (require 'subr-x)
                      (let-alist '((width . 800) (height . 600))
                        (* .width .height)))"#;
    assert_oracle_parity_with_bootstrap(form3);
}

// ---------------------------------------------------------------------------
// Very deep nesting
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_let_very_deep_nesting() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // 20 levels of nested let, each adding 1
    let form = r#"(let ((v 0))
  (let ((v (1+ v)))
    (let ((v (1+ v)))
      (let ((v (1+ v)))
        (let ((v (1+ v)))
          (let ((v (1+ v)))
            (let ((v (1+ v)))
              (let ((v (1+ v)))
                (let ((v (1+ v)))
                  (let ((v (1+ v)))
                    (let ((v (1+ v)))
                      (let ((v (1+ v)))
                        (let ((v (1+ v)))
                          (let ((v (1+ v)))
                            (let ((v (1+ v)))
                              (let ((v (1+ v)))
                                (let ((v (1+ v)))
                                  (let ((v (1+ v)))
                                    (let ((v (1+ v)))
                                      (let ((v (1+ v)))
                                        v))))))))))))))))))))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Deep let* chain building a list incrementally
    let form2 = r#"(let* ((a '(1))
                           (b (cons 2 a))
                           (c (cons 3 b))
                           (d (cons 4 c))
                           (e (cons 5 d))
                           (f (cons 6 e))
                           (g (cons 7 f))
                           (h (cons 8 g))
                           (i (cons 9 h))
                           (j (cons 10 i)))
                      j)"#;
    assert_oracle_parity_with_bootstrap(form2);

    // Alternating let/let* at depth
    let form3 = r#"(let ((x 1))
                      (let* ((y (+ x 1))
                             (z (+ y 1)))
                        (let ((a (+ z 1))
                              (b (+ x y z)))
                          (let* ((c (+ a b))
                                 (d (* c 2)))
                            (list x y z a b c d)))))"#;
    assert_oracle_parity_with_bootstrap(form3);
}
