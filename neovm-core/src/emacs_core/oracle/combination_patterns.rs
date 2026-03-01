//! Complex real-world combination patterns from Elisp codebases.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm, ORACLE_PROP_CASES};

// ---------------------------------------------------------------------------
// Graph algorithms
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_pattern_topological_sort() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Topological sort via DFS on a DAG
    let form = "(progn
  (fset 'neovm--test-topo-visit
    (lambda (node graph visited result)
      (unless (gethash node visited)
        (puthash node t visited)
        (dolist (dep (cdr (assq node graph)))
          (funcall 'neovm--test-topo-visit
                   dep graph visited result))
        (setcar result (cons node (car result))))))
  (unwind-protect
      (let ((graph '((a . (b c)) (b . (d)) (c . (d)) (d . ())))
            (visited (make-hash-table))
            (result (list nil)))
        (dolist (node '(a b c d))
          (funcall 'neovm--test-topo-visit
                   node graph visited result))
        (car result))
    (fmakunbound 'neovm--test-topo-visit)))";
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Parser combinators
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_pattern_recursive_descent_parser() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Parse simple arithmetic: "3+4*2" into ((3 + (4 * 2)) . 5)
    let form = r#"(progn
  (fset 'neovm--test-parse-num
    (lambda (tokens pos)
      (if (and (< pos (length tokens))
               (numberp (aref tokens pos)))
          (cons (aref tokens pos) (1+ pos))
        nil)))
  (fset 'neovm--test-parse-factor
    (lambda (tokens pos)
      (funcall 'neovm--test-parse-num tokens pos)))
  (fset 'neovm--test-parse-term
    (lambda (tokens pos)
      (let ((left (funcall 'neovm--test-parse-factor tokens pos)))
        (when left
          (let ((lval (car left))
                (p (cdr left)))
            (while (and (< p (length tokens))
                        (eq (aref tokens p) '*))
              (let ((right (funcall 'neovm--test-parse-factor
                                    tokens (1+ p))))
                (when right
                  (setq lval (list '* lval (car right))
                        p (cdr right)))))
            (cons lval p))))))
  (fset 'neovm--test-parse-expr
    (lambda (tokens pos)
      (let ((left (funcall 'neovm--test-parse-term tokens pos)))
        (when left
          (let ((lval (car left))
                (p (cdr left)))
            (while (and (< p (length tokens))
                        (eq (aref tokens p) '+))
              (let ((right (funcall 'neovm--test-parse-term
                                    tokens (1+ p))))
                (when right
                  (setq lval (list '+ lval (car right))
                        p (cdr right)))))
            (cons lval p))))))
  (unwind-protect
      (let ((tokens [3 + 4 * 2]))
        (funcall 'neovm--test-parse-expr tokens 0))
    (fmakunbound 'neovm--test-parse-num)
    (fmakunbound 'neovm--test-parse-factor)
    (fmakunbound 'neovm--test-parse-term)
    (fmakunbound 'neovm--test-parse-expr)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Memoization with complex keys
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_pattern_memoized_path_counting() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Count paths in a grid from (0,0) to (m,n)
    let form = "(let ((memo (make-hash-table :test 'equal)))
  (fset 'neovm--test-count-paths
    (lambda (m n)
      (let ((key (cons m n)))
        (or (gethash key memo)
            (let ((result
                   (cond
                     ((or (= m 0) (= n 0)) 1)
                     (t (+ (funcall 'neovm--test-count-paths
                                    (1- m) n)
                           (funcall 'neovm--test-count-paths
                                    m (1- n)))))))
              (puthash key result memo)
              result)))))
  (unwind-protect
      (list (funcall 'neovm--test-count-paths 0 0)
            (funcall 'neovm--test-count-paths 2 2)
            (funcall 'neovm--test-count-paths 3 3)
            (funcall 'neovm--test-count-paths 4 4))
    (fmakunbound 'neovm--test-count-paths)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 6 20 70)", &o, &n);
}

// ---------------------------------------------------------------------------
// Command pattern with undo
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_pattern_command_with_undo() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Execute commands with undo stack
    let form = "(let ((state 0)
                      (undo-stack nil))
                  (let ((execute
                         (lambda (cmd)
                           (let ((old state))
                             (cond
                               ((eq (car cmd) 'add)
                                (setq state (+ state (cadr cmd))))
                               ((eq (car cmd) 'mul)
                                (setq state (* state (cadr cmd)))))
                             (setq undo-stack
                                   (cons old undo-stack)))))
                        (undo
                         (lambda ()
                           (when undo-stack
                             (setq state (car undo-stack)
                                   undo-stack (cdr undo-stack))))))
                    (funcall execute '(add 5))
                    (funcall execute '(mul 3))
                    (funcall execute '(add 2))
                    (let ((before-undo state))
                      (funcall undo)
                      (funcall undo)
                      (list before-undo state undo-stack))))";
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Lazy sequence via closures
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_pattern_lazy_range() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Lazy infinite range, take first N
    let form = "(let ((make-lazy-range
                       (lambda (start step)
                         (let ((current start))
                           (lambda ()
                             (prog1 current
                               (setq current (+ current step)))))))
                      (take-n
                       (lambda (gen n)
                         (let ((result nil))
                           (dotimes (_ n)
                             (setq result (cons (funcall gen) result)))
                           (nreverse result)))))
                  (let ((odds (funcall make-lazy-range 1 2)))
                    (funcall take-n odds 7)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 3 5 7 9 11 13)", &o, &n);
}

// ---------------------------------------------------------------------------
// Compile-time computation via macros
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_pattern_macro_dispatch_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Macro that builds a cond dispatch from an alist
    let form = "(progn
  (defmacro neovm--test-dispatch (key table)
    (let ((clauses nil))
      (dolist (entry (reverse table))
        (setq clauses
              (cons `((eq ,key ',(car entry)) ,(cdr entry))
                    clauses)))
      `(cond ,@clauses (t 'unknown))))
  (unwind-protect
      (list (neovm--test-dispatch 'add
              ((add . 'addition) (sub . 'subtraction)
               (mul . 'multiplication)))
            (neovm--test-dispatch 'mul
              ((add . 'addition) (sub . 'subtraction)
               (mul . 'multiplication)))
            (neovm--test-dispatch 'div
              ((add . 'addition) (sub . 'subtraction)
               (mul . 'multiplication))))
    (fmakunbound 'neovm--test-dispatch)))";
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Accumulator with multiple return values
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_pattern_stats_accumulator() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Compute sum, count, min, max in a single pass
    let form = "(let ((data '(7 3 9 1 5 8 2 6 4 10))
                      (sum 0) (count 0)
                      (mn nil) (mx nil))
                  (dolist (x data)
                    (setq sum (+ sum x)
                          count (1+ count))
                    (when (or (null mn) (< x mn)) (setq mn x))
                    (when (or (null mx) (> x mx)) (setq mx x)))
                  (list (cons 'sum sum)
                        (cons 'count count)
                        (cons 'min mn)
                        (cons 'max mx)
                        (cons 'mean (/ sum count))))";
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Run-length encoding/decoding
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_pattern_run_length_encode() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
  (fset 'neovm--test-rle-encode
    (lambda (lst)
      (if (null lst) nil
        (let ((result nil)
              (current (car lst))
              (count 1)
              (rest (cdr lst)))
          (while rest
            (if (eq (car rest) current)
                (setq count (1+ count))
              (setq result (cons (cons count current) result)
                    current (car rest)
                    count 1))
            (setq rest (cdr rest)))
          (nreverse (cons (cons count current) result))))))
  (unwind-protect
      (funcall 'neovm--test-rle-encode
               '(a a a b b c c c c d))
    (fmakunbound 'neovm--test-rle-encode)))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_pattern_run_length_decode() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
  (fset 'neovm--test-rle-decode
    (lambda (encoded)
      (let ((result nil))
        (dolist (pair encoded)
          (dotimes (_ (car pair))
            (setq result (cons (cdr pair) result))))
        (nreverse result))))
  (unwind-protect
      (funcall 'neovm--test-rle-decode
               '((3 . a) (2 . b) (4 . c) (1 . d)))
    (fmakunbound 'neovm--test-rle-decode)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(a a a b b c c c c d)", &o, &n);
}

// ---------------------------------------------------------------------------
// Complex proptest
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_pattern_grid_paths(
        m in 0u32..5u32,
        n in 0u32..5u32,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(let ((memo (make-hash-table :test 'equal)))
  (fset 'neovm--test-cp
    (lambda (m n)
      (let ((key (cons m n)))
        (or (gethash key memo)
            (let ((r (cond ((or (= m 0) (= n 0)) 1)
                           (t (+ (funcall 'neovm--test-cp (1- m) n)
                                 (funcall 'neovm--test-cp m (1- n)))))))
              (puthash key r memo) r)))))
  (unwind-protect
      (funcall 'neovm--test-cp {} {})
    (fmakunbound 'neovm--test-cp)))",
            m, n
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }
}
