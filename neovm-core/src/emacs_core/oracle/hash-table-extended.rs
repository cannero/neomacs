//! Oracle parity tests for hash-table with thorough parameter coverage.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm, ORACLE_PROP_CASES};

#[test]
fn oracle_prop_hash_table_test_eq() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // eq test: symbols are eq, string literals are not
    let form = "(let ((h (make-hash-table :test 'eq)))
                  (puthash 'foo 1 h)
                  (puthash 'bar 2 h)
                  (list (gethash 'foo h) (gethash 'bar h)
                        (gethash 'baz h)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(1 2 nil)", &o, &n);
}

#[test]
fn oracle_prop_hash_table_test_equal() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // equal test: structural comparison
    let form = r#"(let ((h (make-hash-table :test 'equal)))
                    (puthash "key" 'val1 h)
                    (puthash '(1 2) 'val2 h)
                    (puthash [3 4] 'val3 h)
                    (list (gethash "key" h)
                          (gethash '(1 2) h)
                          (gethash [3 4] h)))"#;
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_hash_table_size_hint() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // :size is just a hint, doesn't limit capacity
    let form = "(let ((h (make-hash-table :size 2)))
                  (dotimes (i 100)
                    (puthash i (* i i) h))
                  (list (hash-table-count h)
                        (gethash 50 h)
                        (gethash 99 h)))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_hash_table_maphash() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((h (make-hash-table))
                      (keys nil))
                  (puthash 'a 1 h)
                  (puthash 'b 2 h)
                  (puthash 'c 3 h)
                  (maphash (lambda (k v)
                             (setq keys (cons k keys))) h)
                  (sort keys (lambda (a b)
                               (string-lessp (symbol-name a)
                                             (symbol-name b)))))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_hash_table_as_set() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Use hash table as a set
    let form = "(let ((seen (make-hash-table :test 'eq))
                      (unique nil))
                  (dolist (x '(a b c a b d e c f a))
                    (unless (gethash x seen)
                      (puthash x t seen)
                      (setq unique (cons x unique))))
                  (nreverse unique))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_hash_table_copy() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((h1 (make-hash-table :test 'equal)))
                  (puthash 'a 1 h1)
                  (puthash 'b 2 h1)
                  (let ((h2 (copy-hash-table h1)))
                    (puthash 'c 3 h2)
                    (list (hash-table-count h1)
                          (hash-table-count h2)
                          (gethash 'c h1)
                          (gethash 'c h2))))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_hash_table_rehash_threshold() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((h (make-hash-table :test 'eq
                                          :rehash-threshold 0.5)))
                  (dotimes (i 50)
                    (puthash i i h))
                  (hash-table-count h))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("50", &o, &n);
}

#[test]
fn oracle_prop_hash_table_chained_operations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Complex: insert, delete, re-insert, check counts
    let form = "(let ((h (make-hash-table)))
                  (dotimes (i 10) (puthash i (* i 10) h))
                  (let ((count-before (hash-table-count h)))
                    (dotimes (i 5) (remhash i h))
                    (let ((count-mid (hash-table-count h)))
                      (dotimes (i 3) (puthash (+ i 100) 'new h))
                      (list count-before count-mid
                            (hash-table-count h)
                            (gethash 0 h)
                            (gethash 7 h)
                            (gethash 100 h)))))";
    assert_oracle_parity(form);
}

#[test]
fn oracle_prop_hash_table_invert() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Invert a hash table (swap keys and values)
    let form = "(let ((h (make-hash-table))
                      (inv (make-hash-table)))
                  (puthash 'a 1 h)
                  (puthash 'b 2 h)
                  (puthash 'c 3 h)
                  (maphash (lambda (k v) (puthash v k inv)) h)
                  (list (gethash 1 inv)
                        (gethash 2 inv)
                        (gethash 3 inv)))";
    assert_oracle_parity(form);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_hash_table_proptest(
        n in 1usize..30usize,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(let ((h (make-hash-table)))
               (dotimes (i {}) (puthash i (* i i) h))
               (list (hash-table-count h) (gethash 0 h) (gethash (1- {}) h)))",
            n, n
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), oracle.as_str());
    }
}
