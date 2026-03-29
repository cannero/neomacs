//! Oracle parity tests for subr (built-in function) inspection operations:
//! `subrp`, `subr-name`, `subr-arity` (min/max args), `commandp`,
//! `functionp`, `byte-code-function-p`, `compiled-function-p`,
//! `special-form-p`, `closurep`, combined predicates on various callable types.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// subrp: comprehensive predicate on all kinds of objects
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_ops_subrp_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Built-in functions are subrs
  (subrp (symbol-function '+))
  (subrp (symbol-function 'car))
  (subrp (symbol-function 'cdr))
  (subrp (symbol-function 'cons))
  (subrp (symbol-function 'length))
  (subrp (symbol-function 'concat))
  (subrp (symbol-function 'mapcar))
  (subrp (symbol-function 'apply))
  ;; Special forms are also subrs
  (subrp (symbol-function 'if))
  (subrp (symbol-function 'progn))
  (subrp (symbol-function 'let))
  (subrp (symbol-function 'setq))
  (subrp (symbol-function 'quote))
  (subrp (symbol-function 'cond))
  ;; Non-subr objects
  (subrp (lambda (x) x))
  (subrp nil)
  (subrp t)
  (subrp 42)
  (subrp "hello")
  (subrp '(1 2 3))
  (subrp (make-hash-table))
  (subrp [1 2 3]))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// subr-name: extracting the name of built-in functions
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_ops_subr_name() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Names of various built-in functions
  (subr-name (symbol-function '+))
  (subr-name (symbol-function 'car))
  (subr-name (symbol-function 'cdr))
  (subr-name (symbol-function 'cons))
  (subr-name (symbol-function 'list))
  (subr-name (symbol-function 'length))
  (subr-name (symbol-function 'concat))
  (subr-name (symbol-function 'format))
  (subr-name (symbol-function 'eq))
  (subr-name (symbol-function 'equal))
  ;; Special forms have names too
  (subr-name (symbol-function 'if))
  (subr-name (symbol-function 'progn))
  (subr-name (symbol-function 'let))
  ;; Return type is string
  (stringp (subr-name (symbol-function '+)))
  ;; Error on non-subr
  (condition-case err
      (subr-name (lambda (x) x))
    (wrong-type-argument (list 'error (car err)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// subr-arity: min/max args for all parameter patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_ops_subr_arity_all_patterns() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; (1 . 1): exactly 1 arg
  (subr-arity (symbol-function 'car))
  (subr-arity (symbol-function 'cdr))
  (subr-arity (symbol-function 'not))
  ;; (2 . 2): exactly 2 args
  (subr-arity (symbol-function 'cons))
  (subr-arity (symbol-function 'eq))
  (subr-arity (symbol-function 'aref))
  ;; (0 . many): zero or more
  (subr-arity (symbol-function '+))
  (subr-arity (symbol-function 'list))
  (subr-arity (symbol-function 'concat))
  ;; (1 . many): one or more
  (subr-arity (symbol-function 'append))
  ;; (2 . many): two or more
  (subr-arity (symbol-function 'mapcar))
  ;; Optional args: min < max but max is finite
  (subr-arity (symbol-function 'substring))
  (subr-arity (symbol-function 'nth))
  ;; Verify the many symbol
  (let ((ar (subr-arity (symbol-function '+))))
    (list (car ar) (eq (cdr ar) 'many)))
  ;; Error on non-subr
  (condition-case err
      (subr-arity 42)
    (wrong-type-argument (list 'error (car err)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// commandp: interactive command detection
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_ops_commandp_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Lambda without interactive: NOT a command
  (commandp (lambda (x) x))
  ;; Lambda with interactive: IS a command
  (commandp (lambda () (interactive) 42))
  ;; Lambda with interactive and args
  (commandp (lambda (n) (interactive "p") (* n 2)))
  ;; Built-in subrs: some are commands, some are not
  ;; + is not a command
  (commandp '+)
  (commandp 'car)
  ;; nil, t, numbers, strings
  (commandp nil)
  (commandp t)
  (commandp 42)
  (commandp "hello")
  ;; Symbol that is fbound to a lambda with interactive
  (unwind-protect
      (progn
        (fset 'neovm--test-cmd (lambda () (interactive) "a command" t))
        (commandp 'neovm--test-cmd))
    (fmakunbound 'neovm--test-cmd))
  ;; commandp with FOR-CALL-INTERACTIVELY = t (2nd arg)
  (commandp (lambda () (interactive) 42) t)
  ;; Verify commandp implies functionp for lambdas
  (let ((f (lambda () (interactive) 42)))
    (list (commandp f) (functionp f))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// functionp: comprehensive type checks
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_ops_functionp_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Lambda is a function
  (functionp (lambda (x) x))
  (functionp (lambda () nil))
  (functionp (lambda (a b &optional c) (+ a b)))
  (functionp (lambda (&rest args) args))
  ;; Built-in subr function object is a function
  (functionp (symbol-function '+))
  (functionp (symbol-function 'car))
  (functionp (symbol-function 'mapcar))
  ;; Symbol naming a function: functionp returns t
  (functionp '+)
  (functionp 'car)
  ;; Special forms: NOT functions per functionp
  (functionp (symbol-function 'if))
  (functionp (symbol-function 'progn))
  (functionp (symbol-function 'let))
  (functionp (symbol-function 'quote))
  ;; Non-functions
  (functionp nil)
  (functionp t)
  (functionp 42)
  (functionp 3.14)
  (functionp "hello")
  (functionp '(1 2 3))
  (functionp [1 2 3])
  (functionp (make-hash-table))
  ;; Void symbol
  (functionp 'nonexistent-function-xyz-12345))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// special-form-p: detect special forms
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_ops_special_form_p() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; Known special forms
  (special-form-p (symbol-function 'if))
  (special-form-p (symbol-function 'let))
  (special-form-p (symbol-function 'let*))
  (special-form-p (symbol-function 'progn))
  (special-form-p (symbol-function 'setq))
  (special-form-p (symbol-function 'quote))
  (special-form-p (symbol-function 'cond))
  (special-form-p (symbol-function 'while))
  (special-form-p (symbol-function 'or))
  (special-form-p (symbol-function 'and))
  (special-form-p (symbol-function 'unwind-protect))
  (special-form-p (symbol-function 'condition-case))
  (special-form-p (symbol-function 'catch))
  (special-form-p (symbol-function 'defconst))
  (special-form-p (symbol-function 'function))
  ;; NOT special forms: regular built-in functions
  (special-form-p (symbol-function '+))
  (special-form-p (symbol-function 'car))
  (special-form-p (symbol-function 'cons))
  (special-form-p (symbol-function 'list))
  ;; Non-subr objects
  (special-form-p (lambda (x) x))
  (special-form-p nil)
  (special-form-p 42))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// closurep: detect closure objects
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_ops_closurep() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Note: in lexical binding context, lambda creates closures
    let form = r#"(list
  ;; Lambda in default lexical binding creates a closure
  (closurep (lambda (x) x))
  (closurep (lambda () nil))
  (closurep (lambda (a b) (+ a b)))
  (closurep (lambda (&rest args) args))
  ;; Closure capturing a lexical variable
  (let ((x 10))
    (closurep (lambda () x)))
  ;; Built-in subrs are NOT closures
  (closurep (symbol-function '+))
  (closurep (symbol-function 'car))
  ;; Special forms are NOT closures
  (closurep (symbol-function 'if))
  ;; Non-function types
  (closurep nil)
  (closurep t)
  (closurep 42)
  (closurep "hello")
  (closurep '(1 2 3))
  ;; Nested closure
  (let ((outer 1))
    (let ((inner 2))
      (closurep (lambda () (+ outer inner))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Combined predicate matrix: classify callables
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_ops_predicate_matrix() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (fset 'neovm--classify-callable
    (lambda (obj)
      "Classify a callable object using all available predicates."
      (list
        (list 'functionp (functionp obj))
        (list 'subrp (subrp obj))
        (list 'special-form-p (special-form-p obj))
        (list 'closurep (closurep obj))
        (list 'commandp (commandp obj)))))

  (unwind-protect
      (list
        ;; Regular lambda / closure
        (funcall 'neovm--classify-callable (lambda (x) x))
        ;; Built-in function (subr)
        (funcall 'neovm--classify-callable (symbol-function '+))
        (funcall 'neovm--classify-callable (symbol-function 'car))
        ;; Special form
        (funcall 'neovm--classify-callable (symbol-function 'if))
        (funcall 'neovm--classify-callable (symbol-function 'let))
        ;; Interactive command
        (funcall 'neovm--classify-callable (lambda () (interactive) nil))
        ;; nil
        (funcall 'neovm--classify-callable nil)
        ;; number
        (funcall 'neovm--classify-callable 42))
    (fmakunbound 'neovm--classify-callable)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Introspection pipeline: enumerate and inspect built-ins
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_ops_introspection_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (fset 'neovm--subr-info
    (lambda (sym)
      "Collect detailed info about a symbol's function binding."
      (let* ((def (and (fboundp sym) (symbol-function sym)))
             (is-subr (and def (subrp def)))
             (name (and is-subr (subr-name def)))
             (arity (and is-subr (subr-arity def)))
             (is-special (and def (special-form-p def)))
             (is-func (functionp sym)))
        (list sym
              (list 'bound (fboundp sym))
              (list 'subr is-subr)
              (list 'name name)
              (list 'min-args (and arity (car arity)))
              (list 'max-args (and arity (cdr arity)))
              (list 'special is-special)
              (list 'functionp is-func)))))

  (unwind-protect
      (let ((syms '(+ - * / = < > <= >=
                    car cdr cons list length nth
                    concat substring format
                    if let progn setq quote cond while
                    not null eq equal
                    apply funcall mapcar))
            (results nil))
        (dolist (s syms)
          (setq results (cons (funcall 'neovm--subr-info s) results)))
        ;; Return sorted by symbol name for stable comparison
        (nreverse results))
    (fmakunbound 'neovm--subr-info)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Arity-based function dispatch and validation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_subr_ops_arity_validation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (fset 'neovm--check-arity
    (lambda (fn nargs)
      "Check if FN can be called with NARGS arguments.
       Returns (ok min max) or (too-few min max) or (too-many min max)."
      (let* ((def (if (symbolp fn) (symbol-function fn) fn))
             (arity (and (subrp def) (subr-arity def))))
        (if (null arity)
            (list 'unknown nil nil)
          (let ((min-a (car arity))
                (max-a (cdr arity)))
            (cond
              ((< nargs min-a)
               (list 'too-few min-a max-a))
              ((and (not (eq max-a 'many)) (> nargs max-a))
               (list 'too-many min-a max-a))
              (t
               (list 'ok min-a max-a))))))))

  (unwind-protect
      (list
        ;; car: exactly 1 arg
        (funcall 'neovm--check-arity 'car 0)
        (funcall 'neovm--check-arity 'car 1)
        (funcall 'neovm--check-arity 'car 2)
        ;; cons: exactly 2 args
        (funcall 'neovm--check-arity 'cons 0)
        (funcall 'neovm--check-arity 'cons 1)
        (funcall 'neovm--check-arity 'cons 2)
        (funcall 'neovm--check-arity 'cons 3)
        ;; +: 0 or more
        (funcall 'neovm--check-arity '+ 0)
        (funcall 'neovm--check-arity '+ 1)
        (funcall 'neovm--check-arity '+ 100)
        ;; substring: (1 . 3) — 1 to 3 args
        (funcall 'neovm--check-arity 'substring 0)
        (funcall 'neovm--check-arity 'substring 1)
        (funcall 'neovm--check-arity 'substring 2)
        (funcall 'neovm--check-arity 'substring 3)
        ;; lambda: unknown arity
        (funcall 'neovm--check-arity (lambda (x) x) 1))
    (fmakunbound 'neovm--check-arity)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
