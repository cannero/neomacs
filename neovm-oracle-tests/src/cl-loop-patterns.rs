//! Oracle parity tests for `cl-loop` macro patterns:
//! `for ... in`, `for ... from ... to`, `collect`, `sum`, `count`,
//! `maximize`/`minimize`, `when`/`unless`, `do`, `finally`, `with`,
//! `append`, `nconc`, multiple `for` clauses, `for ... on`,
//! `for ... across`, `for ... being hash-keys/hash-values`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// Basic for...in with collect, sum, count
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cl_loop_basic_for_in_collect_sum_count() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'cl-lib)
  (list
    ;; collect: square each element
    (cl-loop for x in '(1 2 3 4 5) collect (* x x))
    ;; sum: total of elements
    (cl-loop for x in '(10 20 30 40 50) sum x)
    ;; count: how many are positive
    (cl-loop for x in '(-3 -1 0 2 5 -4 7) count (> x 0))
    ;; collect with when filter
    (cl-loop for x in '(1 2 3 4 5 6 7 8 9 10)
             when (= (% x 2) 0)
             collect x)
    ;; sum only odd elements
    (cl-loop for x in '(1 2 3 4 5 6 7 8 9 10)
             when (= (% x 2) 1)
             sum x)
    ;; count with unless
    (cl-loop for x in '("apple" "banana" "" "cherry" "" "date")
             unless (string= x "")
             count t)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// for...from...to with by, maximize, minimize
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cl_loop_for_from_to_maximize_minimize() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'cl-lib)
  (list
    ;; Basic range collect
    (cl-loop for i from 1 to 10 collect i)
    ;; Range with step
    (cl-loop for i from 0 to 20 by 3 collect i)
    ;; Downward range
    (cl-loop for i from 10 downto 1 collect i)
    ;; Downward by step
    (cl-loop for i from 100 downto 0 by 25 collect i)
    ;; Maximize
    (cl-loop for x in '(3 1 4 1 5 9 2 6 5 3 5) maximize x)
    ;; Minimize
    (cl-loop for x in '(3 1 4 1 5 9 2 6 5 3 5) minimize x)
    ;; Maximize of computed value
    (cl-loop for x in '(-5 -2 3 -1 4 -3)
             maximize (abs x))
    ;; Sum of range with filter
    (cl-loop for i from 1 to 100
             when (= (% i 3) 0)
             sum i)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Multiple for clauses, with, do, finally
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cl_loop_multiple_for_with_do_finally() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'cl-lib)
  (list
    ;; Two parallel for clauses (zip)
    (cl-loop for x in '(a b c d)
             for y in '(1 2 3 4)
             collect (list x y))
    ;; Parallel for clauses of different lengths (stops at shorter)
    (cl-loop for x in '(a b c d e f)
             for y in '(1 2 3)
             collect (cons x y))
    ;; for with index
    (cl-loop for x in '(alpha beta gamma delta)
             for i from 0
             collect (list i x))
    ;; with clause (local variable)
    (cl-loop with total = 0
             for x in '(1 2 3 4 5)
             do (setq total (+ total (* x x)))
             finally return total)
    ;; with and collect
    (cl-loop with factor = 10
             for x in '(1 2 3 4 5)
             collect (* x factor))
    ;; do with side-effect accumulation
    (cl-loop with result = nil
             for x in '(a b c d e)
             for i from 1
             do (when (= (% i 2) 1)
                  (push (cons x i) result))
             finally return (nreverse result))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// for...on (iterate over cdrs), append, nconc
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cl_loop_for_on_append_nconc() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'cl-lib)
  (list
    ;; for...on: iterate over successive cdrs
    (cl-loop for tail on '(1 2 3 4 5) collect tail)
    ;; for...on: collect car of each tail
    (cl-loop for tail on '(a b c d) collect (car tail))
    ;; for...on: pairwise comparison
    (cl-loop for tail on '(1 3 5 7 9)
             when (cdr tail)
             collect (list (car tail) (cadr tail) (- (cadr tail) (car tail))))
    ;; append: flatten lists
    (cl-loop for x in '((1 2) (3 4) (5 6) (7 8))
             append x)
    ;; append with filter
    (cl-loop for x in '((1 2 3) (4 5 6) (7 8 9))
             when (> (car x) 3)
             append x)
    ;; nconc: like append but destructive (on fresh lists it's the same)
    (cl-loop for x in '((a b) (c d) (e f))
             nconc (copy-sequence x))
    ;; Combine append with transformation
    (cl-loop for x in '(1 2 3 4)
             append (list x (* x 10) (* x 100)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// for...across (vectors/strings)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cl_loop_for_across() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'cl-lib)
  (list
    ;; Iterate over vector
    (cl-loop for x across [10 20 30 40 50] collect x)
    ;; Iterate over vector with index
    (cl-loop for x across [a b c d e]
             for i from 0
             collect (list i x))
    ;; Sum over vector
    (cl-loop for x across [1 2 3 4 5 6 7 8 9 10] sum x)
    ;; Iterate over string (chars)
    (cl-loop for ch across "hello"
             collect ch)
    ;; Count vowels in string
    (cl-loop for ch across "the quick brown fox jumps"
             count (memq ch '(?a ?e ?i ?o ?u)))
    ;; Collect uppercase chars from string
    (cl-loop for ch across "Hello World 123"
             when (and (>= ch ?A) (<= ch ?Z))
             collect ch)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// for...being hash-keys / hash-values
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cl_loop_hash_table_iteration() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'cl-lib)
  (let ((ht (make-hash-table :test 'equal)))
    (puthash "alice" 30 ht)
    (puthash "bob" 25 ht)
    (puthash "charlie" 35 ht)
    (puthash "diana" 28 ht)
    (list
      ;; Collect all keys (sorted for determinism)
      (sort (cl-loop for k being the hash-keys of ht collect k) #'string<)
      ;; Collect all values (sorted)
      (sort (cl-loop for v being the hash-values of ht collect v) #'<)
      ;; Collect key-value pairs
      (sort (cl-loop for k being the hash-keys of ht using (hash-values v)
                     collect (cons k v))
            (lambda (a b) (string< (car a) (car b))))
      ;; Sum all values
      (cl-loop for v being the hash-values of ht sum v)
      ;; Count entries matching a predicate
      (cl-loop for v being the hash-values of ht count (>= v 30))
      ;; Collect keys where value > 27
      (sort (cl-loop for k being the hash-keys of ht using (hash-values v)
                     when (> v 27)
                     collect k)
            #'string<))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: combined patterns (real-world-like queries)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cl_loop_combined_complex_patterns() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'cl-lib)
  (let ((students '(("Alice" . ((math . 95) (sci . 88) (eng . 92)))
                    ("Bob" . ((math . 72) (sci . 85) (eng . 78)))
                    ("Charlie" . ((math . 88) (sci . 92) (eng . 90)))
                    ("Diana" . ((math . 65) (sci . 70) (eng . 85)))
                    ("Eve" . ((math . 98) (sci . 95) (eng . 97))))))
    (list
      ;; Compute average score per student
      (cl-loop for student in students
               collect
               (let* ((name (car student))
                      (scores (cdr student))
                      (total (cl-loop for pair in scores sum (cdr pair)))
                      (avg (/ total (length scores))))
                 (list name avg)))
      ;; Find students with all scores >= 80
      (cl-loop for student in students
               when (cl-loop for pair in (cdr student)
                             always (>= (cdr pair) 80))
               collect (car student))
      ;; Maximize: highest single score across all students
      (cl-loop for student in students
               maximize
               (cl-loop for pair in (cdr student) maximize (cdr pair)))
      ;; Collect all (name subject score) triples where score >= 90
      (cl-loop for student in students
               append
               (cl-loop for pair in (cdr student)
                        when (>= (cdr pair) 90)
                        collect (list (car student) (car pair) (cdr pair))))
      ;; Count total number of scores across all students
      (cl-loop for student in students
               sum (length (cdr student)))
      ;; Top scorer per subject
      (cl-loop for subj in '(math sci eng)
               collect
               (cons subj
                     (car (cl-loop for student in students
                                   with best-name = nil
                                   with best-score = -1
                                   do (let ((score (cdr (assq subj (cdr student)))))
                                        (when (> score best-score)
                                          (setq best-name (car student)
                                                best-score score)))
                                   finally return (list best-name best-score))))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Edge cases and special patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_cl_loop_edge_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'cl-lib)
  (list
    ;; Empty list
    (cl-loop for x in nil collect x)
    ;; Empty vector
    (cl-loop for x across [] collect x)
    ;; Single element
    (cl-loop for x in '(42) collect (* x 2))
    ;; from > to with positive step: no iterations
    (cl-loop for i from 10 to 5 collect i)
    ;; Collect with multiple values accumulated
    (cl-loop for x in '(1 2 3 4 5 6)
             if (= (% x 2) 0)
               collect x into evens
             else
               collect x into odds
             end
             finally return (list evens odds))
    ;; Destructuring in for...in
    (cl-loop for (key . val) in '((a . 1) (b . 2) (c . 3))
             collect (list val key))
    ;; Repeat clause
    (cl-loop repeat 5 collect 'x)
    ;; Thereis: return first match
    (cl-loop for x in '(1 3 5 6 7 8)
             thereis (and (= (% x 2) 0) x))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
