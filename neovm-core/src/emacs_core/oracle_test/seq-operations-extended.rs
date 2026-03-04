//! Extended oracle parity tests for `seq.el` operations:
//! seq-map-indexed, seq-do, seq-let, seq-into, seq-concatenate,
//! seq-mapcat, seq-sort-by, seq-group-by, seq-min, seq-max,
//! seq-position, seq-contains-p, seq-difference, seq-intersection,
//! seq-subseq. Tests with lists, vectors, and strings.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::assert_oracle_parity_with_bootstrap;

// ---------------------------------------------------------------------------
// seq-map-indexed and seq-do with side-effects
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_ext_map_indexed_and_do() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // seq-map-indexed passes index as second arg.
    // seq-do is for side-effects only (returns nil).
    let form = r#"(let ((log nil))
  (list
    ;; seq-map-indexed on a list: (element . index) pairs
    (seq-map-indexed (lambda (elt idx) (cons elt idx)) '(a b c d e))
    ;; seq-map-indexed on a vector
    (seq-map-indexed (lambda (elt idx) (list idx (* elt elt))) [3 5 7 9])
    ;; seq-map-indexed on a string: (char . index)
    (seq-map-indexed (lambda (ch idx) (cons (char-to-string ch) idx)) "hello")
    ;; seq-do: accumulate side effects, returns nil
    (progn
      (seq-do (lambda (x) (setq log (cons (* x x) log))) '(1 2 3 4 5))
      (list (nreverse log)))
    ;; seq-map-indexed with empty sequence
    (seq-map-indexed (lambda (e i) (cons e i)) nil)
    ;; seq-map-indexed building a hash-like alist from vector
    (seq-map-indexed
      (lambda (name idx)
        (list :id idx :name name))
      ["alice" "bob" "carol"])))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// seq-let: destructure sequences
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_ext_let_destructuring() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // seq-let binds variables to elements of a sequence.
    let form = r#"(list
  ;; Basic list destructuring
  (seq-let (a b c) '(1 2 3)
    (+ a b c))
  ;; Destructure vector
  (seq-let (x y z) [10 20 30]
    (* x (+ y z)))
  ;; Destructure string (chars)
  (seq-let (a b c) "xyz"
    (list a b c))
  ;; More elements than bindings: extras ignored
  (seq-let (first second) '(10 20 30 40 50)
    (+ first second))
  ;; Fewer elements than bindings: extra vars are nil
  (seq-let (a b c d e) '(1 2)
    (list a b c d e))
  ;; Nested computation with destructured values
  (seq-let (op x y) '(+ 10 20)
    (cond
      ((eq op '+) (+ x y))
      ((eq op '-) (- x y))
      ((eq op '*) (* x y))
      (t nil)))
  ;; Empty sequence
  (seq-let (a b) nil
    (list a b)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// seq-into and seq-concatenate: type conversion
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_ext_into_and_concatenate() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // seq-into converts a sequence to a specified type.
    // seq-concatenate concatenates sequences into a target type.
    let form = r#"(list
  ;; seq-into: list -> vector
  (seq-into '(1 2 3 4 5) 'vector)
  ;; seq-into: vector -> list
  (seq-into [10 20 30] 'list)
  ;; seq-into: string -> list (chars)
  (seq-into "hello" 'list)
  ;; seq-into: list of chars -> string
  (seq-into '(?h ?e ?l ?l ?o) 'string)
  ;; seq-concatenate: merge multiple sequences into a list
  (seq-concatenate 'list '(1 2) [3 4] '(5 6))
  ;; seq-concatenate: merge into vector
  (seq-concatenate 'vector '(1 2 3) [4 5 6])
  ;; seq-concatenate: merge into string
  (seq-concatenate 'string "hello" " " "world")
  ;; seq-into with empty
  (seq-into nil 'vector)
  ;; seq-concatenate with mixed types into list
  (seq-concatenate 'list "ab" [99 100] '(1 2)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// seq-position and seq-contains-p
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_ext_position_and_contains() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // seq-position returns the index of an element.
    // seq-contains-p checks if an element exists.
    let form = r#"(list
  ;; seq-position: find element in list
  (seq-position '(a b c d e) 'c)
  ;; seq-position: not found returns nil
  (seq-position '(a b c) 'z)
  ;; seq-position: first occurrence
  (seq-position '(1 2 3 2 1) 2)
  ;; seq-position in vector
  (seq-position [10 20 30 40] 30)
  ;; seq-position with custom test function
  (seq-position '("apple" "banana" "cherry") "BANANA"
    (lambda (a b) (string= (downcase a) (downcase b))))
  ;; seq-contains-p: element exists
  (seq-contains-p '(1 2 3 4 5) 3)
  ;; seq-contains-p: element missing
  (seq-contains-p '(1 2 3 4 5) 99)
  ;; seq-contains-p on vector
  (seq-contains-p [a b c d] 'c)
  ;; seq-contains-p on string (char)
  (seq-contains-p "hello" ?l)
  ;; seq-position at boundaries
  (list
    (seq-position '(10 20 30) 10)
    (seq-position '(10 20 30) 30)
    (seq-position nil 1)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// seq-difference, seq-intersection
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_ext_set_operations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // seq-difference: elements in first but not second.
    // seq-intersection: elements in both.
    let form = r#"(list
  ;; seq-difference: basic
  (seq-sort #'< (seq-difference '(1 2 3 4 5) '(2 4 6)))
  ;; seq-intersection: basic
  (seq-sort #'< (seq-intersection '(1 2 3 4 5) '(2 4 6 8)))
  ;; With vectors
  (seq-sort #'< (seq-difference [10 20 30 40] [20 40 50]))
  (seq-sort #'< (seq-intersection [10 20 30 40] [20 40 50]))
  ;; Empty cases
  (seq-difference '(1 2 3) nil)
  (seq-difference nil '(1 2 3))
  (seq-intersection '(1 2 3) nil)
  ;; Identical sets
  (seq-sort #'< (seq-intersection '(1 2 3) '(3 2 1)))
  (seq-difference '(1 2 3) '(3 2 1))
  ;; Custom test function: case-insensitive strings
  (let ((strs1 '("Apple" "Banana" "Cherry"))
        (strs2 '("banana" "date" "cherry")))
    (list
      (seq-difference strs1 strs2
        (lambda (a b) (string= (downcase a) (downcase b))))
      (seq-intersection strs1 strs2
        (lambda (a b) (string= (downcase a) (downcase b)))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// seq-subseq with negative indices and all types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_ext_subseq_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // seq-subseq: comprehensive tests with lists, vectors, strings,
    // negative indices, boundary cases.
    let form = r#"(list
  ;; List: normal range
  (seq-subseq '(a b c d e f g) 2 5)
  ;; List: from start
  (seq-subseq '(a b c d e) 0 3)
  ;; List: to end
  (seq-subseq '(a b c d e) 3)
  ;; List: negative start (from end)
  (seq-subseq '(a b c d e f) -3)
  ;; List: negative start and end
  (seq-subseq '(a b c d e f) -4 -1)
  ;; Vector
  (seq-subseq [1 2 3 4 5 6 7 8 9 10] 3 7)
  ;; Vector: negative
  (seq-subseq [1 2 3 4 5] -2)
  ;; String
  (seq-subseq "hello world" 6)
  (seq-subseq "hello world" 0 5)
  (seq-subseq "abcdef" -3 -1)
  ;; Empty result
  (seq-subseq '(1 2 3) 1 1)
  ;; Full copy
  (seq-subseq '(1 2 3) 0)
  ;; Single element
  (seq-subseq '(a b c d e) 2 3))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: multi-step data pipeline with seq operations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_ext_pipeline_word_frequency() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Word frequency analysis pipeline using seq operations.
    // 1. Split text into words, 2. group by word, 3. count per group,
    // 4. sort by frequency descending, 5. extract top-N, 6. compute stats.
    let form = r#"(let* ((words '("the" "cat" "sat" "on" "the" "mat"
                          "the" "cat" "on" "the" "mat" "sat"
                          "on" "the" "dog" "sat" "the" "cat"))
        ;; Step 1: Group by identity (word)
        (grouped (seq-group-by #'identity words))
        ;; Step 2: Build (word . count) alist
        (freqs (seq-map (lambda (g) (cons (car g) (length (cdr g))))
                        grouped))
        ;; Step 3: Sort by frequency descending
        (sorted (seq-sort-by #'cdr (lambda (a b) (> a b)) freqs))
        ;; Step 4: Top 3 most frequent
        (top3 (seq-subseq sorted 0 (min 3 (length sorted))))
        ;; Step 5: Total words
        (total (seq-reduce (lambda (acc pair) (+ acc (cdr pair))) freqs 0))
        ;; Step 6: Unique count
        (unique-count (length freqs))
        ;; Step 7: Words appearing more than twice
        (frequent (seq-filter (lambda (pair) (> (cdr pair) 2)) freqs))
        ;; Step 8: Words appearing exactly once
        (hapax (seq-filter (lambda (pair) (= (cdr pair) 1)) freqs)))
  (list
    ;; Top 3 words
    (seq-map #'car top3)
    ;; Their counts
    (seq-map #'cdr top3)
    ;; Total word count
    total
    ;; Unique word count
    unique-count
    ;; Frequent words (sorted)
    (seq-sort-by #'car (lambda (a b) (string-lessp (symbol-name a) (symbol-name b)))
                 frequent)
    ;; Hapax legomena (words appearing once)
    (seq-map #'car hapax)))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: seq-reduce building complex structures
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_seq_ext_reduce_complex_accumulations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use seq-reduce to build complex structures from sequences:
    // running stats, nested transformations, transposing.
    let form = r#"(let* ((data '(4 7 2 9 1 8 3 6 5 10))
        ;; Running min, max, sum, count
        (stats (seq-reduce
                (lambda (acc x)
                  (list (min (nth 0 acc) x)
                        (max (nth 1 acc) x)
                        (+ (nth 2 acc) x)
                        (1+ (nth 3 acc))))
                (cdr data)
                (list (car data) (car data) (car data) 1)))
        ;; Build histogram: count occurrences of (x mod 3)
        (histogram
          (let ((h (seq-reduce
                    (lambda (acc x)
                      (let* ((key (% x 3))
                             (existing (assq key acc)))
                        (if existing
                            (progn (setcdr existing (1+ (cdr existing))) acc)
                          (cons (cons key 1) acc))))
                    data nil)))
            (sort h (lambda (a b) (< (car a) (car b))))))
        ;; Partition into runs of ascending values
        (runs (let ((result (seq-reduce
                             (lambda (acc x)
                               (let ((current-run (car acc))
                                     (finished (cdr acc)))
                                 (if (or (null current-run)
                                         (>= x (car (last current-run))))
                                     (cons (append current-run (list x)) finished)
                                   (cons (list x) (cons current-run finished)))))
                             data
                             '(nil))))
                (nreverse (cons (car result) (cdr result)))))
        ;; Zip two sequences using seq-mapn then reduce to alist
        (pairs (seq-mapn #'cons '(a b c d) '(1 2 3 4))))
  (list
    ;; Stats: (min max sum count)
    stats
    ;; Mean (integer division)
    (/ (nth 2 stats) (nth 3 stats))
    ;; Histogram of x mod 3
    histogram
    ;; Ascending runs
    runs
    ;; Zipped pairs
    pairs))"#;
    assert_oracle_parity_with_bootstrap(form);
}
