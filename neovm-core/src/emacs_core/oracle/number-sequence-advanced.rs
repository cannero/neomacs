//! Advanced oracle parity tests for `number-sequence` with ALL parameter
//! combinations: FROM only, FROM+TO, FROM+TO+INCR (positive, negative, float),
//! edge cases (FROM=TO, zero step, non-divisible ranges), and complex
//! compositions with mapcar and filtering.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// FROM only (single-argument): should produce list of one element
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_from_only() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // When TO is nil, number-sequence returns a list containing just FROM
    assert_oracle_parity("(number-sequence 5)");
    assert_oracle_parity("(number-sequence 0)");
    assert_oracle_parity("(number-sequence -7)");
    assert_oracle_parity("(number-sequence 999)");
    // Float FROM, no TO
    assert_oracle_parity("(number-sequence 3.14)");
    assert_oracle_parity("(number-sequence -2.5)");
}

// ---------------------------------------------------------------------------
// FROM and TO ascending range (default INCR = 1)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_from_to_ascending() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(number-sequence 1 5)");
    assert_oracle_parity("(number-sequence 0 10)");
    assert_oracle_parity("(number-sequence -3 3)");
    assert_oracle_parity("(number-sequence -10 -5)");
    // Large range
    assert_oracle_parity("(length (number-sequence 1 200))");
    // Verify first and last elements of a range
    assert_oracle_parity(
        "(let ((s (number-sequence 50 75)))
           (list (car s) (car (last s)) (length s)))",
    );
}

// ---------------------------------------------------------------------------
// FROM, TO, and positive INCR
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_positive_incr() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(number-sequence 0 20 5)");
    assert_oracle_parity("(number-sequence 1 15 3)");
    assert_oracle_parity("(number-sequence 10 100 10)");
    assert_oracle_parity("(number-sequence -20 20 7)");
    // INCR larger than range: only FROM included
    assert_oracle_parity("(number-sequence 1 5 100)");
    // INCR = 1 (same as default)
    assert_oracle_parity("(number-sequence 3 8 1)");
    // INCR = 2 (evens)
    assert_oracle_parity("(number-sequence 0 20 2)");
}

// ---------------------------------------------------------------------------
// FROM > TO with negative INCR (descending)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_descending_negative_incr() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(number-sequence 10 1 -1)");
    assert_oracle_parity("(number-sequence 100 0 -10)");
    assert_oracle_parity("(number-sequence 50 -50 -25)");
    assert_oracle_parity("(number-sequence 5 -5 -3)");
    assert_oracle_parity("(number-sequence 0 -20 -4)");
    // Verify elements
    assert_oracle_parity(
        "(let ((s (number-sequence 20 5 -3)))
           (list (car s) (car (last s)) (length s)))",
    );
}

// ---------------------------------------------------------------------------
// Float arguments: FROM, TO, INCR as floats
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_float_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(number-sequence 0.0 1.0 0.25)");
    assert_oracle_parity("(number-sequence 1.0 3.0 0.5)");
    assert_oracle_parity("(number-sequence -1.0 1.0 0.5)");
    // Mixed int and float
    assert_oracle_parity("(number-sequence 0 1.0 0.2)");
    assert_oracle_parity("(number-sequence 0.0 5 1)");
    // Descending floats
    assert_oracle_parity("(number-sequence 2.0 0.0 -0.5)");
    assert_oracle_parity("(number-sequence 1.0 -1.0 -0.25)");
    // Verify length to sidestep float precision
    assert_oracle_parity("(length (number-sequence 0.0 10.0 0.1))");
    assert_oracle_parity("(length (number-sequence 0.0 1.0 0.3))");
}

// ---------------------------------------------------------------------------
// INCR that doesn't evenly divide range
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_non_divisible_incr() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // 1 to 10 by 3: 1, 4, 7, 10 (10 is included since 7+3=10)
    assert_oracle_parity("(number-sequence 1 10 3)");
    // 1 to 11 by 3: 1, 4, 7, 10 (10+3=13 > 11, so stops at 10)
    assert_oracle_parity("(number-sequence 1 11 3)");
    // 0 to 7 by 3: 0, 3, 6
    assert_oracle_parity("(number-sequence 0 7 3)");
    // Large step relative to range
    assert_oracle_parity("(number-sequence 0 100 33)");
    // Descending non-divisible
    assert_oracle_parity("(number-sequence 10 1 -3)");
    assert_oracle_parity("(number-sequence 100 0 -33)");
    // Float non-divisible
    assert_oracle_parity("(length (number-sequence 0.0 1.0 0.3))");
    assert_oracle_parity("(length (number-sequence 0.0 1.0 0.7))");
}

// ---------------------------------------------------------------------------
// Edge cases: FROM=TO, INCR=0 (should signal error), negative numbers
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_edge_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // FROM = TO: single element regardless of INCR
    assert_oracle_parity("(number-sequence 7 7)");
    assert_oracle_parity("(number-sequence 0 0)");
    assert_oracle_parity("(number-sequence -3 -3)");
    assert_oracle_parity("(number-sequence 42 42 5)");
    assert_oracle_parity("(number-sequence 42 42 -5)");
    assert_oracle_parity("(number-sequence 42 42 0)");

    // INCR = 0 with FROM != TO should signal an error
    let (oracle, neovm) = eval_oracle_and_neovm(
        "(condition-case err
           (number-sequence 1 10 0)
           (error (list 'error (car err))))",
    );
    assert_ok_eq(&oracle, &neovm);

    // FROM > TO with positive step: nil (empty)
    assert_oracle_parity("(number-sequence 10 1 2)");
    assert_oracle_parity("(number-sequence 5 3 1)");

    // FROM < TO with negative step: nil (empty)
    assert_oracle_parity("(number-sequence 1 10 -1)");
    assert_oracle_parity("(number-sequence -5 5 -2)");

    // All negative numbers
    assert_oracle_parity("(number-sequence -10 -1)");
    assert_oracle_parity("(number-sequence -1 -10 -1)");
    assert_oracle_parity("(number-sequence -100 -50 7)");
}

// ---------------------------------------------------------------------------
// Complex: arithmetic progressions with filtering
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_arithmetic_progressions_filtered() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Generate arithmetic progressions, then filter for various properties
    let form = r#"(let* ((seq1 (number-sequence 1 100 3))    ;; 1, 4, 7, ..., 100
                         (seq2 (number-sequence 2 100 5))    ;; 2, 7, 12, ..., 97
                         ;; Filter seq1 for elements also in seq2 (intersection)
                         (common nil))
                    (dolist (x seq1)
                      (when (memq x seq2)
                        (setq common (cons x common))))
                    (let* ((common-sorted (sort (nreverse common) #'<))
                           ;; Sum of elements in each progression
                           (sum1 (apply #'+ seq1))
                           (sum2 (apply #'+ seq2))
                           ;; Filter for primes in short range
                           (small (number-sequence 2 30))
                           (primes (let ((result nil))
                                     (dolist (n small)
                                       (let ((is-prime t) (d 2))
                                         (while (and is-prime (<= (* d d) n))
                                           (when (= 0 (% n d))
                                             (setq is-prime nil))
                                           (setq d (1+ d)))
                                         (when is-prime
                                           (setq result (cons n result)))))
                                     (nreverse result))))
                      (list
                        (length seq1)
                        (length seq2)
                        sum1
                        sum2
                        common-sorted
                        primes
                        ;; Squares of first 10 naturals
                        (mapcar (lambda (n) (* n n)) (number-sequence 1 10))
                        ;; Cubes of first 5 naturals
                        (mapcar (lambda (n) (* n n n)) (number-sequence 1 5)))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: combining number-sequence with mapcar for computed sequences
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_mapcar_compositions() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let* (;; Fibonacci-like: use number-sequence as index, accumulate
                         (fib-indices (number-sequence 0 15))
                         (fibs (let ((memo (make-hash-table)))
                                 (puthash 0 0 memo)
                                 (puthash 1 1 memo)
                                 (mapcar
                                   (lambda (n)
                                     (or (gethash n memo)
                                         (let ((val (+ (gethash (- n 1) memo)
                                                       (gethash (- n 2) memo))))
                                           (puthash n val memo)
                                           val)))
                                   fib-indices)))
                         ;; Factorial via accumulation
                         (fact-seq (let ((acc 1)
                                         (result nil))
                                    (dolist (n (number-sequence 1 10))
                                      (setq acc (* acc n))
                                      (setq result (cons acc result)))
                                    (nreverse result)))
                         ;; Pascal's triangle row: C(n,k) for n=8
                         (pascal-row
                           (let ((n 8))
                             (mapcar
                               (lambda (k)
                                 (let ((num 1) (den 1) (i 0))
                                   (while (< i k)
                                     (setq num (* num (- n i)))
                                     (setq den (* den (1+ i)))
                                     (setq i (1+ i)))
                                   (/ num den)))
                               (number-sequence 0 n))))
                         ;; Partial sums: cumulative sum of 1..10
                         (partial-sums
                           (let ((sum 0) (result nil))
                             (dolist (n (number-sequence 1 10))
                               (setq sum (+ sum n))
                               (setq result (cons sum result)))
                             (nreverse result))))
                    (list
                      fibs
                      fact-seq
                      pascal-row
                      partial-sums
                      ;; Zip two sequences together
                      (let ((letters '("a" "b" "c" "d" "e"))
                            (nums (number-sequence 1 5)))
                        (mapcar (lambda (i)
                                  (cons (nth (1- i) letters) i))
                                nums))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: number-sequence for matrix generation and manipulation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_matrix_generation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let* (;; Generate a 4x4 identity-like matrix using number-sequence
                         (rows (number-sequence 0 3))
                         (cols (number-sequence 0 3))
                         (identity-matrix
                           (mapcar (lambda (r)
                                     (mapcar (lambda (c)
                                               (if (= r c) 1 0))
                                             cols))
                                   rows))
                         ;; Generate a multiplication table 1..6 x 1..6
                         (mul-table
                           (mapcar (lambda (r)
                                     (mapcar (lambda (c) (* r c))
                                             (number-sequence 1 6)))
                                   (number-sequence 1 6)))
                         ;; Diagonal of multiplication table
                         (diagonal
                           (mapcar (lambda (i)
                                     (nth i (nth i mul-table)))
                                   (number-sequence 0 5)))
                         ;; Sum of each row
                         (row-sums
                           (mapcar (lambda (row) (apply #'+ row)) mul-table))
                         ;; Sum of each column via transpose
                         (col-sums
                           (mapcar (lambda (j)
                                     (apply #'+ (mapcar (lambda (row) (nth j row))
                                                        mul-table)))
                                   (number-sequence 0 5))))
                    (list identity-matrix
                          mul-table
                          diagonal
                          row-sums
                          col-sums))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: number-sequence with reduce/fold patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_reduce_patterns() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (fset 'neovm--nsadv-fold-left
    (lambda (fn init seq)
      "Left fold: (fn (fn (fn init e1) e2) e3) ..."
      (let ((acc init))
        (dolist (x seq)
          (setq acc (funcall fn acc x)))
        acc)))

  (unwind-protect
      (let* (;; Sum via fold
             (sum (funcall 'neovm--nsadv-fold-left #'+ 0 (number-sequence 1 100)))
             ;; Product of 1..10 via fold
             (product (funcall 'neovm--nsadv-fold-left #'* 1 (number-sequence 1 10)))
             ;; Maximum via fold
             (maxval (funcall 'neovm--nsadv-fold-left #'max -999 (number-sequence -50 50 7)))
             ;; Build a reversed list via fold
             (reversed (funcall 'neovm--nsadv-fold-left
                                (lambda (acc x) (cons x acc))
                                nil
                                (number-sequence 1 8)))
             ;; Running maximum
             (running-max
               (let ((mx nil) (result nil))
                 (dolist (n (number-sequence 5 1 -1))
                   (setq mx (if mx (max mx n) n))
                   (setq result (cons mx result)))
                 (nreverse result)))
             ;; Alternating sum: 1 - 2 + 3 - 4 + ... + 9 - 10
             (alt-sum
               (let ((sum 0) (sign 1))
                 (dolist (n (number-sequence 1 10))
                   (setq sum (+ sum (* sign n)))
                   (setq sign (* sign -1)))
                 sum)))
        (list sum product maxval reversed running-max alt-sum))
    (fmakunbound 'neovm--nsadv-fold-left)))"#;
    assert_oracle_parity(form);
}
