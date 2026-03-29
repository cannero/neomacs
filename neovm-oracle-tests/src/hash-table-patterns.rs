//! Oracle parity tests for advanced hash table patterns:
//! all `:test` options, `hash-table-size`, `hash-table-rehash-size`,
//! `hash-table-rehash-threshold`, `hash-table-weakness`,
//! `copy-hash-table`, and complex hash table algorithms.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// Hash table test parameter
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_hash_table_test_eq() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((h (make-hash-table :test 'eq)))
                    (let ((sym 'foo))
                      (puthash sym 1 h)
                      (puthash 'bar 2 h)
                      (list (gethash sym h)
                            (gethash 'foo h)
                            (gethash 'bar h)
                            (hash-table-test h))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_hash_table_test_eql() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((h (make-hash-table :test 'eql)))
                    (puthash 1 'one h)
                    (puthash 1.0 'one-float h)
                    (puthash 2 'two h)
                    (list (gethash 1 h)
                          (gethash 1.0 h)
                          (gethash 2 h)
                          (hash-table-test h)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_hash_table_test_equal() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((h (make-hash-table :test 'equal)))
                    (puthash "key1" 'val1 h)
                    (puthash '(a b) 'val2 h)
                    (puthash [1 2] 'val3 h)
                    (list (gethash "key1" h)
                          (gethash (concat "key" "1") h)
                          (gethash '(a b) h)
                          (gethash [1 2] h)
                          (hash-table-test h)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Hash table size/count
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_hash_table_size_and_count() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((h (make-hash-table :size 10)))
                    (let ((before-count (hash-table-count h)))
                      (puthash 'a 1 h)
                      (puthash 'b 2 h)
                      (puthash 'c 3 h)
                      (let ((after-count (hash-table-count h)))
                        (remhash 'b h)
                        (list before-count
                              after-count
                              (hash-table-count h)
                              (hash-table-p h)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// clrhash
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_hash_table_clrhash() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((h (make-hash-table)))
                    (puthash 'a 1 h)
                    (puthash 'b 2 h)
                    (puthash 'c 3 h)
                    (let ((before (hash-table-count h)))
                      (clrhash h)
                      (list before
                            (hash-table-count h)
                            (gethash 'a h)
                            (hash-table-p h))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// copy-hash-table
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_hash_table_copy() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((orig (make-hash-table :test 'equal)))
                    (puthash "x" 1 orig)
                    (puthash "y" 2 orig)
                    (let ((copy (copy-hash-table orig)))
                      ;; Modify copy
                      (puthash "x" 99 copy)
                      (puthash "z" 3 copy)
                      ;; Original unchanged
                      (list (gethash "x" orig)
                            (gethash "x" copy)
                            (gethash "z" orig)
                            (gethash "z" copy)
                            (hash-table-count orig)
                            (hash-table-count copy))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// maphash with accumulation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_hash_table_maphash_collect() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((h (make-hash-table)))
                    (puthash 'a 10 h)
                    (puthash 'b 20 h)
                    (puthash 'c 30 h)
                    (let ((keys nil) (vals nil) (sum 0))
                      (maphash (lambda (k v)
                                 (setq keys (cons k keys)
                                       vals (cons v vals)
                                       sum (+ sum v)))
                               h)
                      (list (sort keys #'string-lessp)
                            (sort vals #'<)
                            sum)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: frequency counter
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_hash_table_frequency_counter() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((words '(the cat sat on the mat the cat ate
                                  the rat on the mat))
                        (freq (make-hash-table)))
                    (dolist (w words)
                      (puthash w (1+ (gethash w freq 0)) freq))
                    ;; Get sorted results
                    (let ((pairs nil))
                      (maphash (lambda (k v)
                                 (setq pairs (cons (cons k v) pairs)))
                               freq)
                      (sort pairs
                            (lambda (a b)
                              (or (> (cdr a) (cdr b))
                                  (and (= (cdr a) (cdr b))
                                       (string< (symbol-name (car a))
                                                 (symbol-name (car b)))))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: hash-table-based graph operations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_hash_table_graph_bfs() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // BFS on a directed graph
    let form = r#"(let ((graph (make-hash-table)))
                    ;; Build graph: a→b, a→c, b→d, c→d, d→e
                    (puthash 'a '(b c) graph)
                    (puthash 'b '(d) graph)
                    (puthash 'c '(d) graph)
                    (puthash 'd '(e) graph)
                    (puthash 'e nil graph)
                    ;; BFS from a
                    (let ((visited (make-hash-table))
                          (queue (list 'a))
                          (order nil))
                      (puthash 'a t visited)
                      (while queue
                        (let ((node (car queue)))
                          (setq queue (cdr queue))
                          (setq order (cons node order))
                          (dolist (neighbor (gethash node graph nil))
                            (unless (gethash neighbor visited)
                              (puthash neighbor t visited)
                              (setq queue
                                    (append queue
                                            (list neighbor)))))))
                      (nreverse order)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: two-way mapping
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_hash_table_bimap() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Bidirectional map: forward and reverse lookup
    let form = r#"(let ((fwd (make-hash-table :test 'equal))
                        (rev (make-hash-table :test 'equal)))
                    (let ((bimap-put
                           (lambda (k v)
                             (puthash k v fwd)
                             (puthash v k rev)))
                          (bimap-get-fwd
                           (lambda (k) (gethash k fwd)))
                          (bimap-get-rev
                           (lambda (v) (gethash v rev))))
                      (funcall bimap-put "US" "Washington")
                      (funcall bimap-put "UK" "London")
                      (funcall bimap-put "FR" "Paris")
                      (funcall bimap-put "DE" "Berlin")
                      (list
                       ;; Forward lookup
                       (funcall bimap-get-fwd "US")
                       (funcall bimap-get-fwd "FR")
                       ;; Reverse lookup
                       (funcall bimap-get-rev "London")
                       (funcall bimap-get-rev "Berlin")
                       ;; Missing
                       (funcall bimap-get-fwd "JP")
                       (funcall bimap-get-rev "Tokyo"))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: grouping/pivoting with hash tables
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_hash_table_group_pivot() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Group records by department, compute per-dept stats
    let form = r#"(let ((people '((alice eng 90) (bob qa 85) (carol eng 95)
                                   (dave qa 80) (eve eng 88) (frank ops 92)))
                        (by-dept (make-hash-table)))
                    ;; Group by department
                    (dolist (p people)
                      (let ((name (nth 0 p))
                            (dept (nth 1 p))
                            (score (nth 2 p)))
                        (puthash dept
                                 (cons (cons name score)
                                       (gethash dept by-dept nil))
                                 by-dept)))
                    ;; Compute stats per department
                    (let ((stats nil))
                      (maphash
                       (lambda (dept members)
                         (let ((n (length members))
                               (total 0)
                               (best nil)
                               (best-score 0))
                           (dolist (m members)
                             (setq total (+ total (cdr m)))
                             (when (> (cdr m) best-score)
                               (setq best (car m)
                                     best-score (cdr m))))
                           (setq stats
                                 (cons (list dept n
                                             (/ (float total) n)
                                             best)
                                       stats))))
                       by-dept)
                      ;; Sort by department name
                      (sort stats
                            (lambda (a b)
                              (string< (symbol-name (car a))
                                       (symbol-name (car b)))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
