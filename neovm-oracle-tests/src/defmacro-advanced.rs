//! Oracle parity tests for advanced `defmacro` patterns:
//! macro with body, &rest, backquote expansion, nested macros,
//! macroexpand-all, and common macro idioms.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{
    assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
    eval_oracle_and_neovm_with_bootstrap,
};

// ---------------------------------------------------------------------------
// Macro with backquote and splicing
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_defmacro_when_unless() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Define and use when/unless style macros
    let form = "(progn
                  (defmacro neovm--test-my-when (cond &rest body)
                    `(if ,cond (progn ,@body)))
                  (defmacro neovm--test-my-unless (cond &rest body)
                    `(if ,cond nil (progn ,@body)))
                  (unwind-protect
                      (list
                        (neovm--test-my-when t 1 2 3)
                        (neovm--test-my-when nil 1 2 3)
                        (neovm--test-my-unless nil 4 5 6)
                        (neovm--test-my-unless t 4 5 6))
                    (fmakunbound 'neovm--test-my-when)
                    (fmakunbound 'neovm--test-my-unless)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_defmacro_with_let() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Macro that introduces let binding
    let form = "(progn
                  (defmacro neovm--test-with-val (var expr &rest body)
                    `(let ((,var ,expr))
                       ,@body))
                  (unwind-protect
                      (neovm--test-with-val x (+ 1 2)
                        (* x x))
                    (fmakunbound 'neovm--test-with-val)))";
    let (o, n) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_ok_eq("9", &o, &n);
}

// ---------------------------------------------------------------------------
// macroexpand / macroexpand-1
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_macroexpand_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (defmacro neovm--test-double (x)
                    `(+ ,x ,x))
                  (unwind-protect
                      (list
                        (macroexpand '(neovm--test-double 5))
                        (eval (macroexpand '(neovm--test-double 5))))
                    (fmakunbound 'neovm--test-double)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_macroexpand_nested() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (defmacro neovm--test-inc (var)
                    `(setq ,var (1+ ,var)))
                  (defmacro neovm--test-inc-twice (var)
                    `(progn (neovm--test-inc ,var)
                            (neovm--test-inc ,var)))
                  (unwind-protect
                      (let ((counter 0))
                        (neovm--test-inc-twice counter)
                        counter)
                    (fmakunbound 'neovm--test-inc)
                    (fmakunbound 'neovm--test-inc-twice)))";
    let (o, n) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_ok_eq("2", &o, &n);
}

// ---------------------------------------------------------------------------
// Macro for iteration patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_defmacro_while_collect() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (defmacro neovm--test-collect-while (var init test step)
                    `(let ((,var ,init)
                           (neovm--collect-result nil))
                       (while ,test
                         (setq neovm--collect-result
                               (cons ,var neovm--collect-result))
                         (setq ,var ,step))
                       (nreverse neovm--collect-result)))
                  (unwind-protect
                      (neovm--test-collect-while i 1 (<= i 10) (* i 2))
                    (fmakunbound 'neovm--test-collect-while)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Macro with list processing
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_defmacro_cond_like() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a chain of if-else from macro args
    let form = "(progn
                  (defmacro neovm--test-match (val &rest clauses)
                    (let ((v (make-symbol \"v\")))
                      `(let ((,v ,val))
                         ,(let ((result nil))
                            (dolist (clause (reverse clauses))
                              (if (eq (car clause) 'otherwise)
                                  (setq result (cadr clause))
                                (setq result
                                      `(if (equal ,v ',(car clause))
                                           ,(cadr clause)
                                         ,result))))
                            result))))
                  (unwind-protect
                      (list
                        (neovm--test-match 'b
                          (a 1) (b 2) (c 3) (otherwise 0))
                        (neovm--test-match 'x
                          (a 1) (b 2) (otherwise 99)))
                    (fmakunbound 'neovm--test-match)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: swap macro and other side-effect macros
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_defmacro_swap() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (defmacro neovm--test-swap (a b)
                    (let ((tmp (make-symbol \"tmp\")))
                      `(let ((,tmp ,a))
                         (setq ,a ,b ,b ,tmp))))
                  (unwind-protect
                      (let ((x 10) (y 20))
                        (neovm--test-swap x y)
                        (list x y))
                    (fmakunbound 'neovm--test-swap)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_defmacro_push_pop() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (defmacro neovm--test-push! (val place)
                    `(setq ,place (cons ,val ,place)))
                  (defmacro neovm--test-pop! (place)
                    (let ((v (make-symbol \"v\")))
                      `(let ((,v (car ,place)))
                         (setq ,place (cdr ,place))
                         ,v)))
                  (unwind-protect
                      (let ((stack nil))
                        (neovm--test-push! 1 stack)
                        (neovm--test-push! 2 stack)
                        (neovm--test-push! 3 stack)
                        (let ((a (neovm--test-pop! stack))
                              (b (neovm--test-pop! stack)))
                          (list a b stack)))
                    (fmakunbound 'neovm--test-push!)
                    (fmakunbound 'neovm--test-pop!)))";
    assert_oracle_parity_with_bootstrap(form);
}
