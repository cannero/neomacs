//! Oracle parity tests for `vconcat`: vector concatenation with vectors,
//! lists, strings, multiple arguments, empty/nil arguments, and complex
//! matrix construction patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// vconcat two vectors
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vconcat_two_vectors() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (vconcat [1 2 3] [4 5 6])
                        (vconcat [a] [b])
                        (vconcat [1] [2 3 4 5])
                        (vconcat [100 200] [300]))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// vconcat vector + list
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vconcat_vector_and_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (vconcat [1 2] '(3 4))
                        (vconcat '(a b) [c d])
                        (vconcat [10] '(20 30 40))
                        (vconcat '(x) '(y) [z]))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// vconcat vector + string (string becomes char codes)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vconcat_vector_and_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (vconcat [1 2] "abc")
                        (vconcat "hello" [33])
                        (vconcat "AB" "CD")
                        (vconcat [0] "x" [255]))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// vconcat with 3+ arguments
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vconcat_multiple_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (vconcat [1] [2] [3] [4] [5])
                        (vconcat '(a) [b] '(c) [d])
                        (vconcat "A" [66] '(67) "D")
                        (vconcat [1 2] '(3 4) [5 6] '(7 8) [9 10]))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// vconcat with empty args
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vconcat_empty_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (vconcat)
                        (vconcat [])
                        (vconcat [] [])
                        (vconcat [] [1 2] [])
                        (vconcat '() [3 4] '())
                        (vconcat "" [5]))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// vconcat with nil arguments
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vconcat_nil_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (vconcat nil)
                        (vconcat nil nil)
                        (vconcat nil [1 2 3])
                        (vconcat [4 5] nil [6])
                        (vconcat nil nil nil))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: build a matrix row-by-row using vconcat
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vconcat_matrix_build() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a 4x4 identity-like matrix stored as a flat vector,
    // row-by-row using vconcat.
    let form = r#"(let ((matrix [])
                        (size 4))
                    (dotimes (row size)
                      (let ((row-vec (make-vector size 0)))
                        (aset row-vec row (1+ row))
                        (setq matrix (vconcat matrix row-vec))))
                    ;; matrix should be [1 0 0 0 0 2 0 0 0 0 3 0 0 0 0 4]
                    ;; Verify by extracting diagonal elements
                    (let ((diag nil)
                          (off-diag-sum 0))
                      (dotimes (i size)
                        (let ((idx (+ (* i size) i)))
                          (setq diag (cons (aref matrix idx) diag))))
                      ;; Sum of off-diagonal
                      (dotimes (i (* size size))
                        (let ((r (/ i size))
                              (c (% i size)))
                          (unless (= r c)
                            (setq off-diag-sum
                                  (+ off-diag-sum (aref matrix i))))))
                      (list matrix
                            (nreverse diag)
                            off-diag-sum
                            (length matrix))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: flatten nested list-of-vectors using vconcat
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vconcat_flatten_chunks() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((chunks '([1 2 3] [4 5] [6 7 8 9] [10])))
                    (let ((flat [])
                          (sizes nil))
                      (dolist (chunk chunks)
                        (setq sizes (cons (length chunk) sizes))
                        (setq flat (vconcat flat chunk)))
                      ;; Verify total length and contents
                      (let ((total (apply '+ (nreverse sizes)))
                            (sum 0))
                        (dotimes (i (length flat))
                          (setq sum (+ sum (aref flat i))))
                        (list flat
                              total
                              (= total (length flat))
                              sum
                              ;; Verify sum is 1+2+...+10 = 55
                              (= sum 55)))))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: interleave two vectors using vconcat
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vconcat_interleave() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((interleave
                         (lambda (v1 v2)
                           (let ((result [])
                                 (len1 (length v1))
                                 (len2 (length v2))
                                 (i 0))
                             (while (or (< i len1) (< i len2))
                               (when (< i len1)
                                 (setq result (vconcat result
                                                       (vector (aref v1 i)))))
                               (when (< i len2)
                                 (setq result (vconcat result
                                                       (vector (aref v2 i)))))
                               (setq i (1+ i)))
                             result))))
                    (list
                     (funcall interleave [1 2 3] [a b c])
                     (funcall interleave [1 2 3 4] [x y])
                     (funcall interleave [p] [q r s])
                     (funcall interleave [] [a b])
                     (funcall interleave [] [])))"#;
    assert_oracle_parity_with_bootstrap(form);
}
