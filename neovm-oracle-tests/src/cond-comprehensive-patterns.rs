//! Oracle parity tests for comprehensive `cond` form patterns.
//!
//! Covers: basic multi-clause, `t` default, no-match returning nil,
//! multiple body forms per clause, single-test return value, nested cond,
//! cond with side effects, complex test expressions, cond in function
//! definitions, and cond vs if dispatch patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// 1. Multi-clause cond with mixed types in tests and bodies
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_mixed_clause_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Test that cond evaluates each test in order, handles string/symbol/list tests
    let form = r#"(let ((results nil))
  (dolist (x '(0 "" nil t 42 "hello" (1 2)))
    (setq results
          (cons
           (cond
            ((null x) 'is-null)
            ((and (integerp x) (= x 0)) 'is-zero)
            ((and (stringp x) (= (length x) 0)) 'empty-string)
            ((integerp x) 'integer)
            ((stringp x) 'string)
            ((consp x) 'list)
            (t 'other))
           results)))
  (nreverse results))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 2. cond with `t` default clause returning complex value
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_t_default_complex() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((categorize
         (lambda (n)
           (cond
            ((< n -100) (list 'very-negative n (abs n)))
            ((< n 0) (list 'negative n))
            ((= n 0) '(zero))
            ((< n 100) (list 'positive n))
            (t (list 'very-positive n (* n n)))))))
  (mapcar categorize '(-500 -42 0 7 999)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 3. cond with no matching clause returns nil
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_no_match_nil() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  ;; All clauses fail
  (cond
   ((eq 'a 'b) 1)
   ((> 3 5) 2)
   ((string= "foo" "bar") 3))
  ;; Empty cond
  (cond)
  ;; Single failing clause
  (cond (nil 'never))
  ;; Multiple nil tests
  (cond (nil) (nil) (nil))
  ;; Even with side-effect-free complex tests that are nil
  (cond
   ((car nil) 'a)
   ((cdr nil) 'b)
   ((nth 5 '(1 2)) 'c)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 4. cond clause with multiple body forms (each evaluated, last returned)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_multiple_body_forms() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((trace nil))
  (let ((result
         (cond
          (nil
           (setq trace (cons 'a trace))
           (setq trace (cons 'b trace))
           'first)
          (t
           (setq trace (cons 'c trace))
           (setq trace (cons 'd trace))
           (setq trace (cons 'e trace))
           'second)
          (t
           (setq trace (cons 'f trace))
           'third))))
    (list result (nreverse trace))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 5. cond clause with single test (returns test value itself)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_single_test_return() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // When a cond clause has no body, the test value is returned
    let form = r#"(list
  (cond (42))
  (cond ('hello))
  (cond ("a string"))
  (cond ((+ 10 20)))
  (cond ((cons 'a 'b)))
  ;; First nil, then test-only returning its value
  (cond (nil) ((+ 3 4)))
  ;; Numeric expression as test
  (cond ((* 6 7))
        (t 'never))
  ;; Assoc as test (returns the found pair)
  (cond ((assoc 'b '((a . 1) (b . 2) (c . 3))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 6. Deeply nested cond (cond inside cond inside cond)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_deeply_nested() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((classify
         (lambda (shape color size)
           (cond
            ((eq shape 'circle)
             (cond
              ((eq color 'red)
               (cond
                ((eq size 'big) "big red circle")
                ((eq size 'small) "small red circle")
                (t "medium red circle")))
              ((eq color 'blue) "blue circle")
              (t (format "%s circle" color))))
            ((eq shape 'square)
             (cond
              ((eq size 'big) "big square")
              (t "square")))
            (t "unknown shape")))))
  (list
   (funcall classify 'circle 'red 'big)
   (funcall classify 'circle 'red 'small)
   (funcall classify 'circle 'red 'medium)
   (funcall classify 'circle 'blue 'any)
   (funcall classify 'circle 'green 'any)
   (funcall classify 'square 'red 'big)
   (funcall classify 'square 'red 'small)
   (funcall classify 'triangle 'red 'big)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 7. cond with side effects tracked via accumulator
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_side_effects_tracked() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((counter 0)
      (log nil))
  (dolist (val '(1 2 3 4 5 6 7 8 9 10))
    (cond
     ((= (% val 3) 0)
      (setq counter (+ counter val))
      (setq log (cons (list 'div3 val counter) log)))
     ((= (% val 2) 0)
      (setq counter (+ counter (* val 2)))
      (setq log (cons (list 'even val counter) log)))
     (t
      (setq counter (1+ counter))
      (setq log (cons (list 'other val counter) log)))))
  (list counter (nreverse log)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 8. cond with complex test expressions (and/or/not combinations)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_complex_tests() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((eval-access
         (lambda (user role level)
           (cond
            ;; Admin with high level: full access
            ((and (eq role 'admin) (>= level 5))
             'full-access)
            ;; Admin or editor with level >= 3: write access
            ((and (or (eq role 'admin) (eq role 'editor))
                  (>= level 3))
             'write-access)
            ;; Not banned and level >= 1: read access
            ((and (not (eq role 'banned))
                  (>= level 1))
             'read-access)
            ;; Banned users
            ((eq role 'banned) 'no-access)
            ;; Default
            (t 'guest-access)))))
  (list
   (funcall eval-access "alice" 'admin 7)
   (funcall eval-access "bob" 'admin 3)
   (funcall eval-access "carol" 'editor 4)
   (funcall eval-access "dave" 'editor 1)
   (funcall eval-access "eve" 'viewer 2)
   (funcall eval-access "frank" 'banned 10)
   (funcall eval-access "grace" 'viewer 0)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 9. cond inside a recursive function definition
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_in_recursive_defun() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (fset 'neovm--cond-test-eval
    (lambda (expr env)
      (cond
       ;; Number literal
       ((numberp expr) expr)
       ;; Symbol lookup
       ((symbolp expr)
        (let ((binding (assq expr env)))
          (cond
           (binding (cdr binding))
           (t (error "Unbound: %s" expr)))))
       ;; (+ a b)
       ((and (consp expr) (eq (car expr) '+))
        (+ (funcall 'neovm--cond-test-eval (nth 1 expr) env)
           (funcall 'neovm--cond-test-eval (nth 2 expr) env)))
       ;; (* a b)
       ((and (consp expr) (eq (car expr) '*))
        (* (funcall 'neovm--cond-test-eval (nth 1 expr) env)
           (funcall 'neovm--cond-test-eval (nth 2 expr) env)))
       ;; (let1 var val body)
       ((and (consp expr) (eq (car expr) 'let1))
        (let* ((var (nth 1 expr))
               (val (funcall 'neovm--cond-test-eval (nth 2 expr) env))
               (body (nth 3 expr)))
          (funcall 'neovm--cond-test-eval body (cons (cons var val) env))))
       ;; (neg x)
       ((and (consp expr) (eq (car expr) 'neg))
        (- 0 (funcall 'neovm--cond-test-eval (nth 1 expr) env)))
       (t (error "Unknown expr: %S" expr)))))

  (unwind-protect
      (let ((env '((x . 10) (y . 3))))
        (list
         (funcall 'neovm--cond-test-eval 42 env)
         (funcall 'neovm--cond-test-eval 'x env)
         (funcall 'neovm--cond-test-eval '(+ x y) env)
         (funcall 'neovm--cond-test-eval '(* x (+ y 2)) env)
         (funcall 'neovm--cond-test-eval '(let1 z 7 (+ z x)) env)
         (funcall 'neovm--cond-test-eval '(neg (+ x y)) env)
         (funcall 'neovm--cond-test-eval '(let1 a 5 (let1 b 3 (* a (+ b x)))) env)))
    (fmakunbound 'neovm--cond-test-eval)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 10. cond vs if: equivalent dispatch patterns produce same results
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_cond_vs_if_equivalence() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Verify that a cond and an equivalent nested if produce identical results
    let form = r#"(let ((via-cond
         (lambda (n)
           (cond
            ((< n 0) 'negative)
            ((= n 0) 'zero)
            ((< n 10) 'small)
            ((< n 100) 'medium)
            (t 'large))))
        (via-if
         (lambda (n)
           (if (< n 0) 'negative
             (if (= n 0) 'zero
               (if (< n 10) 'small
                 (if (< n 100) 'medium
                   'large)))))))
  (let ((inputs '(-99 -1 0 1 9 10 50 99 100 1000)))
    (list
     ;; Both produce same results
     (equal (mapcar via-cond inputs)
            (mapcar via-if inputs))
     ;; Show actual results from cond
     (mapcar via-cond inputs)
     ;; Show actual results from if
     (mapcar via-if inputs))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 11. cond with progn-like clause chaining and setq mutations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_stateful_dispatch() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((state 'idle)
      (transitions nil))
  (dolist (event '(start process error retry process done reset))
    (let ((old-state state))
      (cond
       ((and (eq state 'idle) (eq event 'start))
        (setq state 'running))
       ((and (eq state 'running) (eq event 'process))
        (setq state 'processing))
       ((and (eq state 'processing) (eq event 'done))
        (setq state 'idle))
       ((and (memq state '(running processing)) (eq event 'error))
        (setq state 'error))
       ((and (eq state 'error) (eq event 'retry))
        (setq state 'running))
       ((eq event 'reset)
        (setq state 'idle))
       (t nil))  ;; no valid transition
      (setq transitions
            (cons (list old-state event state
                        (if (eq old-state state) 'no-op 'transitioned))
                  transitions))))
  (list state (nreverse transitions)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// 12. cond with string-based dispatch and format
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cond_comprehensive_string_dispatch() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((parse-token
         (lambda (tok)
           (cond
            ((string-match "\\`[0-9]+\\'" tok)
             (list 'number (string-to-number tok)))
            ((string-match "\\`[a-zA-Z_][a-zA-Z0-9_]*\\'" tok)
             (cond
              ((member tok '("if" "else" "while" "for" "return"))
               (list 'keyword tok))
              (t (list 'identifier tok))))
            ((member tok '("+" "-" "*" "/" "=" "==" "!=" "<" ">"))
             (list 'operator tok))
            ((member tok '("(" ")" "{" "}" "[" "]" ";" ","))
             (list 'punctuation tok))
            ((string-match "\\`\"" tok)
             (list 'string tok))
            (t (list 'unknown tok))))))
  (mapcar parse-token
          '("42" "hello" "if" "+" "(" ";" "3" "return"
            "my_var" "==" "}" "\"hi\"" "???" "while" "0" "x1")))"#;
    assert_oracle_parity_with_bootstrap(form);
}
