//! Oracle parity tests for advanced `dolist` and `dotimes` patterns:
//! result forms, nested loops, fibonacci, cartesian products,
//! matrix operations, dynamic list construction, and early return via catch/throw.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{
    assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
    eval_oracle_and_neovm_with_bootstrap,
};

// ---------------------------------------------------------------------------
// dolist with result form referencing accumulator and loop var
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_dolist_result_form_complex() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Result form can reference the loop variable (nil after loop) and accumulator.
    // Also tests that the result form is evaluated in the same lexical scope.
    let form = r#"(let ((acc nil)
                        (count 0))
                    (dolist (item '(alpha beta gamma delta epsilon)
                            (list (nreverse acc) count item))
                      (setq count (1+ count))
                      (when (> (length (symbol-name item)) 4)
                        (setq acc (cons (cons item (length (symbol-name item))) acc)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Nested dolist: cartesian product with filtering
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_dolist_nested_cartesian_filtered() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Cartesian product of three lists, keeping only triples that satisfy a constraint
    let form = r#"(let ((result nil))
                    (dolist (x '(1 2 3 4 5))
                      (dolist (y '(1 2 3 4 5))
                        (dolist (z '(1 2 3 4 5))
                          (when (and (< x y) (< y z)
                                     (= (+ (* x x) (* y y)) (* z z)))
                            (setq result (cons (list x y z) result))))))
                    (nreverse result))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// dotimes with result form accumulating a running product
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_dotimes_result_accumulating_factorial_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a table of (n . n!) using dotimes, return via result form
    let form = r#"(let ((table nil)
                        (fact 1))
                    (dotimes (i 10 (nreverse table))
                      (if (= i 0)
                          (setq fact 1)
                        (setq fact (* fact i)))
                      (setq table (cons (cons i fact) table))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// dotimes generating fibonacci sequence
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_dotimes_fibonacci() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Generate first 15 Fibonacci numbers using dotimes with shifting accumulators
    let form = r#"(let ((fibs nil)
                        (a 0)
                        (b 1))
                    (dotimes (i 15 (nreverse fibs))
                      (setq fibs (cons a fibs))
                      (let ((next (+ a b)))
                        (setq a b
                              b next))))"#;
    let (o, n) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_ok_eq("(0 1 1 2 3 5 8 13 21 34 55 89 144 233 377)", &o, &n);
}

// ---------------------------------------------------------------------------
// Mixed dolist + dotimes for matrix transposition
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_dolist_dotimes_matrix_transpose() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a 3x4 matrix, then transpose it to 4x3 using mixed iteration
    let form = r#"(let* ((rows 3)
                         (cols 4)
                         ;; Build matrix as list of lists using dotimes
                         (matrix nil))
                    (dotimes (i rows)
                      (let ((row nil))
                        (dotimes (j cols)
                          (setq row (cons (+ (* i cols) j 1) row)))
                        (setq matrix (cons (nreverse row) matrix))))
                    (setq matrix (nreverse matrix))
                    ;; Transpose: iterate columns, then rows
                    (let ((transposed nil))
                      (dotimes (j cols)
                        (let ((new-row nil))
                          (dolist (row matrix)
                            (setq new-row (cons (nth j row) new-row)))
                          (setq transposed (cons (nreverse new-row) transposed))))
                      (list matrix (nreverse transposed))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// dolist over dynamically constructed lists with side effects
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_dolist_dynamic_list_side_effects() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a list dynamically, iterate it while modifying an external state (hash table),
    // then use hash table to compute final result
    let form = r#"(let ((freq (make-hash-table :test 'equal))
                        (words nil)
                        (sentence "the cat sat on the mat the cat"))
                    ;; Split sentence into words using a simple loop
                    (let ((start 0)
                          (len (length sentence)))
                      (dotimes (i (1+ len))
                        (when (or (= i len)
                                  (= (aref sentence i) ?\s))
                          (when (> i start)
                            (setq words (cons (substring sentence start i) words)))
                          (setq start (1+ i)))))
                    (setq words (nreverse words))
                    ;; Count frequencies
                    (dolist (w words)
                      (puthash w (1+ (gethash w freq 0)) freq))
                    ;; Collect as sorted alist
                    (let ((pairs nil))
                      (maphash (lambda (k v)
                                 (setq pairs (cons (cons k v) pairs)))
                               freq)
                      (sort pairs (lambda (a b) (string< (car a) (car b))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// dolist with early return via catch/throw
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_dolist_early_return_catch_throw() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Search a nested structure for a target using dolist + catch/throw for early exit
    let form = r#"(let ((data '((group-a (alice 90) (bob 85) (carol 72))
                                 (group-b (dave 95) (eve 88) (frank 60))
                                 (group-c (grace 78) (heidi 92) (ivan 50)))))
                    (catch 'found
                      (dolist (group data)
                        (let ((group-name (car group))
                              (members (cdr group)))
                          (dolist (member members)
                            (let ((name (car member))
                                  (score (cadr member)))
                              (when (>= score 95)
                                (throw 'found (list 'winner name
                                                    'in group-name
                                                    'with-score score)))))))
                      'nobody-found))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// dotimes building Pascal's triangle
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_dotimes_pascals_triangle() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build first 7 rows of Pascal's triangle using dotimes
    let form = r#"(let ((triangle nil)
                        (prev-row '(1)))
                    (setq triangle (cons prev-row triangle))
                    (dotimes (i 6)
                      (let ((new-row (list 1))
                            (row prev-row))
                        (while (cdr row)
                          (setq new-row (cons (+ (car row) (cadr row)) new-row))
                          (setq row (cdr row)))
                        (setq new-row (cons 1 new-row))
                        (setq new-row (nreverse new-row))
                        (setq triangle (cons new-row triangle))
                        (setq prev-row new-row)))
                    (nreverse triangle))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// dolist with unwind-protect ensuring cleanup of temp globals
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_dolist_unwind_protect_cleanup() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Process items while maintaining a global processing log,
    // ensure cleanup happens even if an error occurs mid-iteration
    let form = r#"(progn
                    (defvar neovm--test-process-log nil)
                    (unwind-protect
                        (let ((items '(10 20 0 30 40))
                              (results nil))
                          (dolist (item items (list (nreverse results)
                                                   (nreverse neovm--test-process-log)))
                            (setq neovm--test-process-log
                                  (cons (format "processing %d" item)
                                        neovm--test-process-log))
                            (condition-case err
                                (progn
                                  (when (= item 0)
                                    (error "Division by zero for item %d" item))
                                  (setq results (cons (/ 100 item) results)))
                              (error
                               (setq results (cons (list 'error (cadr err)) results))
                               (setq neovm--test-process-log
                                     (cons (format "error on %d" item)
                                           neovm--test-process-log))))))
                      (makunbound 'neovm--test-process-log)))"#;
    assert_oracle_parity_with_bootstrap(form);
}
