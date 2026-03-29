//! Complex oracle tests for algorithm implementations in Elisp.
//!
//! Tests sorting algorithms, searching, permutations, dynamic
//! programming, and classic algorithmic patterns.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// Merge sort implementation
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_algo_merge_sort() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
  (fset 'neovm--test-merge
    (lambda (a b)
      (cond
        ((null a) b)
        ((null b) a)
        ((<= (car a) (car b))
         (cons (car a)
               (funcall 'neovm--test-merge (cdr a) b)))
        (t (cons (car b)
                 (funcall 'neovm--test-merge a (cdr b)))))))
  (fset 'neovm--test-msort
    (lambda (lst)
      (if (or (null lst) (null (cdr lst)))
          lst
        (let ((mid (/ (length lst) 2)))
          (let ((left (let ((r nil) (i 0) (l lst))
                        (while (< i mid)
                          (setq r (cons (car l) r)
                                l (cdr l) i (1+ i)))
                        (nreverse r)))
                (right (nthcdr mid lst)))
            (funcall 'neovm--test-merge
                     (funcall 'neovm--test-msort left)
                     (funcall 'neovm--test-msort right)))))))
  (unwind-protect
      (funcall 'neovm--test-msort '(38 27 43 3 9 82 10))
    (fmakunbound 'neovm--test-merge)
    (fmakunbound 'neovm--test-msort)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Binary search
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_algo_binary_search() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((bsearch
                       (lambda (vec target)
                         (let ((lo 0)
                               (hi (1- (length vec)))
                               (found nil))
                           (while (and (<= lo hi) (not found))
                             (let ((mid (/ (+ lo hi) 2)))
                               (let ((val (aref vec mid)))
                                 (cond
                                   ((= val target) (setq found mid))
                                   ((< val target) (setq lo (1+ mid)))
                                   (t (setq hi (1- mid)))))))
                           found))))
                  (let ((sorted [2 5 8 12 16 23 38 56 72 91]))
                    (list (funcall bsearch sorted 23)
                          (funcall bsearch sorted 2)
                          (funcall bsearch sorted 91)
                          (funcall bsearch sorted 50))))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Fibonacci with memoization
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_algo_memoized_fibonacci() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((memo (make-hash-table)))
                  (puthash 0 0 memo)
                  (puthash 1 1 memo)
                  (fset 'neovm--test-fib
                    (lambda (n)
                      (or (gethash n memo)
                          (let ((result
                                 (+ (funcall 'neovm--test-fib (- n 1))
                                    (funcall 'neovm--test-fib (- n 2)))))
                            (puthash n result memo)
                            result))))
                  (unwind-protect
                      (list (funcall 'neovm--test-fib 0)
                            (funcall 'neovm--test-fib 1)
                            (funcall 'neovm--test-fib 5)
                            (funcall 'neovm--test-fib 10)
                            (funcall 'neovm--test-fib 20))
                    (fmakunbound 'neovm--test-fib)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Sieve of Eratosthenes
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_algo_sieve() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((limit 50))
                  (let ((sieve (make-vector (1+ limit) t)))
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
                    ;; Collect primes
                    (let ((primes nil))
                      (dotimes (i (1+ limit))
                        (when (aref sieve i)
                          (setq primes (cons i primes))))
                      (nreverse primes))))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Longest common subsequence (dynamic programming)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_algo_lcs() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((lcs
                     (lambda (s1 s2)
                       (let ((m (length s1))
                             (n (length s2)))
                         ;; Build DP table as vector of vectors
                         (let ((dp (make-vector (1+ m) nil)))
                           (dotimes (i (1+ m))
                             (aset dp i (make-vector (1+ n) 0)))
                           ;; Fill table
                           (dotimes (i m)
                             (dotimes (j n)
                               (if (= (aref s1 i) (aref s2 j))
                                   (aset (aref dp (1+ i)) (1+ j)
                                         (1+ (aref (aref dp i) j)))
                                 (aset (aref dp (1+ i)) (1+ j)
                                       (max (aref (aref dp i) (1+ j))
                                            (aref (aref dp (1+ i)) j))))))
                           ;; Backtrack to find the subsequence
                           (let ((result nil) (i m) (j n))
                             (while (and (> i 0) (> j 0))
                               (cond
                                 ((= (aref s1 (1- i)) (aref s2 (1- j)))
                                  (setq result
                                        (cons (aref s1 (1- i)) result))
                                  (setq i (1- i) j (1- j)))
                                 ((> (aref (aref dp (1- i)) j)
                                     (aref (aref dp i) (1- j)))
                                  (setq i (1- i)))
                                 (t (setq j (1- j)))))
                             (concat result)))))))
                  (list (funcall lcs "ABCBDAB" "BDCAB")
                        (funcall lcs "hello" "hallo")
                        (funcall lcs "abc" "xyz")))"#;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Permutations generator
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_algo_permutations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
  (fset 'neovm--test-perms
    (lambda (lst)
      (if (null lst)
          '(nil)
        (let ((result nil))
          (dolist (x lst)
            (let ((rest (delete x (copy-sequence lst))))
              (dolist (p (funcall 'neovm--test-perms rest))
                (setq result (cons (cons x p) result)))))
          (nreverse result)))))
  (unwind-protect
      (funcall 'neovm--test-perms '(1 2 3))
    (fmakunbound 'neovm--test-perms)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// Run-length encoding / decoding
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_algo_rle_roundtrip() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((rle-encode
                       (lambda (lst)
                         (when lst
                           (let ((result nil)
                                 (current (car lst))
                                 (count 1))
                             (dolist (x (cdr lst))
                               (if (equal x current)
                                   (setq count (1+ count))
                                 (setq result
                                       (cons (cons current count)
                                             result)
                                       current x count 1)))
                             (setq result
                                   (cons (cons current count) result))
                             (nreverse result)))))
                      (rle-decode
                       (lambda (encoded)
                         (let ((result nil))
                           (dolist (pair encoded)
                             (dotimes (_ (cdr pair))
                               (setq result
                                     (cons (car pair) result))))
                           (nreverse result)))))
                  (let ((input '(a a a b b c c c c a a)))
                    (let ((encoded (funcall rle-encode input)))
                      (let ((decoded (funcall rle-decode encoded)))
                        (list encoded
                              (equal input decoded))))))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// GCD / LCM
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_algo_gcd_lcm() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
  (fset 'neovm--test-gcd
    (lambda (a b)
      (if (= b 0) a
        (funcall 'neovm--test-gcd b (% a b)))))
  (unwind-protect
      (let ((lcm (lambda (a b)
                   (/ (* a b) (funcall 'neovm--test-gcd a b)))))
        (list (funcall 'neovm--test-gcd 48 18)
              (funcall 'neovm--test-gcd 100 75)
              (funcall lcm 12 8)
              (funcall lcm 15 20)))
    (fmakunbound 'neovm--test-gcd)))";
    assert_oracle_parity_with_bootstrap(form);
}
