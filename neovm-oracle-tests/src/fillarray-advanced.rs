//! Oracle parity tests for `fillarray` — advanced patterns:
//! complex element types, string fills, bool-vector fills, return value identity,
//! algorithm building blocks with fillarray, pattern construction, and
//! large-array operations.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// fillarray on vectors with diverse element types (cons, vectors, symbols, etc.)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fillarray_advanced_diverse_element_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Fill vectors with various Lisp types and verify that all elements
    // become the fill value, regardless of what was there before.
    let form = r#"
(let ((results nil))
  ;; Fill with cons cell
  (let ((v (vector 1 2 3 4 5)))
    (fillarray v '(a . b))
    (push (list 'cons-fill
                (equal (aref v 0) '(a . b))
                (equal (aref v 4) '(a . b))
                ;; All elements eq to each other (same cons cell)
                (eq (aref v 0) (aref v 1)))
          results))
  ;; Fill with a vector
  (let ((v (make-vector 4 0)))
    (fillarray v [10 20 30])
    (push (list 'vector-fill
                (equal (aref v 0) [10 20 30])
                (eq (aref v 0) (aref v 3)))
          results))
  ;; Fill with a string
  (let ((v (make-vector 3 nil)))
    (fillarray v "hello")
    (push (list 'string-fill
                (equal (aref v 0) "hello")
                (eq (aref v 0) (aref v 2)))
          results))
  ;; Fill with a float
  (let ((v (vector 'a 'b 'c)))
    (fillarray v 3.14)
    (push (list 'float-fill
                (= (aref v 0) 3.14)
                (= (aref v 2) 3.14))
          results))
  ;; Fill with t, then with nil
  (let ((v (make-vector 5 42)))
    (fillarray v t)
    (let ((all-t (and (eq (aref v 0) t) (eq (aref v 4) t))))
      (fillarray v nil)
      (let ((all-nil (and (eq (aref v 0) nil) (eq (aref v 4) nil))))
        (push (list 'bool-fill all-t all-nil) results))))
  ;; Fill with a keyword symbol
  (let ((v (vector 1 2 3)))
    (fillarray v :keyword)
    (push (list 'keyword-fill (eq (aref v 0) :keyword) (eq (aref v 2) :keyword))
          results))
  (nreverse results))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// fillarray on strings with various characters
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fillarray_advanced_string_chars() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(let ((results nil))
  ;; Fill with ASCII character
  (let ((s (copy-sequence "abcdef")))
    (fillarray s ?X)
    (push (list 'ascii s (length s)) results))
  ;; Fill with space
  (let ((s (copy-sequence "hello world")))
    (fillarray s ?\s)
    (push (list 'space s (length s)) results))
  ;; Fill with zero character
  (let ((s (copy-sequence "test")))
    (fillarray s 0)
    (push (list 'zero (length s) (aref s 0) (aref s 3)) results))
  ;; Fill with digit character
  (let ((s (copy-sequence "abc")))
    (fillarray s ?9)
    (push (list 'digit s) results))
  ;; Fill empty string (should work, no-op)
  (let ((s (copy-sequence "")))
    (fillarray s ?z)
    (push (list 'empty s (length s)) results))
  ;; Fill and verify each position individually
  (let ((s (make-string 8 ?a)))
    (fillarray s ?b)
    (let ((chars nil))
      (dotimes (i 8)
        (push (aref s i) chars))
      (push (list 'each-pos (nreverse chars)) results)))
  (nreverse results))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// fillarray on bool-vectors with size edge cases
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fillarray_advanced_bool_vector_sizes() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(let ((results nil))
  ;; Size 1 bool-vector
  (let ((bv (make-bool-vector 1 nil)))
    (fillarray bv t)
    (push (list 'size1 (aref bv 0)) results))
  ;; Size 7 (not byte-aligned)
  (let ((bv (make-bool-vector 7 t)))
    (fillarray bv nil)
    (let ((all-nil t))
      (dotimes (i 7)
        (when (aref bv i) (setq all-nil nil)))
      (push (list 'size7 all-nil) results)))
  ;; Size 8 (exact byte boundary)
  (let ((bv (make-bool-vector 8 nil)))
    (fillarray bv t)
    (let ((all-t t))
      (dotimes (i 8)
        (unless (aref bv i) (setq all-t nil)))
      (push (list 'size8 all-t) results)))
  ;; Size 9 (one past byte boundary)
  (let ((bv (make-bool-vector 9 t)))
    (fillarray bv nil)
    (fillarray bv t)
    (let ((all-t t))
      (dotimes (i 9)
        (unless (aref bv i) (setq all-t nil)))
      (push (list 'size9 all-t) results)))
  ;; Size 16 (two bytes)
  (let ((bv (make-bool-vector 16 nil)))
    (fillarray bv t)
    (push (list 'size16 (aref bv 0) (aref bv 7) (aref bv 8) (aref bv 15)) results))
  ;; Alternating fill/verify cycles
  (let ((bv (make-bool-vector 10 nil)))
    (fillarray bv t)
    (let ((count-t 0))
      (dotimes (i 10) (when (aref bv i) (setq count-t (1+ count-t))))
      (fillarray bv nil)
      (let ((count-nil 0))
        (dotimes (i 10) (unless (aref bv i) (setq count-nil (1+ count-nil))))
        (push (list 'alternate count-t count-nil) results))))
  (nreverse results))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// fillarray return value identity (eq check) across types
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fillarray_advanced_return_identity() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(let ((results nil))
  ;; Vector: return value is eq to argument
  (let* ((v (make-vector 5 0))
         (ret (fillarray v 99)))
    (push (list 'vec-eq (eq v ret) (aref ret 2)) results))
  ;; String: return value is eq to argument
  (let* ((s (copy-sequence "hello"))
         (ret (fillarray s ?z)))
    (push (list 'str-eq (eq s ret) (string= ret "zzzzz")) results))
  ;; Bool-vector: return value is eq to argument
  (let* ((bv (make-bool-vector 4 nil))
         (ret (fillarray bv t)))
    (push (list 'bv-eq (eq bv ret) (aref ret 0) (aref ret 3)) results))
  ;; Chain fillarray calls using return value
  (let* ((v (make-vector 3 0))
         (r1 (fillarray v 1))
         (r2 (fillarray r1 2))
         (r3 (fillarray r2 3)))
    (push (list 'chain (eq v r1) (eq r1 r2) (eq r2 r3)
                (aref v 0) (aref v 1) (aref v 2))
          results))
  ;; Use return value in an expression
  (let ((v (make-vector 4 0)))
    (push (list 'expr-use (aref (fillarray v 42) 0)
                (length (fillarray v 99)))
          results))
  (nreverse results))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: fillarray + aset to build patterns (checkerboard, gradient)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fillarray_advanced_pattern_building() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(let ((results nil))
  ;; Checkerboard pattern: fillarray to 0, then set odd indices to 1
  (let ((v (make-vector 10 nil)))
    (fillarray v 0)
    (let ((i 1))
      (while (< i 10)
        (aset v i 1)
        (setq i (+ i 2))))
    (let ((pattern nil))
      (dotimes (i 10) (push (aref v i) pattern))
      (push (list 'checkerboard (nreverse pattern)) results)))
  ;; Gradient: fillarray to base, then add offset per position
  (let ((v (make-vector 8 0)))
    (fillarray v 100)
    (dotimes (i 8)
      (aset v i (+ (aref v i) (* i 10))))
    (let ((gradient nil))
      (dotimes (i 8) (push (aref v i) gradient))
      (push (list 'gradient (nreverse gradient)) results)))
  ;; Ring buffer pattern: fillarray clears, then write head/tail
  (let ((buf (make-vector 6 nil))
        (head 0) (tail 0))
    (fillarray buf nil)
    ;; Write some values
    (aset buf tail 'a) (setq tail (% (1+ tail) 6))
    (aset buf tail 'b) (setq tail (% (1+ tail) 6))
    (aset buf tail 'c) (setq tail (% (1+ tail) 6))
    ;; Read one
    (let ((first-val (aref buf head)))
      (aset buf head nil) (setq head (% (1+ head) 6))
      ;; Clear and verify
      (fillarray buf nil)
      (let ((all-nil t))
        (dotimes (i 6)
          (when (aref buf i) (setq all-nil nil)))
        (push (list 'ring-buf first-val all-nil) results))))
  ;; Matrix row initialization: 2D array as vector of vectors
  (let ((rows 3) (cols 4))
    (let ((matrix (make-vector rows nil)))
      (dotimes (r rows)
        (aset matrix r (make-vector cols 0))
        (fillarray (aref matrix r) (1+ r)))
      (let ((row-sums nil))
        (dotimes (r rows)
          (let ((sum 0))
            (dotimes (c cols)
              (setq sum (+ sum (aref (aref matrix r) c))))
            (push sum row-sums)))
        (push (list 'matrix-rows (nreverse row-sums)) results))))
  (nreverse results))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: fillarray in algorithm clearing (sieve, histogram reset)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fillarray_advanced_algorithm_clearing() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(let ((results nil))
  ;; Sieve of Eratosthenes using fillarray for initialization
  (let ((limit 30)
        (sieve nil))
    (setq sieve (make-vector (1+ limit) t))
    (aset sieve 0 nil)
    (aset sieve 1 nil)
    (let ((i 2))
      (while (<= (* i i) limit)
        (when (aref sieve i)
          (let ((j (* i i)))
            (while (<= j limit)
              (aset sieve j nil)
              (setq j (+ j i)))))
        (setq i (1+ i))))
    (let ((primes nil))
      (let ((k 2))
        (while (<= k limit)
          (when (aref sieve k) (push k primes))
          (setq k (1+ k))))
      (push (list 'sieve-primes (nreverse primes)) results))
    ;; Now reset the sieve with fillarray for reuse
    (fillarray sieve nil)
    (let ((all-nil t))
      (dotimes (i (1+ limit))
        (when (aref sieve i) (setq all-nil nil)))
      (push (list 'sieve-reset all-nil) results)))
  ;; Multi-round histogram: compute, snapshot, reset, compute again
  (let ((hist (make-vector 5 0))
        (data1 '(0 1 2 3 4 0 1 2 3 4 0 1))
        (data2 '(4 4 4 3 3 2)))
    ;; Round 1
    (dolist (d data1) (aset hist d (1+ (aref hist d))))
    (let ((snap1 (copy-sequence hist)))
      ;; Reset
      (fillarray hist 0)
      ;; Round 2
      (dolist (d data2) (aset hist d (1+ (aref hist d))))
      (push (list 'hist-r1 (append snap1 nil)
                  'hist-r2 (append hist nil))
            results)))
  ;; Frequency table with fillarray-based pooling
  (let ((pool (make-vector 26 0))
        (word "helloworld"))
    (dotimes (i (length word))
      (let ((idx (- (aref word i) ?a)))
        (aset pool idx (1+ (aref pool idx)))))
    (let ((freq nil))
      (dotimes (i 26)
        (when (> (aref pool i) 0)
          (push (list (+ ?a i) (aref pool i)) freq)))
      (push (list 'freq (nreverse freq)) results))
    ;; Reset pool
    (fillarray pool 0)
    (push (list 'pool-reset (aref pool 0) (aref pool 7) (aref pool 25)) results))
  (nreverse results))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Complex: fillarray with large arrays and repeated operations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fillarray_advanced_large_operations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(let ((results nil))
  ;; Large vector fill and spot-check
  (let ((v (make-vector 500 0)))
    (fillarray v 'x)
    (push (list 'large-500
                (eq (aref v 0) 'x)
                (eq (aref v 249) 'x)
                (eq (aref v 499) 'x)
                (length v))
          results))
  ;; Repeated fill cycles
  (let ((v (make-vector 100 nil)))
    (let ((i 0))
      (while (< i 10)
        (fillarray v i)
        (setq i (1+ i))))
    ;; After 10 fills, should contain 9 everywhere
    (push (list 'repeated
                (= (aref v 0) 9)
                (= (aref v 50) 9)
                (= (aref v 99) 9))
          results))
  ;; Fill + selective modify + verify non-modified
  (let ((v (make-vector 200 0)))
    (fillarray v -1)
    ;; Modify every 10th element
    (let ((i 0))
      (while (< i 200)
        (aset v i i)
        (setq i (+ i 10))))
    ;; Count elements that are still -1
    (let ((neg-count 0) (mod-count 0))
      (dotimes (i 200)
        (if (= (aref v i) -1)
            (setq neg-count (1+ neg-count))
          (setq mod-count (1+ mod-count))))
      (push (list 'selective neg-count mod-count) results)))
  ;; Large string fill
  (let ((s (make-string 200 ?a)))
    (fillarray s ?z)
    (push (list 'large-str
                (aref s 0)
                (aref s 99)
                (aref s 199)
                (length s))
          results))
  ;; Large bool-vector fill
  (let ((bv (make-bool-vector 100 nil)))
    (fillarray bv t)
    (let ((true-count 0))
      (dotimes (i 100)
        (when (aref bv i) (setq true-count (1+ true-count))))
      (push (list 'large-bv true-count) results)))
  (nreverse results))
"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// fillarray interaction with shared structure (multiple references)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fillarray_advanced_shared_structure() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(let ((results nil))
  ;; Two variables pointing to same vector
  (let* ((v1 (make-vector 4 0))
         (v2 v1))
    (fillarray v1 42)
    (push (list 'shared-vec
                (eq v1 v2)
                (aref v2 0)
                (aref v2 3))
          results))
  ;; Vector stored in a list, fillarray via extraction
  (let* ((v (make-vector 3 'a))
         (lst (list 'data v 'end)))
    (fillarray (cadr lst) 'b)
    (push (list 'in-list
                (aref v 0)
                (aref (cadr lst) 2)
                (eq v (cadr lst)))
          results))
  ;; Vector stored in another vector
  (let* ((inner (make-vector 3 0))
         (outer (vector inner 'x 'y)))
    (fillarray (aref outer 0) 99)
    (push (list 'nested
                (aref inner 0)
                (aref inner 2)
                (aref (aref outer 0) 1))
          results))
  ;; fillarray a string that's also in a list
  (let* ((s (copy-sequence "abc"))
         (data (list s (length s))))
    (fillarray s ?z)
    (push (list 'str-shared
                (string= s "zzz")
                (string= (car data) "zzz")
                (eq s (car data)))
          results))
  ;; Hash table value is a vector, fillarray it
  (let ((ht (make-hash-table)))
    (let ((v (make-vector 3 0)))
      (puthash 'key v ht)
      (fillarray (gethash 'key ht) 7)
      (push (list 'hash-val
                  (aref v 0)
                  (aref (gethash 'key ht) 2)
                  (eq v (gethash 'key ht)))
            results)))
  (nreverse results))
"#;
    assert_oracle_parity_with_bootstrap(form);
}
