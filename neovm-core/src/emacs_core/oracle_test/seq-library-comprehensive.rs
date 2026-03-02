//! Oracle parity tests for seq.el library comprehensive coverage:
//! seq-elt, seq-length, seq-do, seq-map, seq-map-indexed, seq-filter,
//! seq-remove, seq-reduce, seq-find, seq-every-p, seq-some, seq-count,
//! seq-contains-p, seq-position, seq-uniq, seq-sort, seq-concatenate,
//! seq-partition, seq-group-by, seq-min, seq-max, seq-take, seq-drop,
//! seq-take-while, seq-drop-while, seq-into.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// seq-elt with various sequence types and edge cases
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_elt_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (list
    ;; seq-elt on lists at boundaries
    (seq-elt '(alpha beta gamma delta epsilon) 0)
    (seq-elt '(alpha beta gamma delta epsilon) 4)
    (seq-elt '(alpha beta gamma delta epsilon) 2)
    ;; seq-elt on vectors
    (seq-elt [100 200 300 400 500] 0)
    (seq-elt [100 200 300 400 500] 4)
    ;; seq-elt on strings
    (seq-elt "abcdefg" 0)
    (seq-elt "abcdefg" 6)
    ;; seq-elt on nested structures
    (seq-elt '((1 2) (3 4) (5 6)) 1)
    ;; seq-length on all types
    (seq-length '(a b c d e f))
    (seq-length [1 2 3 4 5 6 7 8 9 10])
    (seq-length "hello world")
    (seq-length nil)
    (seq-length [])
    (seq-length "")))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// seq-do side-effect iteration
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_do_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (let ((acc nil))
    (seq-do (lambda (x) (push (* x x) acc)) '(1 2 3 4 5))
    (let ((list-result (nreverse acc)))
      (setq acc nil)
      (seq-do (lambda (x) (push (+ x 10) acc)) [100 200 300])
      (let ((vec-result (nreverse acc)))
        (setq acc nil)
        (seq-do (lambda (c) (push (upcase c) acc)) "abc")
        (let ((str-result (nreverse acc)))
          (setq acc nil)
          (seq-do (lambda (x) (push x acc)) nil)
          (let ((empty-result acc))
            (list list-result vec-result str-result empty-result)))))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// seq-map-indexed with index tracking
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_map_indexed() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (list
    ;; map-indexed on list: pair each element with its index
    (seq-map-indexed (lambda (elt idx) (cons idx elt)) '(a b c d e))
    ;; map-indexed on vector
    (seq-map-indexed (lambda (elt idx) (+ elt (* idx 100))) [10 20 30 40])
    ;; map-indexed on string
    (seq-map-indexed (lambda (c idx) (list idx c)) "xyz")
    ;; map-indexed with computation
    (seq-map-indexed (lambda (elt idx) (* elt (1+ idx))) '(1 2 3 4 5))
    ;; map-indexed on empty
    (seq-map-indexed (lambda (elt idx) (cons idx elt)) nil)
    ;; map-indexed on single element
    (seq-map-indexed (lambda (elt idx) (list idx elt)) '(42))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// seq-contains-p and seq-position
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_contains_position() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (list
    ;; seq-contains-p basic
    (seq-contains-p '(1 2 3 4 5) 3)
    (seq-contains-p '(1 2 3 4 5) 99)
    (seq-contains-p nil 1)
    ;; seq-contains-p on vector
    (seq-contains-p [10 20 30 40] 30)
    (seq-contains-p [10 20 30 40] 99)
    ;; seq-contains-p on string
    (seq-contains-p "hello" ?l)
    (seq-contains-p "hello" ?z)
    ;; seq-contains-p with custom testfn
    (seq-contains-p '("Hello" "World") "hello"
                    (lambda (a b) (string= (downcase a) (downcase b))))
    ;; seq-position basic
    (seq-position '(a b c d e) 'c)
    (seq-position '(a b c d e) 'z)
    (seq-position '(10 20 30 40 50) 30)
    ;; seq-position on vector
    (seq-position [100 200 300 400] 300)
    (seq-position [100 200 300 400] 999)
    ;; seq-position with testfn
    (seq-position '(1 2 3 4 5 6) 4 #'>)
    ;; seq-position on string
    (seq-position "abcdef" ?d)
    (seq-position "abcdef" ?z)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// seq-concatenate across types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_concatenate() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (list
    ;; Concatenate lists into list
    (seq-concatenate 'list '(1 2 3) '(4 5 6))
    ;; Concatenate vectors into vector
    (seq-concatenate 'vector [1 2] [3 4] [5 6])
    ;; Concatenate strings
    (seq-concatenate 'string "hello" " " "world")
    ;; Concatenate mixed types into list
    (seq-concatenate 'list '(1 2) [3 4] '(5 6))
    ;; Concatenate mixed types into vector
    (seq-concatenate 'vector '(1 2 3) [4 5 6])
    ;; Concatenate with empty sequences
    (seq-concatenate 'list nil '(1 2) nil '(3 4) nil)
    ;; Concatenate single sequence
    (seq-concatenate 'list '(a b c))
    ;; Concatenate into string from char lists
    (seq-concatenate 'string '(?a ?b ?c) '(?d ?e ?f))
    ;; Three-way concatenation
    (seq-concatenate 'list '(1 2) '(3 4) '(5 6))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// seq-partition
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_partition() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (list
    ;; Partition into evens and odds
    (seq-partition #'cl-evenp '(1 2 3 4 5 6 7 8 9 10))
    ;; Partition by positive/non-positive
    (seq-partition (lambda (x) (> x 0)) '(-3 -2 -1 0 1 2 3))
    ;; Partition on vector
    (seq-partition (lambda (x) (> x 50)) [10 60 20 70 30 80 40 90])
    ;; Partition with all matching
    (seq-partition #'cl-evenp '(2 4 6 8))
    ;; Partition with none matching
    (seq-partition #'cl-evenp '(1 3 5 7))
    ;; Partition empty
    (seq-partition #'identity nil)
    ;; Partition on string (vowels vs consonants)
    (seq-partition (lambda (c) (memq c '(?a ?e ?i ?o ?u)))
                   "hello world")))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// seq-group-by
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_group_by() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (list
    ;; Group by even/odd
    (let ((groups (seq-group-by #'cl-evenp '(1 2 3 4 5 6 7 8))))
      (list (assoc t groups) (assoc nil groups)))
    ;; Group by first character of strings
    (seq-group-by (lambda (s) (aref s 0))
                  '("apple" "banana" "avocado" "blueberry" "cherry" "apricot"))
    ;; Group numbers by magnitude
    (seq-group-by (lambda (x) (cond ((< x 0) 'negative)
                                     ((= x 0) 'zero)
                                     (t 'positive)))
                  '(-5 -2 0 1 3 -1 0 7))
    ;; Group empty list
    (seq-group-by #'identity nil)
    ;; Group by modulo
    (seq-group-by (lambda (x) (% x 3)) '(1 2 3 4 5 6 7 8 9))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// seq-min and seq-max
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_min_max() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (list
    ;; seq-min on various lists
    (seq-min '(5 3 8 1 4 2 7 6))
    (seq-min '(42))
    (seq-min '(-10 -5 -20 -1))
    (seq-min '(1.5 0.5 2.5 0.1))
    ;; seq-max on various lists
    (seq-max '(5 3 8 1 4 2 7 6))
    (seq-max '(42))
    (seq-max '(-10 -5 -20 -1))
    (seq-max '(1.5 0.5 2.5 3.7))
    ;; seq-min/max on vectors
    (seq-min [100 50 200 75 150])
    (seq-max [100 50 200 75 150])
    ;; seq-min/max with negative and positive mix
    (seq-min '(-100 0 100 -50 50))
    (seq-max '(-100 0 100 -50 50))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// seq-take, seq-drop, seq-take-while, seq-drop-while
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_take_drop_while() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (list
    ;; seq-take
    (seq-take '(1 2 3 4 5 6 7 8) 3)
    (seq-take '(1 2 3 4 5) 0)
    (seq-take '(1 2 3) 10)
    (seq-take nil 5)
    (seq-take [10 20 30 40 50] 3)
    (seq-take "hello world" 5)
    ;; seq-drop
    (seq-drop '(1 2 3 4 5 6 7 8) 3)
    (seq-drop '(1 2 3 4 5) 0)
    (seq-drop '(1 2 3) 10)
    (seq-drop nil 5)
    (seq-drop [10 20 30 40 50] 2)
    (seq-drop "hello world" 6)
    ;; seq-take-while
    (seq-take-while (lambda (x) (< x 5)) '(1 2 3 4 5 6 7))
    (seq-take-while #'cl-evenp '(2 4 6 7 8 10))
    (seq-take-while #'identity '(t t t nil t))
    (seq-take-while (lambda (x) (< x 100)) nil)
    ;; seq-drop-while
    (seq-drop-while (lambda (x) (< x 5)) '(1 2 3 4 5 6 7))
    (seq-drop-while #'cl-evenp '(2 4 6 7 8 10))
    (seq-drop-while #'identity '(t t t nil t))
    (seq-drop-while (lambda (x) (< x 100)) nil)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// seq-into: type conversion
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_into() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (list
    ;; List to vector
    (seq-into '(1 2 3 4 5) 'vector)
    ;; Vector to list
    (seq-into [10 20 30] 'list)
    ;; String to list of chars
    (seq-into "hello" 'list)
    ;; String to vector of chars
    (seq-into "abc" 'vector)
    ;; List of chars to string
    (seq-into '(?h ?e ?l ?l ?o) 'string)
    ;; Vector of chars to string
    (seq-into [?w ?o ?r ?l ?d] 'string)
    ;; Empty conversions
    (seq-into nil 'vector)
    (seq-into [] 'list)
    (seq-into "" 'list)
    ;; Identity conversions
    (seq-into '(1 2 3) 'list)
    (seq-into [1 2 3] 'vector)
    (seq-into "abc" 'string)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: chaining seq operations for data transformation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_chained_transformations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (let ((data '(34 67 23 89 12 56 78 45 91 15 38 72)))
    (list
      ;; Take top 5 after sorting descending
      (seq-take (seq-sort #'> data) 5)
      ;; Drop the smallest 3 after sorting ascending
      (seq-drop (seq-sort #'< data) 3)
      ;; Filter evens, map to squares, take first 3
      (seq-take (seq-map (lambda (x) (* x x))
                         (seq-filter #'cl-evenp data))
                3)
      ;; Group by decade, count per group
      (seq-map (lambda (group) (cons (car group) (length (cdr group))))
               (seq-group-by (lambda (x) (* 10 (/ x 10))) data))
      ;; Partition by median-like threshold, count each side
      (let ((parts (seq-partition (lambda (x) (> x 50)) data)))
        (list (length (car parts)) (length (cadr parts))))
      ;; Reduce to running max
      (let ((running nil))
        (seq-reduce (lambda (acc x)
                      (let ((new-max (if acc (max acc x) x)))
                        (push new-max running)
                        new-max))
                    data nil)
        (nreverse running)))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: seq functions on deeply nested data
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_lib_nested_data_operations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (require 'seq)
  (let ((students '((:name "Alice"   :grade 92 :subject "math")
                    (:name "Bob"     :grade 85 :subject "science")
                    (:name "Carol"   :grade 78 :subject "math")
                    (:name "Dave"    :grade 95 :subject "science")
                    (:name "Eve"     :grade 88 :subject "math")
                    (:name "Frank"   :grade 72 :subject "science")
                    (:name "Grace"   :grade 91 :subject "math")
                    (:name "Hank"    :grade 67 :subject "science"))))
    (let ((get-name  (lambda (s) (plist-get s :name)))
          (get-grade (lambda (s) (plist-get s :grade)))
          (get-subj  (lambda (s) (plist-get s :subject))))
      (list
        ;; Names of students with grade > 85 sorted
        (seq-sort #'string<
                  (seq-map get-name
                           (seq-filter (lambda (s) (> (plist-get s :grade) 85))
                                       students)))
        ;; Average grade per subject
        (seq-map (lambda (group)
                   (let* ((subj (car group))
                          (entries (cdr group))
                          (grades (seq-map (lambda (s) (plist-get s :grade)) entries))
                          (total (seq-reduce #'+ grades 0)))
                     (list subj (/ total (length entries)))))
                 (seq-group-by get-subj students))
        ;; Count per subject
        (seq-map (lambda (group)
                   (cons (car group) (length (cdr group))))
                 (seq-group-by get-subj students))
        ;; Highest grade
        (seq-max (seq-map get-grade students))
        ;; Lowest grade
        (seq-min (seq-map get-grade students))
        ;; Position of first student with grade > 90
        (seq-position students 90
                      (lambda (s threshold) (> (plist-get s :grade) threshold)))
        ;; Do all students have grade > 60?
        (seq-every-p (lambda (s) (> (plist-get s :grade) 60)) students)
        ;; Any student with grade = 100?
        (seq-some (lambda (s) (= (plist-get s :grade) 100)) students)))))"#;
    assert_oracle_parity(form);
}
