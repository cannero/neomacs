//! Oracle parity tests for advanced sort patterns: string predicates,
//! multi-field comparators, stability verification, nested structure sorting,
//! destructive mutation, descending order, Elisp merge sort, and group-by.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// sort with string< predicate
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_sort_alg_string_less_predicate() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(sort (list "zebra" "apple" "mango" "banana" "cherry" "apricot" "avocado")
                        'string<)"#;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_sort_alg_string_less_case_sensitivity() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // string< is case-sensitive: uppercase < lowercase in ASCII
    let form = r#"(sort (list "banana" "Apple" "cherry" "Banana" "apple" "Cherry")
                        'string<)"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Multi-field comparator
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_sort_alg_multi_field_comparator() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Sort by department (string), then by salary (descending), then by name
    let form = r#"(let ((employees
                    (list '("engineering" 90000 "Carol")
                          '("engineering" 120000 "Alice")
                          '("sales" 80000 "Eve")
                          '("engineering" 90000 "Bob")
                          '("sales" 80000 "Dave")
                          '("sales" 95000 "Frank"))))
                  (sort employees
                        (lambda (a b)
                          (cond
                           ;; Primary: department ascending
                           ((string< (nth 0 a) (nth 0 b)) t)
                           ((string< (nth 0 b) (nth 0 a)) nil)
                           ;; Secondary: salary descending
                           ((> (nth 1 a) (nth 1 b)) t)
                           ((< (nth 1 a) (nth 1 b)) nil)
                           ;; Tertiary: name ascending
                           (t (string< (nth 2 a) (nth 2 b)))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Stability verification
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_sort_alg_stability_indexed() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Pair each element with its original index, sort by value,
    // then verify oracle and neovm agree on tie-breaking order
    let form = "(let ((data '((3 . 0) (1 . 1) (4 . 2) (1 . 3) (5 . 4)
                               (9 . 5) (2 . 6) (6 . 7) (5 . 8) (3 . 9))))
                  (sort (copy-sequence data)
                        (lambda (a b) (< (car a) (car b)))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_sort_alg_stability_string_groups() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Sort words by first character only; words starting with same char
    // should maintain relative order if the sort is stable
    let form = r#"(sort (list "cat" "apple" "ant" "banana" "cherry" "avocado" "blueberry")
                        (lambda (a b)
                          (< (aref a 0) (aref b 0))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Sort nested structures (alists by value)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_sort_alg_alist_by_nested_value() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Sort alist of (name . (score . grade)) by score then grade
    let form = r#"(let ((students
                    '((alice . (85 . b))
                      (bob . (92 . a))
                      (carol . (85 . a))
                      (dave . (78 . c))
                      (eve . (92 . b)))))
                  ;; Sort by score descending, then grade ascending
                  (sort (copy-sequence students)
                        (lambda (x y)
                          (let ((sx (cadr x)) (sy (cadr y))
                                (gx (cddr x)) (gy (cddr y)))
                            (cond
                             ((> sx sy) t)
                             ((< sx sy) nil)
                             (t (string< (symbol-name gx) (symbol-name gy))))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Sort is destructive: verify original list is modified
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_sort_alg_destructive_mutation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // sort destructively modifies its input; we test the result pointer
    // and that the sorted result is correct
    let form = "(let* ((original (list 5 3 1 4 2))
                       (sorted (sort original '<)))
                  (list
                    ;; sorted result is correct
                    (equal sorted '(1 2 3 4 5))
                    ;; length of sorted
                    (length sorted)
                    ;; original may have been mutated (first cons cell may point elsewhere)
                    ;; but sorted is definitely correct
                    sorted))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// sort + nreverse for descending order
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_sort_alg_ascending_then_reverse() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((data (list 3 1 4 1 5 9 2 6 5 3 5)))
                  (nreverse (sort data '<)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: merge sort implemented in Elisp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_sort_alg_elisp_merge_sort() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Implement merge sort from scratch and verify it matches built-in sort
    let form = "(progn
  (fset 'neovm--test-merge
    (lambda (a b pred)
      (cond
       ((null a) b)
       ((null b) a)
       ((funcall pred (car a) (car b))
        (cons (car a) (funcall 'neovm--test-merge (cdr a) b pred)))
       (t
        (cons (car b) (funcall 'neovm--test-merge a (cdr b) pred))))))

  (fset 'neovm--test-msort
    (lambda (lst pred)
      (if (or (null lst) (null (cdr lst)))
          lst
        (let* ((mid (/ (length lst) 2))
               (left nil) (right nil) (i 0))
          (dolist (x lst)
            (if (< i mid)
                (setq left (cons x left))
              (setq right (cons x right)))
            (setq i (1+ i)))
          (funcall 'neovm--test-merge
                   (funcall 'neovm--test-msort (nreverse left) pred)
                   (funcall 'neovm--test-msort (nreverse right) pred)
                   pred)))))

  (unwind-protect
      (let* ((data '(38 27 43 3 9 82 10 15 42 7 99 1))
             (my-sorted (funcall 'neovm--test-msort (copy-sequence data) '<))
             (builtin-sorted (sort (copy-sequence data) '<)))
        (list
          my-sorted
          builtin-sorted
          (equal my-sorted builtin-sorted)))
    (fmakunbound 'neovm--test-merge)
    (fmakunbound 'neovm--test-msort)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: sort-based group-by (consecutive equal elements)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_sort_alg_group_by_consecutive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Sort then group consecutive equal elements
    let form = "(let ((group-consecutive
                       (lambda (lst key-fn)
                         (if (null lst) nil
                           (let ((groups nil)
                                 (current-key (funcall key-fn (car lst)))
                                 (current-group (list (car lst))))
                             (dolist (x (cdr lst))
                               (let ((k (funcall key-fn x)))
                                 (if (equal k current-key)
                                     (setq current-group (cons x current-group))
                                   (setq groups (cons (cons current-key (nreverse current-group)) groups))
                                   (setq current-key k)
                                   (setq current-group (list x)))))
                             (setq groups (cons (cons current-key (nreverse current-group)) groups))
                             (nreverse groups))))))
                  ;; Sort numbers by (mod n 3), then group
                  (let* ((data '(9 1 7 2 8 3 5 4 6 10))
                         (sorted (sort (copy-sequence data)
                                       (lambda (a b)
                                         (< (mod a 3) (mod b 3)))))
                         (grouped (funcall group-consecutive sorted
                                          (lambda (x) (mod x 3)))))
                    (list sorted grouped)))";
    assert_oracle_parity_with_bootstrap(form);
}
