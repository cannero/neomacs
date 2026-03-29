//! Advanced oracle parity tests for apply/funcall combinations:
//! varying trailing args, nested function indirection, lambda directly
//! in funcall, generic dispatch, middleware chains, and higher-order
//! composition patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// apply with varying number of trailing args before final list
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_varying_trailing_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // 0 trailing args
    assert_oracle_parity_with_bootstrap("(apply #'+ '(1 2 3))");
    // 1 trailing arg
    assert_oracle_parity_with_bootstrap("(apply #'+ 10 '(1 2 3))");
    // 2 trailing args
    assert_oracle_parity_with_bootstrap("(apply #'+ 10 20 '(1 2 3))");
    // 3 trailing args
    assert_oracle_parity_with_bootstrap("(apply #'+ 10 20 30 '(1 2 3))");
    // 4 trailing args
    assert_oracle_parity_with_bootstrap("(apply #'list 'a 'b 'c 'd '(e f))");
    // trailing args with nil final list
    assert_oracle_parity_with_bootstrap("(apply #'list 'a 'b 'c '())");
    // All args via trailing, empty final list
    assert_oracle_parity_with_bootstrap("(apply #'+ 1 2 3 4 5 '())");
}

// ---------------------------------------------------------------------------
// apply with nested function calls as the function argument
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_nested_function_arg() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // symbol-function indirection
    let form = r#"(progn
                    (fset 'neovm--test-afa-add #'+)
                    (unwind-protect
                        (apply (symbol-function 'neovm--test-afa-add) '(1 2 3))
                      (fmakunbound 'neovm--test-afa-add)))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Selecting function from alist
    let form = r#"(let ((ops '((add . +) (mul . *) (cat . concat))))
                    (list
                     (apply (cdr (assq 'add ops)) '(10 20 30))
                     (apply (cdr (assq 'mul ops)) '(2 3 4))
                     (apply (cdr (assq 'cat ops)) '("a" "b" "c"))))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Function returned from a closure
    let form = r#"(let ((make-adder (lambda (n) (lambda (&rest args) (apply #'+ n args)))))
                    (let ((add10 (funcall make-adder 10)))
                      (list (funcall add10 1 2 3)
                            (funcall add10)
                            (apply add10 '(5 5 5)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// funcall with lambda expressions directly
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_direct_lambda() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simple direct lambda
    assert_oracle_parity_with_bootstrap("(funcall (lambda (x y) (+ (* x x) (* y y))) 3 4)");

    // Lambda with &optional
    assert_oracle_parity_with_bootstrap(
        "(list (funcall (lambda (a &optional b) (list a b)) 1)
               (funcall (lambda (a &optional b) (list a b)) 1 2))",
    );

    // Lambda with &rest
    assert_oracle_parity_with_bootstrap(
        "(funcall (lambda (head &rest tail) (cons head (length tail))) 'a 'b 'c 'd)",
    );

    // Nested lambda application
    assert_oracle_parity_with_bootstrap(
        "(funcall (funcall (lambda (x) (lambda (y) (+ x y))) 10) 20)",
    );

    // Lambda with destructuring via let inside
    let form = r#"(funcall (lambda (pair)
                              (let ((a (car pair)) (b (cdr pair)))
                                (* a b)))
                            '(6 . 7))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// funcall with symbol-function indirection chains
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_symbol_function_chain() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Chain of fset aliases
    let form = r#"(progn
                    (fset 'neovm--test-afa-f1 (lambda (x) (* x 2)))
                    (fset 'neovm--test-afa-f2 'neovm--test-afa-f1)
                    (unwind-protect
                        (list
                         (funcall 'neovm--test-afa-f1 5)
                         (funcall 'neovm--test-afa-f2 5)
                         (funcall (indirect-function 'neovm--test-afa-f2) 5)
                         (eq (indirect-function 'neovm--test-afa-f2)
                             (symbol-function 'neovm--test-afa-f1)))
                      (fmakunbound 'neovm--test-afa-f1)
                      (fmakunbound 'neovm--test-afa-f2)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// apply + mapcar combination (apply #'append (mapcar ...))
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_mapcar_flatten() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Classic flatten-one-level: (apply #'append (mapcar ...))
    let form = r#"(let ((data '((1 2) (3 4 5) (6) nil (7 8 9 10))))
                    (apply #'append
                           (mapcar (lambda (sub)
                                     (if sub (mapcar #'1+ sub) nil))
                                   data)))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Build format strings and concat via apply
    let form = r#"(let ((parts '(("hello" . "HELLO") ("world" . "WORLD"))))
                    (apply #'concat
                           (mapcar (lambda (p)
                                     (format "%s->%s " (car p) (cdr p)))
                                   parts)))"#;
    assert_oracle_parity_with_bootstrap(form);

    // Transpose a matrix via mapcar + apply
    let form = r#"(let ((matrix '((1 2 3) (4 5 6) (7 8 9))))
                    (apply #'mapcar #'list matrix))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// funcall with &rest parameter functions -- accumulator pattern
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_rest_accumulator() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Variadic function that accumulates into a hash table
    let form = r#"(let ((make-collector
                         (lambda ()
                           (let ((tbl (make-hash-table :test 'equal))
                                 (count 0))
                             (list
                              ;; add: accepts any number of key-value pairs
                              (lambda (&rest kvs)
                                (while kvs
                                  (puthash (car kvs) (cadr kvs) tbl)
                                  (setq count (1+ count))
                                  (setq kvs (cddr kvs)))
                                count)
                              ;; get
                              (lambda (k) (gethash k tbl))
                              ;; count
                              (lambda () count))))))
                    (let* ((col (funcall make-collector))
                           (add (nth 0 col))
                           (get (nth 1 col))
                           (cnt (nth 2 col)))
                      (funcall add "a" 1 "b" 2 "c" 3)
                      (funcall add "d" 4)
                      (list (funcall cnt)
                            (funcall get "a")
                            (funcall get "c")
                            (funcall get "d"))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: generic dispatch using apply (method table pattern)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_apply_method_dispatch() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // A method table: object is a plist, methods in a hash table.
    // Dispatch calls the method with the object + extra args via apply.
    let form = r#"(let ((methods (make-hash-table :test 'eq)))
                    ;; Register methods
                    (puthash 'area
                             (lambda (obj)
                               (let ((kind (plist-get obj :kind)))
                                 (cond
                                  ((eq kind 'rect)
                                   (* (plist-get obj :w) (plist-get obj :h)))
                                  ((eq kind 'circle)
                                   ;; approximate pi * r^2 with integer math
                                   (let ((r (plist-get obj :r)))
                                     (* 314 r r)))
                                  (t 0))))
                             methods)
                    (puthash 'scale
                             (lambda (obj factor)
                               (let ((kind (plist-get obj :kind)))
                                 (cond
                                  ((eq kind 'rect)
                                   (list :kind 'rect
                                         :w (* (plist-get obj :w) factor)
                                         :h (* (plist-get obj :h) factor)))
                                  ((eq kind 'circle)
                                   (list :kind 'circle
                                         :r (* (plist-get obj :r) factor)))
                                  (t obj))))
                             methods)
                    ;; Dispatch function
                    (let ((dispatch
                           (lambda (method-name obj &rest args)
                             (let ((fn (gethash method-name methods)))
                               (if fn
                                   (apply fn obj args)
                                 (error "no method: %s" method-name))))))
                      (let ((rect '(:kind rect :w 3 :h 4))
                            (circ '(:kind circle :r 5)))
                        (list
                         (funcall dispatch 'area rect)
                         (funcall dispatch 'area circ)
                         (funcall dispatch 'scale rect 2)
                         (plist-get (funcall dispatch 'scale circ 3) :r)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: middleware chain using funcall (wrap-and-call pattern)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_funcall_middleware_chain() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Middleware pattern: each middleware wraps the next handler.
    // We build a chain and run a "request" through it, collecting a log.
    let form = r#"(let ((log nil))
                    (let ((make-timing
                           (lambda (next label)
                             (lambda (req)
                               (setq log (cons (list 'enter label) log))
                               (let ((result (funcall next req)))
                                 (setq log (cons (list 'exit label) log))
                                 result))))
                          (make-transform
                           (lambda (next fn)
                             (lambda (req)
                               (funcall next (funcall fn req)))))
                          (make-validator
                           (lambda (next pred)
                             (lambda (req)
                               (if (funcall pred req)
                                   (funcall next req)
                                 (list 'error "validation failed"))))))
                      ;; Build chain: validate -> transform (upcase) -> time -> handler
                      (let* ((handler (lambda (req) (list 'ok (concat "processed:" req))))
                             (chain handler))
                        (setq chain (funcall make-timing chain "core"))
                        (setq chain (funcall make-transform chain #'upcase))
                        (setq chain (funcall make-validator chain
                                             (lambda (s) (> (length s) 0))))
                        (let ((result1 (funcall chain "hello"))
                              (result2 (funcall chain "")))
                          (list result1 result2 (nreverse log))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
