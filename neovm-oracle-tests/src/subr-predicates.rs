//! Oracle parity tests for subr/function predicates and introspection:
//! `subrp`, `subr-arity`, `commandp`, `interactive-form`,
//! `byte-code-function-p`, `autoloadp`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// subrp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subrp() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (subrp (symbol-function '+))
                        (subrp (symbol-function 'car))
                        (subrp (symbol-function 'cons))
                        (subrp (lambda (x) x))
                        (subrp 42)
                        (subrp nil))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// subr-arity
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_arity() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (subr-arity (symbol-function 'car))
                        (subr-arity (symbol-function 'cons))
                        (subr-arity (symbol-function '+))
                        (subr-arity (symbol-function 'list)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// commandp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_commandp_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
                    ;; Regular lambda is NOT a command
                    (commandp (lambda (x) x))
                    ;; Lambda with interactive IS a command
                    (commandp (lambda () (interactive) 42))
                    ;; Symbols
                    (commandp '+))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// functionp with various types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_functionp_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (functionp (lambda (x) x))
                        (functionp #'car)
                        (functionp '+)
                        (functionp (symbol-function '+))
                        (functionp nil)
                        (functionp t)
                        (functionp 42)
                        (functionp "hello")
                        (functionp '(1 2 3)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: function introspection framework
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_introspect_framework() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Introspect a set of functions and categorize them
    let form = r#"(let ((fns '(+ car cons list length append
                               mapcar format concat))
                        (results nil))
                    (dolist (f fns)
                      (let ((def (symbol-function f)))
                        (let ((arity (when (subrp def)
                                       (subr-arity def))))
                          (setq results
                                (cons (list f
                                            (subrp def)
                                            (when arity (car arity))
                                            (when arity (cdr arity)))
                                      results)))))
                    (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: arity-based dispatch
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_arity_dispatch() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Choose how to call function based on its arity
    let form = r#"(let ((call-with-defaults
                         (lambda (fn args defaults)
                           (let* ((def (if (symbolp fn)
                                           (symbol-function fn)
                                         fn))
                                  (arity (when (subrp def)
                                           (subr-arity def)))
                                  (min-args (if arity (car arity) 0))
                                  (padded args))
                             ;; Pad with defaults if needed
                             (while (< (length padded) min-args)
                               (let ((idx (length padded)))
                                 (setq padded
                                       (append padded
                                               (list (nth idx defaults))))))
                             (apply fn padded)))))
                    (list
                     ;; cons needs exactly 2 args
                     (funcall call-with-defaults
                              'cons '(hello) '(nil))
                     ;; + works with 0 args
                     (funcall call-with-defaults
                              '+ nil nil)
                     ;; concat works with 0 args
                     (funcall call-with-defaults
                              'concat nil nil)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
