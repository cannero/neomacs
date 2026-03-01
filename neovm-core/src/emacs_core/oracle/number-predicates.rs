//! Oracle parity tests for number predicates and type checks:
//! `zerop`, `natnump`, `fixnump`, `bignump`, `floatp`, `integerp`,
//! `numberp`, `number-or-marker-p`, `wholenump`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// zerop
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_zerop() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (zerop 0)
                        (zerop 0.0)
                        (zerop 1)
                        (zerop -1)
                        (zerop 0.0e0)
                        (zerop -0.0))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// natnump / wholenump
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_natnump() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (natnump 0)
                        (natnump 1)
                        (natnump 42)
                        (natnump -1)
                        (natnump 0.0)
                        (natnump nil)
                        (natnump 'a))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// fixnump
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fixnump() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (fixnump 0)
                        (fixnump 42)
                        (fixnump -100)
                        (fixnump most-positive-fixnum)
                        (fixnump most-negative-fixnum)
                        (fixnump 3.14)
                        (fixnump nil)
                        (fixnump "42"))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// floatp / integerp / numberp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_type_predicates() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((values (list 0 42 -1 3.14 0.0 -2.5
                                      nil t 'sym "str" '(1 2))))
                    (mapcar
                     (lambda (v)
                       (list v
                             (floatp v)
                             (integerp v)
                             (numberp v)))
                     values))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// most-positive-fixnum / most-negative-fixnum
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_fixnum_bounds() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list (integerp most-positive-fixnum)
                        (integerp most-negative-fixnum)
                        (> most-positive-fixnum 0)
                        (< most-negative-fixnum 0)
                        (fixnump most-positive-fixnum)
                        (fixnump most-negative-fixnum))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: numeric type dispatch
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_predicates_dispatch() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Dispatch based on numeric type to format differently
    let form = r#"(let ((format-num
                         (lambda (n)
                           (cond
                            ((not (numberp n)) (format "NaN(%s)" n))
                            ((and (integerp n) (zerop n)) "zero")
                            ((and (integerp n) (natnump n))
                             (format "+%d" n))
                            ((integerp n)
                             (format "%d" n))
                            ((and (floatp n) (isnan n)) "NaN")
                            ((floatp n)
                             (format "%.2f" n))
                            (t "?")))))
                    (mapcar format-num
                            (list 0 42 -7 3.14159 0.0
                                  -2.5 0.0e+NaN nil 'sym)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: statistics with type checking
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_predicates_safe_stats() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Compute statistics, filtering out non-numbers
    let form = r#"(let ((data '(10 "bad" 20 nil 30 'sym 40 50)))
                    (let ((nums nil)
                          (rejected nil))
                      ;; Partition into numbers and non-numbers
                      (dolist (x data)
                        (if (numberp x)
                            (setq nums (cons x nums))
                          (setq rejected (cons x rejected))))
                      (setq nums (nreverse nums))
                      (let ((n (length nums))
                            (sum (apply #'+ nums))
                            (mn (apply #'min nums))
                            (mx (apply #'max nums)))
                        (list (list 'count n)
                              (list 'sum sum)
                              (list 'mean (/ (float sum) n))
                              (list 'min mn)
                              (list 'max mx)
                              (list 'rejected (length rejected))))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: number base conversion
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_predicates_base_convert() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Convert integer to different base representations
    let form = r#"(let ((to-base
                         (lambda (n base)
                           (if (zerop n) "0"
                             (let ((digits nil)
                                   (neg (< n 0))
                                   (num (abs n)))
                               (while (> num 0)
                                 (let ((d (% num base)))
                                   (setq digits
                                         (cons (if (< d 10)
                                                   (+ ?0 d)
                                                 (+ ?a (- d 10)))
                                               digits))
                                   (setq num (/ num base))))
                               (concat (if neg "-" "")
                                       (apply #'string digits)))))))
                    (list
                     (funcall to-base 255 2)
                     (funcall to-base 255 8)
                     (funcall to-base 255 16)
                     (funcall to-base 0 10)
                     (funcall to-base -42 16)
                     (funcall to-base 1000 36)))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Complex: validation framework using predicates
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_predicates_validator() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Validate data records using predicate chains
    let form = r#"(let ((validators
                         (list
                          (cons 'name (lambda (v) (stringp v)))
                          (cons 'age (lambda (v)
                                       (and (integerp v)
                                            (natnump v)
                                            (< v 200))))
                          (cons 'score (lambda (v)
                                         (and (numberp v)
                                              (>= v 0.0)
                                              (<= v 100.0))))
                          (cons 'active (lambda (v)
                                          (or (eq v t) (eq v nil)))))))
                    (let ((validate
                           (lambda (record)
                             (let ((errors nil))
                               (dolist (v validators)
                                 (let ((field (car v))
                                       (pred (cdr v))
                                       (val (plist-get record (car v))))
                                   (unless (funcall pred val)
                                     (setq errors
                                           (cons (list field val)
                                                 errors)))))
                               (if errors
                                   (cons 'invalid (nreverse errors))
                                 'valid)))))
                      (list
                       (funcall validate
                                '(name "Alice" age 30
                                  score 95.5 active t))
                       (funcall validate
                                '(name 42 age -5
                                  score 150 active "yes")))))"#;
    assert_oracle_parity(form);
}
