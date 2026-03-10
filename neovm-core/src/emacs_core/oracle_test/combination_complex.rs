//! Complex cross-feature combination oracle tests.
//!
//! These tests exercise deep interactions between multiple Elisp features:
//! closures, recursion, error handling, macros, mutation, higher-order
//! functions, hash tables, and control flow.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
    eval_oracle_and_neovm_with_bootstrap,
};

// ---------------------------------------------------------------------------
// State machines and accumulators
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_state_machine_via_closures() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Implement a tiny state machine: start -> running -> done
    let form = "(let ((state 'start)
                      (log nil))
                  (let ((transition
                         (lambda (event)
                           (setq log (cons (cons state event) log))
                           (cond
                             ((and (eq state 'start) (eq event 'go))
                              (setq state 'running))
                             ((and (eq state 'running) (eq event 'finish))
                              (setq state 'done))
                             ((eq event 'reset)
                              (setq state 'start))
                             (t (setq state 'error))))))
                    (funcall transition 'go)
                    (funcall transition 'finish)
                    (funcall transition 'reset)
                    (funcall transition 'go)
                    (list state (nreverse log))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_accumulator_with_error_recovery() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Process a list of operations, recovering from errors
    let form = "(let ((result 0)
                      (errors nil))
                  (dolist (op '((+ 5) (+ 3) (/ 0) (+ 2) (* bad) (+ 1)))
                    (condition-case err
                        (let ((fn (car op))
                              (arg (cadr op)))
                          (setq result (funcall fn result arg)))
                      (error
                       (setq errors (cons (list (car err) (car op)) errors)))))
                  (list result (nreverse errors)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Higher-order patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_pipeline_via_funcall_chain() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Functional pipeline: compose a list of transformations
    let form = "(let ((pipeline (lambda (fns val)
                                  (let ((result val))
                                    (dolist (f fns result)
                                      (setq result (funcall f result)))))))
                  (funcall pipeline
                           (list (lambda (x) (+ x 10))
                                 (lambda (x) (* x 2))
                                 (lambda (x) (- x 3)))
                           5))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("27", &o, &n);
}

#[test]
fn oracle_prop_combo_reduce_via_closure() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Implement reduce/fold-left via closure
    let form = "(let ((my-reduce (lambda (f init lst)
                                    (let ((acc init))
                                      (while lst
                                        (setq acc (funcall f acc (car lst))
                                              lst (cdr lst)))
                                      acc))))
                  (list (funcall my-reduce '+ 0 '(1 2 3 4 5))
                        (funcall my-reduce '* 1 '(1 2 3 4 5))
                        (funcall my-reduce 'max 0 '(3 1 4 1 5 9 2 6))
                        (funcall my-reduce (lambda (acc x) (cons x acc)) nil '(a b c))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_filter_map_via_closures() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // filter then map
    let form = "(let ((my-filter (lambda (pred lst)
                                    (let ((result nil))
                                      (dolist (x lst (nreverse result))
                                        (when (funcall pred x)
                                          (setq result (cons x result))))))))
                  (mapcar (lambda (x) (* x x))
                          (funcall my-filter
                                   (lambda (x) (= 0 (% x 2)))
                                   '(1 2 3 4 5 6 7 8 9 10))))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(4 16 36 64 100)", &o, &n);
}

#[test]
fn oracle_prop_combo_zip_two_lists() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((my-zip (lambda (a b)
                                 (let ((result nil))
                                   (while (and a b)
                                     (setq result (cons (cons (car a) (car b)) result)
                                           a (cdr a)
                                           b (cdr b)))
                                   (nreverse result)))))
                  (funcall my-zip '(a b c) '(1 2 3)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Hash table + closure patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_frequency_counter() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a frequency counter using hash tables
    let form = "(let ((counts (make-hash-table :test 'eq)))
                  (dolist (x '(a b a c b a d c a b))
                    (puthash x (1+ (or (gethash x counts) 0)) counts))
                  (let ((result nil))
                    (dolist (key '(a b c d))
                      (setq result (cons (cons key (gethash key counts)) result)))
                    (nreverse result)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_memoized_fibonacci() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Memoized fibonacci using hash table
    let form = "(let ((memo (make-hash-table)))
                  (puthash 0 0 memo)
                  (puthash 1 1 memo)
                  (fset 'neovm--test-memo-fib
                        (lambda (n)
                          (or (gethash n memo)
                              (let ((result (+ (funcall 'neovm--test-memo-fib (- n 1))
                                               (funcall 'neovm--test-memo-fib (- n 2)))))
                                (puthash n result memo)
                                result))))
                  (unwind-protect
                      (mapcar 'neovm--test-memo-fib '(0 1 2 5 10 15 20))
                    (fmakunbound 'neovm--test-memo-fib)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_hash_table_group_by() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Group items by a key function
    let form = "(let ((groups (make-hash-table :test 'eq)))
                  (dolist (x '(1 2 3 4 5 6 7 8 9 10))
                    (let ((key (if (= 0 (% x 2)) 'even 'odd)))
                      (puthash key (cons x (or (gethash key groups) nil)) groups)))
                  (list (nreverse (gethash 'even groups))
                        (nreverse (gethash 'odd groups))))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Macro + closure + error interactions
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_macro_generates_closures() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (defmacro neovm--test-make-counter (init)
                    `(let ((n ,init))
                       (list (lambda () (setq n (1+ n)) n)
                             (lambda () n))))
                  (unwind-protect
                      (let ((counter (neovm--test-make-counter 10)))
                        (funcall (car counter))
                        (funcall (car counter))
                        (funcall (car counter))
                        (funcall (cadr counter)))
                    (fmakunbound 'neovm--test-make-counter)))";
    let (o, n) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_ok_eq("13", &o, &n);
}

#[test]
fn oracle_prop_combo_macro_with_unwind_protect() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (defmacro neovm--test-with-log (var &rest body)
                    `(let ((,var nil))
                       (unwind-protect
                           (progn ,@body)
                         (setq ,var (nreverse ,var)))))
                  (unwind-protect
                      (let (log)
                        (neovm--test-with-log log
                          (setq log (cons 'start log))
                          (setq log (cons 'middle log))
                          (setq log (cons 'end log)))
                        log)
                    (fmakunbound 'neovm--test-with-log)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_nested_catch_throw_with_closures() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((log nil))
                  (let ((result
                         (catch 'outer
                           (catch 'inner
                             (let ((escape (lambda (tag val)
                                             (setq log (cons (list 'throwing tag val) log))
                                             (throw tag val))))
                               (setq log (cons 'start log))
                               (funcall escape 'inner 'from-inner)
                               (setq log (cons 'unreachable log)))))))
                    (list result (nreverse log))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_recursive_error_collection() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Walk a data structure, collecting errors from invalid operations
    let form = "(let ((errors nil))
                  (fset 'neovm--test-safe-process
                        (lambda (tree)
                          (cond
                            ((null tree) 0)
                            ((numberp tree) tree)
                            ((consp tree)
                             (condition-case err
                                 (+ (funcall 'neovm--test-safe-process (car tree))
                                    (funcall 'neovm--test-safe-process (cdr tree)))
                               (error
                                (setq errors (cons (car err) errors))
                                0)))
                            (t (signal 'wrong-type-argument (list 'numberp tree))))))
                  (unwind-protect
                      (let ((sum (funcall 'neovm--test-safe-process '(1 (2 bad 3) (4 (5 oops))))))
                        (list sum (length errors)))
                    (fmakunbound 'neovm--test-safe-process)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// String building patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_string_builder_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((parts nil))
                    (dotimes (i 5)
                      (setq parts (cons (format "item-%d" i) parts)))
                    (mapconcat 'identity (nreverse parts) ", "))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_string_repeat_join() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((words '("hello" "world" "foo" "bar")))
                    (mapconcat 'upcase words " | "))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Alist manipulation patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_alist_update_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Common pattern: update an alist by consing new pair at front
    let form = "(let ((env '((x . 1) (y . 2) (z . 3))))
                  (let ((env (cons (cons 'x 99) env)))
                    (list (assq 'x env)
                          (assq 'y env)
                          (assq 'z env))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_alist_to_hash_and_back() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Convert alist -> hash-table -> alist
    let form = "(let ((al '((a . 1) (b . 2) (c . 3)))
                      (ht (make-hash-table :test 'eq)))
                  (dolist (pair al)
                    (puthash (car pair) (cdr pair) ht))
                  (let ((result nil))
                    (dolist (key '(a b c))
                      (setq result (cons (cons key (gethash key ht)) result)))
                    (nreverse result)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Deeply nested control flow
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_deeply_nested_lets() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((a 1))
                  (let ((b (+ a 1)))
                    (let ((c (+ b 1)))
                      (let ((d (+ c 1)))
                        (let ((e (+ d 1)))
                          (list a b c d e (+ a b c d e)))))))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 2 3 4 5 15)", &o, &n);
}

#[test]
fn oracle_prop_combo_nested_condition_case_layers() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(condition-case outer-err
                  (condition-case mid-err
                      (condition-case inner-err
                          (progn
                            (list (+ 1 2)
                                  (car 'not-a-list)))
                        (wrong-type-argument
                         (signal 'error (list \"propagated\" (cdr inner-err)))))
                    (error
                     (list 'caught-mid (car mid-err) (cadr mid-err))))
                  (error (list 'caught-outer (car outer-err))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_complex_while_with_multiple_exits() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // while loop with multiple exit conditions
    let form = "(let ((i 0)
                      (sum 0)
                      (found nil))
                  (catch 'done
                    (while (< i 100)
                      (setq sum (+ sum i))
                      (when (> sum 50)
                        (setq found i)
                        (throw 'done nil))
                      (setq i (1+ i))))
                  (list found sum))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Setq with multiple pairs
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_setq_multiple_pairs() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let (a b c)
                  (setq a 1 b 2 c 3)
                  (list a b c))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 2 3)", &o, &n);
}

#[test]
fn oracle_prop_combo_setq_sequential_dependency() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // setq evaluates pairs left-to-right, each sees previous bindings
    let form = "(let ((x 0))
                  (setq x 5 x (+ x 10))
                  x)";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("15", &o, &n);
}

// ---------------------------------------------------------------------------
// Vector operations combined
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_vector_map_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((v [10 20 30 40 50])
                      (result nil))
                  (dotimes (i (length v))
                    (setq result (cons (* 2 (aref v i)) result)))
                  (nreverse result))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(20 40 60 80 100)", &o, &n);
}

#[test]
fn oracle_prop_combo_vector_accumulate_with_aset() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((v (vector 0 0 0 0 0)))
                  (dotimes (i 5)
                    (aset v i (* i i)))
                  (list (aref v 0) (aref v 1) (aref v 2) (aref v 3) (aref v 4)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(0 1 4 9 16)", &o, &n);
}

// ---------------------------------------------------------------------------
// Sorting with complex comparators
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_sort_with_custom_predicate() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Sort by absolute value
    let form = "(let ((lst (list 3 -1 4 -1 5 -9 2 -6)))
                  (sort lst (lambda (a b) (< (abs a) (abs b)))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_sort_alist_by_value() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((al (list (cons 'b 2) (cons 'a 1) (cons 'c 3) (cons 'd 0))))
                  (sort al (lambda (x y) (< (cdr x) (cdr y)))))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex mapcar chains
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_mapcar_chain() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Chained mapcar transformations
    let form = "(let ((data '(1 2 3 4 5)))
                  (mapcar 'number-to-string
                          (mapcar (lambda (x) (* x x))
                                  (mapcar '1+ data))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_mapcar_with_index_via_counter() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Simulate mapcar-with-index using a mutable counter
    let form = "(let ((idx 0))
                  (mapcar (lambda (x)
                            (prog1 (list idx x)
                              (setq idx (1+ idx))))
                          '(a b c d)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Property list operations in context
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_plist_based_config() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((config '(:width 80 :height 24 :color t :name \"term\")))
                  (list (plist-get config :width)
                        (plist-get config :height)
                        (plist-get config :color)
                        (plist-get config :name)
                        (plist-get config :missing)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Recursive data structure building
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_build_binary_tree() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a sorted binary tree, then flatten via in-order traversal
    let form = "(progn
                  (fset 'neovm--test-tree-insert
                        (lambda (tree val)
                          (if (null tree)
                              (list val nil nil)
                            (let ((node-val (car tree))
                                  (left (cadr tree))
                                  (right (caddr tree)))
                              (if (< val node-val)
                                  (list node-val
                                        (funcall 'neovm--test-tree-insert left val)
                                        right)
                                (list node-val
                                      left
                                      (funcall 'neovm--test-tree-insert right val)))))))
                  (fset 'neovm--test-tree-inorder
                        (lambda (tree)
                          (if (null tree) nil
                            (append (funcall 'neovm--test-tree-inorder (cadr tree))
                                    (list (car tree))
                                    (funcall 'neovm--test-tree-inorder (caddr tree))))))
                  (unwind-protect
                      (let ((tree nil))
                        (dolist (x '(5 3 7 1 4 6 8 2))
                          (setq tree (funcall 'neovm--test-tree-insert tree x)))
                        (funcall 'neovm--test-tree-inorder tree))
                    (fmakunbound 'neovm--test-tree-insert)
                    (fmakunbound 'neovm--test-tree-inorder)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 2 3 4 5 6 7 8)", &o, &n);
}

// ---------------------------------------------------------------------------
// Complex error handling patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_retry_pattern() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Retry a failing operation up to N times
    let form = "(let ((attempts 0)
                      (success nil))
                  (catch 'done
                    (dotimes (try 5)
                      (setq attempts (1+ attempts))
                      (condition-case nil
                          (progn
                            (when (< attempts 3)
                              (signal 'error '(\"not ready\")))
                            (setq success t)
                            (throw 'done nil))
                        (error nil))))
                  (list success attempts))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_combo_unwind_protect_chain() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Chain of unwind-protect with different cleanup actions
    let form = "(let ((log nil))
                  (condition-case nil
                      (unwind-protect
                          (unwind-protect
                              (unwind-protect
                                  (progn
                                    (setq log (cons 'body log))
                                    (signal 'error '(\"boom\")))
                                (setq log (cons 'cleanup-1 log)))
                            (setq log (cons 'cleanup-2 log)))
                        (setq log (cons 'cleanup-3 log)))
                    (error nil))
                  (nreverse log))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(body cleanup-1 cleanup-2 cleanup-3)", &o, &n);
}

// ---------------------------------------------------------------------------
// Let* with complex dependencies
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_combo_let_star_computation_chain() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let* ((a 2)
                       (b (* a 3))
                       (c (+ a b))
                       (d (list a b c))
                       (e (length d))
                       (f (apply '+ d)))
                  (list a b c d e f))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Proptest: complex forms with random data
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_combo_mapcar_filter_reduce(
        a in -50i64..50i64,
        b in -50i64..50i64,
        c in -50i64..50i64,
        d in -50i64..50i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        // Map, filter evens, sum
        let form = format!(
            "(let ((data '({} {} {} {})))
               (let ((doubled (mapcar (lambda (x) (* x 2)) data))
                     (sum 0))
                 (dolist (x doubled sum)
                   (setq sum (+ sum x)))))",
            a, b, c, d
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }

    #[test]
    fn oracle_prop_combo_closure_over_random_values(
        x in -100i64..100i64,
        y in -100i64..100i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(let ((x {}) (y {}))
               (let ((f (lambda () (+ x y)))
                     (g (lambda () (* x y))))
                 (list (funcall f) (funcall g) (+ (funcall f) (funcall g)))))",
            x, y
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }

    #[test]
    fn oracle_prop_combo_recursive_sum_proptest(
        a in 0i64..20i64,
        b in 0i64..20i64,
        c in 0i64..20i64,
        d in 0i64..20i64,
        e in 0i64..20i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(progn
               (fset 'neovm--test-rsum
                     (lambda (lst) (if (null lst) 0 (+ (car lst) (funcall 'neovm--test-rsum (cdr lst))))))
               (unwind-protect
                   (funcall 'neovm--test-rsum '({} {} {} {} {}))
                 (fmakunbound 'neovm--test-rsum)))",
            a, b, c, d, e
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }
}
