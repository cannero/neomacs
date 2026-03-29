//! Oracle parity tests for advanced `while` loop patterns:
//! while with multiple state variables, while as iterator,
//! while with complex termination conditions, and nested while.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// while with multiple state variables
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_fibonacci_generator() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Generate Fibonacci numbers up to limit
    let form = "(let ((a 0) (b 1) (result nil) (limit 100))
                  (while (<= a limit)
                    (setq result (cons a result))
                    (let ((next (+ a b)))
                      (setq a b b next)))
                  (nreverse result))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_while_collatz_sequence() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Collatz conjecture sequence
    let form = "(let ((n 27) (steps nil))
                  (while (/= n 1)
                    (setq steps (cons n steps))
                    (if (= 0 (% n 2))
                        (setq n (/ n 2))
                      (setq n (+ (* 3 n) 1))))
                  (setq steps (cons 1 steps))
                  (list (length steps) (apply #'max (nreverse steps))))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// while as scan/fold
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_running_statistics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Compute running statistics in single pass
    let form = "(let ((data '(4 8 15 16 23 42))
                      (remaining nil)
                      (n 0) (sum 0) (min-val nil) (max-val nil))
                  (setq remaining data)
                  (while remaining
                    (let ((x (car remaining)))
                      (setq n (1+ n)
                            sum (+ sum x))
                      (when (or (null min-val) (< x min-val))
                        (setq min-val x))
                      (when (or (null max-val) (> x max-val))
                        (setq max-val x)))
                    (setq remaining (cdr remaining)))
                  (list n sum min-val max-val
                        (/ (float sum) n)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Nested while loops
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_nested_multiplication_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((result nil) (i 1))
                  (while (<= i 4)
                    (let ((row nil) (j 1))
                      (while (<= j 4)
                        (setq row (cons (* i j) row))
                        (setq j (1+ j)))
                      (setq result (cons (nreverse row) result)))
                    (setq i (1+ i)))
                  (nreverse result))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_while_nested_find_pairs() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Find all pairs summing to target
    let form = "(let ((nums '(1 2 3 4 5 6 7 8 9))
                      (target 10)
                      (pairs nil))
                  (let ((outer nums))
                    (while outer
                      (let ((inner (cdr outer)))
                        (while inner
                          (when (= (+ (car outer) (car inner)) target)
                            (setq pairs
                                  (cons (list (car outer) (car inner))
                                        pairs)))
                          (setq inner (cdr inner))))
                      (setq outer (cdr outer))))
                  (nreverse pairs))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// while with complex termination
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_newton_sqrt() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Newton's method for square root
    let form = "(let ((n 2.0)
                      (guess 1.0)
                      (epsilon 1e-10)
                      (iterations 0))
                  (while (> (abs (- (* guess guess) n)) epsilon)
                    (setq guess (/ (+ guess (/ n guess)) 2.0)
                          iterations (1+ iterations)))
                  (list (< (abs (- (* guess guess) n)) epsilon)
                        (< iterations 100)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(t t)", &o, &n);
}

#[test]
fn oracle_prop_while_converge() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Iterate until convergence
    let form = "(let ((x 100.0) (prev nil) (steps 0))
                  (while (or (null prev)
                             (> (abs (- x prev)) 0.001))
                    (setq prev x
                          x (/ (+ x (/ 50.0 x)) 2.0)
                          steps (1+ steps)))
                  (list (< (abs (- (* x x) 50.0)) 0.01)
                        (< steps 50)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(t t)", &o, &n);
}

// ---------------------------------------------------------------------------
// while with buffer iteration
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_while_buffer_tokenizer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "alpha=1 beta=2 gamma=3")
                    (goto-char (point-min))
                    (let ((tokens nil))
                      (while (re-search-forward
                              "\\([a-z]+\\)=\\([0-9]+\\)" nil t)
                        (setq tokens
                              (cons (cons (match-string 1)
                                          (string-to-number
                                           (match-string 2)))
                                    tokens)))
                      (nreverse tokens)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
