//! Oracle parity tests for advanced `progn`, `prog1`, `prog2` patterns:
//! side effects and return values, nested progn in let/if/cond,
//! return value differences between prog1/prog2/progn,
//! error handling with condition-case wrapping progn,
//! state building through sequential mutations,
//! and progn in macro expansion context.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// progn with side effects and return value
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_progn_side_effects_and_return() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((trace nil)
                        (counter 0))
  ;; progn evaluates all forms for side effects, returns the last
  (let ((result
         (progn
           (setq counter (1+ counter))
           (setq trace (cons (list 'step1 counter) trace))
           (setq counter (* counter 10))
           (setq trace (cons (list 'step2 counter) trace))
           (setq counter (+ counter 7))
           (setq trace (cons (list 'step3 counter) trace))
           ;; Return value is this last expression
           (format "final=%d" counter))))
    (list result counter (nreverse trace))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Nested progn in let, if, and cond
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_progn_nested_in_let_if_cond() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((log nil))
  ;; progn inside let binding
  (let ((a (progn
             (setq log (cons 'computing-a log))
             (* 6 7)))
        (b (progn
             (setq log (cons 'computing-b log))
             (+ 10 3))))
    ;; progn inside if
    (let ((r1 (if (> a 40)
                  (progn
                    (setq log (cons 'a-is-big log))
                    (- a b))
                (progn
                  (setq log (cons 'a-is-small log))
                  (+ a b)))))
      ;; progn inside cond
      (let ((r2 (cond
                  ((< r1 0)
                   (setq log (cons 'negative log))
                   'neg)
                  ((< r1 20)
                   (setq log (cons 'small log))
                   'small)
                  ((< r1 50)
                   (setq log (cons 'medium log))
                   'medium)
                  (t
                   (setq log (cons 'large log))
                   'large))))
        (list a b r1 r2 (nreverse log))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// prog1 vs prog2 vs progn return value differences
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_progn_prog1_prog2_comparison() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((effects nil))
  ;; All three evaluate all forms for side effects;
  ;; they differ only in which value they return
  (let ((r-progn
         (progn
           (setq effects (cons 'progn-1 effects))
           (setq effects (cons 'progn-2 effects))
           (setq effects (cons 'progn-3 effects))
           'progn-last))
        (r-prog1
         (prog1
             (progn
               (setq effects (cons 'prog1-first effects))
               'prog1-val)
           (setq effects (cons 'prog1-2 effects))
           (setq effects (cons 'prog1-3 effects))))
        (r-prog2
         (prog2
             (setq effects (cons 'prog2-1 effects))
             (progn
               (setq effects (cons 'prog2-second effects))
               'prog2-val)
           (setq effects (cons 'prog2-3 effects)))))
    (list r-progn r-prog1 r-prog2
          (nreverse effects))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// progn with error handling: condition-case wrapping progn
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_progn_with_condition_case() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((log nil))
  ;; condition-case around a progn that errors partway through
  (let ((result
         (condition-case err
             (progn
               (setq log (cons 'step1 log))
               (setq log (cons 'step2 log))
               ;; This will signal an error
               (let ((x (/ 1 0)))
                 ;; This should never be reached
                 (setq log (cons 'step3-unreachable log))
                 x))
           (arith-error
            (setq log (cons 'caught-error log))
            (list 'error-caught (car err))))))
    ;; step1 and step2 should have been logged, step3 should not
    (list result (nreverse log))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: build state through sequential mutations in progn
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_progn_build_state_pipeline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((state (make-hash-table :test 'equal)))
  ;; Build up a state machine through sequential progn mutations
  (progn
    ;; Phase 1: initialize
    (puthash "status" "init" state)
    (puthash "items" nil state)
    (puthash "count" 0 state)
    ;; Phase 2: add items
    (let ((items-to-add '("alpha" "beta" "gamma" "delta")))
      (dolist (item items-to-add)
        (progn
          (puthash "items"
                   (append (gethash "items" state) (list item))
                   state)
          (puthash "count"
                   (1+ (gethash "count" state))
                   state))))
    ;; Phase 3: transform - uppercase all items
    (puthash "items"
             (mapcar #'upcase (gethash "items" state))
             state)
    (puthash "status" "transformed" state)
    ;; Phase 4: filter - keep only items with length > 4
    (puthash "items"
             (let ((result nil))
               (dolist (item (gethash "items" state))
                 (when (> (length item) 4)
                   (setq result (cons item result))))
               (nreverse result))
             state)
    (puthash "count" (length (gethash "items" state)) state)
    (puthash "status" "filtered" state))
  ;; Return final state
  (list (gethash "status" state)
        (gethash "count" state)
        (gethash "items" state)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// progn in macro expansion context
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_progn_in_macro_expansion() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  ;; Define a macro that expands to a progn with logging
  (defmacro neovm--test-with-trace (name &rest body)
    (list 'let (list (list 'neovm--trace nil))
          (list 'setq 'neovm--trace
                (list 'cons (list 'quote name) 'neovm--trace))
          (cons 'prog1
                (append body
                        (list (list 'setq 'neovm--trace
                                    (list 'cons 'result 'neovm--trace)))))))
  (unwind-protect
      ;; Use the macro in nested contexts
      (let ((result
             (neovm--test-with-trace outer
               (let ((x 10))
                 (neovm--test-with-trace inner
                   (setq x (* x 3))
                   (+ x 5))))))
        (list result neovm--trace))
    (fmakunbound 'neovm--test-with-trace)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// progn as implicit body: while, unwind-protect, when, unless
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_progn_implicit_body_forms() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((trace nil)
                        (i 0)
                        (sum 0))
  ;; while has an implicit progn body
  (while (< i 5)
    (setq sum (+ sum i))
    (setq trace (cons (list 'while-iter i sum) trace))
    (setq i (1+ i)))
  ;; when has an implicit progn body
  (when (= sum 10)
    (setq trace (cons 'sum-is-10 trace))
    (setq trace (cons (format "verified: %d" sum) trace)))
  ;; unless has an implicit progn body
  (unless (= sum 999)
    (setq trace (cons 'not-999 trace))
    (setq sum (* sum 2)))
  ;; unwind-protect cleanup has an implicit progn body
  (let ((resource nil))
    (unwind-protect
        (progn
          (setq resource 'acquired)
          (setq trace (cons (list 'resource resource) trace))
          sum)
      ;; cleanup forms = implicit progn
      (setq trace (cons 'cleanup-1 trace))
      (setq resource 'released)
      (setq trace (cons (list 'cleanup-resource resource) trace))))
  (list sum (nreverse trace)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// progn return value in deeply nested expression position
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_progn_deeply_nested_return() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((log nil))
  ;; progn used as argument to functions, deeply nested
  (let ((result
         (+ (progn
              (setq log (cons 'a log))
              10)
            (* (progn
                 (setq log (cons 'b log))
                 3)
               (progn
                 (setq log (cons 'c log))
                 (if (> 5 2)
                     (progn
                       (setq log (cons 'd log))
                       7)
                   (progn
                     (setq log (cons 'e log))
                     0)))))))
    ;; 10 + (3 * 7) = 31
    (list result (nreverse log))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
