//! Complex oracle tests for advanced control flow patterns combining
//! catch/throw, condition-case, unwind-protect, recursion, closures,
//! and dynamic binding in non-trivial ways.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// Coroutine-like control flow with catch/throw
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cf_coroutine_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simulate pipeline stages with catch/throw for early termination
    let form = r#"(let ((log nil))
                    (let ((pipeline
                           (lambda (input stages)
                             (catch 'pipeline-abort
                               (let ((val input))
                                 (dolist (stage stages)
                                   (setq val (funcall stage val))
                                   (setq log (cons val log)))
                                 val)))))
                      (let ((stages
                             (list
                              (lambda (x)
                                (if (numberp x) (* x 2)
                                  (throw 'pipeline-abort
                                         (list 'error "not a number"))))
                              (lambda (x)
                                (if (> x 100)
                                    (throw 'pipeline-abort
                                           (list 'overflow x))
                                  (+ x 10)))
                              (lambda (x) (* x 3)))))
                        ;; Normal flow
                        (let ((r1 (funcall pipeline 5 stages))
                              (_ (setq log nil))
                              ;; Overflow abort
                              (r2 (funcall pipeline 60 stages))
                              (_ (setq log nil))
                              ;; Type error abort
                              (r3 (funcall pipeline "bad" stages)))
                          (list r1 r2 r3)))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Exception-safe resource management (RAII-like)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cf_raii_resource_manager() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Track resource acquisition/release even through errors
    let form = r#"(let ((resources nil)
                        (acquired nil))
                    (let ((acquire
                           (lambda (name)
                             (setq acquired (cons name acquired))
                             (setq resources (cons name resources))
                             name))
                          (release
                           (lambda (name)
                             (setq resources (delete name resources))
                             name)))
                      ;; Normal case: all resources released
                      (unwind-protect
                          (progn
                            (funcall acquire 'db)
                            (funcall acquire 'file)
                            (funcall acquire 'net)
                            42)
                        (dolist (r (copy-sequence resources))
                          (funcall release r)))
                      (let ((after-normal (copy-sequence resources)))
                        ;; Error case: resources still released
                        (setq acquired nil)
                        (condition-case err
                            (unwind-protect
                                (progn
                                  (funcall acquire 'db2)
                                  (funcall acquire 'file2)
                                  (error "simulated crash")
                                  (funcall acquire 'net2))
                              (dolist (r (copy-sequence resources))
                                (funcall release r)))
                          (error nil))
                        (list after-normal
                              resources
                              (nreverse acquired)))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Recursive descent with dynamic-binding context
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cf_recursive_evaluator() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Mini expression evaluator with let-scoped environment
    let form = r#"(progn
                    (defvar neovm--test-eval-env nil)
                    (fset 'neovm--test-eval-expr
                      (lambda (expr)
                        (cond
                         ((numberp expr) expr)
                         ((symbolp expr)
                          (let ((binding (assq expr neovm--test-eval-env)))
                            (if binding (cdr binding)
                              (error "Unbound: %s" expr))))
                         ((eq (car expr) '+)
                          (+ (funcall 'neovm--test-eval-expr (nth 1 expr))
                             (funcall 'neovm--test-eval-expr (nth 2 expr))))
                         ((eq (car expr) '*)
                          (* (funcall 'neovm--test-eval-expr (nth 1 expr))
                             (funcall 'neovm--test-eval-expr (nth 2 expr))))
                         ((eq (car expr) 'let1)
                          ;; (let1 var val body)
                          (let* ((var (nth 1 expr))
                                 (val (funcall 'neovm--test-eval-expr
                                               (nth 2 expr)))
                                 (neovm--test-eval-env
                                  (cons (cons var val)
                                        neovm--test-eval-env)))
                            (funcall 'neovm--test-eval-expr (nth 3 expr))))
                         ((eq (car expr) 'if0)
                          ;; (if0 test then else)
                          (if (= 0 (funcall 'neovm--test-eval-expr
                                            (nth 1 expr)))
                              (funcall 'neovm--test-eval-expr (nth 2 expr))
                            (funcall 'neovm--test-eval-expr (nth 3 expr)))))))
                    (unwind-protect
                        (let ((neovm--test-eval-env nil))
                          (list
                           ;; (let1 x 5 (let1 y 3 (+ x (* y 2))))
                           (funcall 'neovm--test-eval-expr
                                    '(let1 x 5
                                       (let1 y 3
                                         (+ x (* y 2)))))
                           ;; (if0 0 42 99)
                           (funcall 'neovm--test-eval-expr
                                    '(if0 0 42 99))
                           ;; (if0 1 42 99)
                           (funcall 'neovm--test-eval-expr
                                    '(if0 1 42 99))
                           ;; nested: (let1 a 10 (if0 (+ a -10) (+ a 1) a))
                           (funcall 'neovm--test-eval-expr
                                    '(let1 a 10
                                       (if0 (+ a -10)
                                         (+ a 1)
                                         a)))))
                      (fmakunbound 'neovm--test-eval-expr)
                      (makunbound 'neovm--test-eval-env)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Continuation-passing style with closures
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cf_cps_factorial() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // CPS factorial: all calls are in tail position
    let form = r#"(progn
                    (fset 'neovm--test-cps-fact
                      (lambda (n k)
                        (if (= n 0)
                            (funcall k 1)
                          (funcall 'neovm--test-cps-fact
                                   (1- n)
                                   (lambda (result)
                                     (funcall k (* n result)))))))
                    (unwind-protect
                        (list
                         (funcall 'neovm--test-cps-fact 0 #'identity)
                         (funcall 'neovm--test-cps-fact 1 #'identity)
                         (funcall 'neovm--test-cps-fact 5 #'identity)
                         (funcall 'neovm--test-cps-fact 10 #'identity))
                      (fmakunbound 'neovm--test-cps-fact)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_cf_cps_tree_sum() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // CPS tree sum: sum all numbers in a nested list structure
    let form = r#"(progn
                    (fset 'neovm--test-cps-tree-sum
                      (lambda (tree k)
                        (cond
                         ((null tree) (funcall k 0))
                         ((numberp tree) (funcall k tree))
                         ((consp tree)
                          (funcall 'neovm--test-cps-tree-sum
                                   (car tree)
                                   (lambda (left-sum)
                                     (funcall 'neovm--test-cps-tree-sum
                                              (cdr tree)
                                              (lambda (right-sum)
                                                (funcall k (+ left-sum
                                                             right-sum))))))))))
                    (unwind-protect
                        (list
                         (funcall 'neovm--test-cps-tree-sum
                                  '(1 (2 3) (4 (5 6))) #'identity)
                         (funcall 'neovm--test-cps-tree-sum
                                  '(10 20 30) #'identity)
                         (funcall 'neovm--test-cps-tree-sum nil #'identity))
                      (fmakunbound 'neovm--test-cps-tree-sum)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: backtracking solver with catch/throw
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cf_backtracking_solver() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simple constraint solver with backtracking
    // Find x, y in 1..5 such that x + y = 7 and x < y
    let form = r#"(catch 'found
                    (let ((x 1))
                      (while (<= x 5)
                        (let ((y 1))
                          (while (<= y 5)
                            (when (and (= (+ x y) 7)
                                       (< x y))
                              (throw 'found (list x y)))
                            (setq y (1+ y))))
                        (setq x (1+ x))))
                    'no-solution)"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_cf_backtracking_all_solutions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Find ALL pairs (x, y) in 1..6 where x*y = 12
    let form = r#"(let ((solutions nil))
                    (let ((x 1))
                      (while (<= x 6)
                        (let ((y 1))
                          (while (<= y 6)
                            (when (= (* x y) 12)
                              (setq solutions
                                    (cons (list x y) solutions)))
                            (setq y (1+ y))))
                        (setq x (1+ x))))
                    (nreverse solutions))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: state machine with dynamic binding
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cf_state_machine_lexer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Lexer state machine for simple number/identifier tokens
    let form = r#"(with-temp-buffer
                    (insert "foo 123 bar45 678baz")
                    (goto-char (point-min))
                    (let ((tokens nil)
                          (state 'start))
                      (while (< (point) (point-max))
                        (let ((c (char-after (point))))
                          (cond
                           ;; Start state
                           ((eq state 'start)
                            (cond
                             ((and (>= c ?a) (<= c ?z))
                              (setq state 'ident))
                             ((and (>= c ?0) (<= c ?9))
                              (setq state 'number))
                             ((= c ?\ )
                              (forward-char 1))
                             (t (forward-char 1))))
                           ;; Identifier state
                           ((eq state 'ident)
                            (let ((start (point)))
                              (skip-chars-forward "a-zA-Z0-9_")
                              (setq tokens
                                    (cons (cons 'id
                                                (buffer-substring
                                                 start (point)))
                                          tokens)
                                    state 'start)))
                           ;; Number state
                           ((eq state 'number)
                            (let ((start (point)))
                              (skip-chars-forward "0-9")
                              (setq tokens
                                    (cons (cons 'num
                                                (string-to-number
                                                 (buffer-substring
                                                  start (point))))
                                          tokens)
                                    state 'start)))))
                        )
                      (nreverse tokens)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: trampoline pattern for tail-call optimization
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cf_trampoline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Trampoline: bounce thunks until we get a non-function result
    let form = r#"(progn
                    (fset 'neovm--test-trampoline
                      (lambda (thunk)
                        (let ((result thunk))
                          (while (functionp result)
                            (setq result (funcall result)))
                          result)))
                    ;; Even/odd via mutual trampolining
                    (fset 'neovm--test-even-t
                      (lambda (n)
                        (if (= n 0) t
                          (lambda ()
                            (funcall 'neovm--test-odd-t (1- n))))))
                    (fset 'neovm--test-odd-t
                      (lambda (n)
                        (if (= n 0) nil
                          (lambda ()
                            (funcall 'neovm--test-even-t (1- n))))))
                    (unwind-protect
                        (list
                         (funcall 'neovm--test-trampoline
                                  (funcall 'neovm--test-even-t 0))
                         (funcall 'neovm--test-trampoline
                                  (funcall 'neovm--test-even-t 1))
                         (funcall 'neovm--test-trampoline
                                  (funcall 'neovm--test-even-t 10))
                         (funcall 'neovm--test-trampoline
                                  (funcall 'neovm--test-odd-t 7))
                         (funcall 'neovm--test-trampoline
                                  (funcall 'neovm--test-odd-t 100)))
                      (fmakunbound 'neovm--test-trampoline)
                      (fmakunbound 'neovm--test-even-t)
                      (fmakunbound 'neovm--test-odd-t)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: nested condition-case with rethrow
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cf_nested_condition_case_rethrow() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Inner handler catches, logs, and rethrows to outer
    let form = r#"(let ((inner-log nil)
                        (outer-log nil))
                    (condition-case outer-err
                        (condition-case inner-err
                            (progn
                              (/ 1 0))
                          (arith-error
                           (setq inner-log
                                 (list 'caught-inner
                                       (car inner-err)))
                           ;; Rethrow as a different error
                           (error "Wrapped: %s"
                                  (error-message-string inner-err))))
                      (error
                       (setq outer-log
                             (list 'caught-outer
                                   (error-message-string outer-err)))))
                    (list inner-log outer-log))"#;
    assert_oracle_parity(form);
}
