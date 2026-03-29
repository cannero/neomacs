//! Oracle parity tests for vector operations: `make-vector`, `vconcat`,
//! `vectorp`, `arrayp`, `elt`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
    eval_oracle_and_neovm_with_bootstrap,
};

// ---------------------------------------------------------------------------
// make-vector
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_make_vector_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(make-vector 5 0)");
    assert_oracle_parity_with_bootstrap("(make-vector 3 nil)");
    assert_oracle_parity_with_bootstrap("(make-vector 0 42)");
    assert_oracle_parity_with_bootstrap("(make-vector 4 'hello)");
}

#[test]
fn oracle_prop_make_vector_with_string_init() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(make-vector 3 "test")"#);
}

#[test]
fn oracle_prop_make_vector_modify() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // All elements share the same initial value
    let form = "(let ((v (make-vector 5 0)))
                  (aset v 0 10)
                  (aset v 2 20)
                  (aset v 4 30)
                  (list (aref v 0) (aref v 1) (aref v 2)
                        (aref v 3) (aref v 4)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// vconcat
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vconcat_vectors() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(vconcat [1 2 3] [4 5 6])");
    assert_oracle_parity_with_bootstrap("(vconcat [1] [2] [3])");
    assert_oracle_parity_with_bootstrap("(vconcat [] [1 2])");
    assert_oracle_parity_with_bootstrap("(vconcat [1 2] [])");
    assert_oracle_parity_with_bootstrap("(vconcat)");
}

#[test]
fn oracle_prop_vconcat_lists() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // vconcat can accept lists
    assert_oracle_parity_with_bootstrap("(vconcat '(1 2 3))");
    assert_oracle_parity_with_bootstrap("(vconcat '(a b) '(c d))");
    assert_oracle_parity_with_bootstrap("(vconcat [1 2] '(3 4))");
}

#[test]
fn oracle_prop_vconcat_strings() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // vconcat converts strings to vectors of char codes
    assert_oracle_parity_with_bootstrap(r#"(vconcat "abc")"#);
    assert_oracle_parity_with_bootstrap(r#"(vconcat "hi" [33])"#);
}

#[test]
fn oracle_prop_vconcat_multiple_types() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(vconcat [1 2] '(3 4) "AB")"#);
}

// ---------------------------------------------------------------------------
// vectorp / arrayp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vectorp_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(vectorp [1 2 3])");
    assert_oracle_parity_with_bootstrap("(vectorp [])");
    assert_oracle_parity_with_bootstrap("(vectorp '(1 2 3))");
    assert_oracle_parity_with_bootstrap("(vectorp nil)");
    assert_oracle_parity_with_bootstrap("(vectorp 42)");
    assert_oracle_parity_with_bootstrap(r#"(vectorp "hello")"#);
}

#[test]
fn oracle_prop_arrayp_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(arrayp [1 2 3])");
    assert_oracle_parity_with_bootstrap(r#"(arrayp "hello")"#);
    assert_oracle_parity_with_bootstrap("(arrayp '(1 2 3))");
    assert_oracle_parity_with_bootstrap("(arrayp nil)");
    assert_oracle_parity_with_bootstrap("(arrayp 42)");
}

// ---------------------------------------------------------------------------
// elt (works on sequences: lists, vectors, strings)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_elt_vector() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(elt [10 20 30 40] 0)");
    assert_oracle_parity_with_bootstrap("(elt [10 20 30 40] 2)");
    assert_oracle_parity_with_bootstrap("(elt [10 20 30 40] 3)");
}

#[test]
fn oracle_prop_elt_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(elt '(a b c d) 0)");
    assert_oracle_parity_with_bootstrap("(elt '(a b c d) 2)");
    assert_oracle_parity_with_bootstrap("(elt '(a b c d) 3)");
}

#[test]
fn oracle_prop_elt_string() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(r#"(elt "hello" 0)"#);
    assert_oracle_parity_with_bootstrap(r#"(elt "hello" 4)"#);
}

#[test]
fn oracle_prop_elt_complex_sequence_dispatch() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // elt dispatches based on sequence type
    let form = r####"(let ((vec [a b c])
                        (lst '(x y z))
                        (str "ABC"))
                    (list (elt vec 1) (elt lst 1) (elt str 1)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// delete (destructive removal)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_delete_from_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(delete 3 (list 1 2 3 4 3 5))");
    assert_oracle_parity_with_bootstrap("(delete 'b (list 'a 'b 'c 'b 'd))");
    assert_oracle_parity_with_bootstrap("(delete 99 (list 1 2 3))");
    assert_oracle_parity_with_bootstrap("(delete 1 (list 1))");
    assert_oracle_parity_with_bootstrap("(delete 1 nil)");
}

#[test]
fn oracle_prop_delete_string_from_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // delete uses equal comparison
    let form = r####"(delete "hello" (list "hello" "world" "hello" "foo"))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_delete_from_vector() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // delete on vectors returns a new vector
    assert_oracle_parity_with_bootstrap("(delete 3 [1 2 3 4 3 5])");
    assert_oracle_parity_with_bootstrap("(delete 99 [1 2 3])");
}

// ---------------------------------------------------------------------------
// number-sequence
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_number_sequence_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(number-sequence 1 5)");
    assert_oracle_parity_with_bootstrap("(number-sequence 0 0)");
    assert_oracle_parity_with_bootstrap("(number-sequence 5 1)");
}

#[test]
fn oracle_prop_number_sequence_with_step() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(number-sequence 0 10 2)");
    assert_oracle_parity_with_bootstrap("(number-sequence 0 10 3)");
    assert_oracle_parity_with_bootstrap("(number-sequence 10 0 -2)");
    assert_oracle_parity_with_bootstrap("(number-sequence 5 -5 -3)");
}

#[test]
fn oracle_prop_number_sequence_float() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(number-sequence 0.0 1.0 0.25)");
    assert_oracle_parity_with_bootstrap("(number-sequence 1.0 2.0 0.5)");
}

#[test]
fn oracle_prop_number_sequence_single() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(number-sequence 42 42)");
    assert_oracle_parity_with_bootstrap("(number-sequence 42 42 5)");
}

// ---------------------------------------------------------------------------
// Complex: vector as lookup table
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_vector_as_lookup_table() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Build a frequency table using a vector
    let form = r####"(let ((data '(3 1 4 1 5 9 2 6 5 3 5))
                        (freq (make-vector 10 0)))
                    (dolist (n data)
                      (aset freq n (1+ (aref freq n))))
                    (let ((result nil))
                      (dotimes (i 10)
                        (when (> (aref freq i) 0)
                          (setq result
                                (cons (cons i (aref freq i)) result))))
                      (nreverse result)))"####;
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_vector_matrix_operations() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // 2x2 matrix multiply using vectors
    let form = "(let ((a [1 2 3 4])
                      (b [5 6 7 8]))
                  ;; Matrix multiply: [a00*b00+a01*b10, a00*b01+a01*b11,
                  ;;                   a10*b00+a11*b10, a10*b01+a11*b11]
                  (let ((r (make-vector 4 0)))
                    (aset r 0 (+ (* (aref a 0) (aref b 0))
                                 (* (aref a 1) (aref b 2))))
                    (aset r 1 (+ (* (aref a 0) (aref b 1))
                                 (* (aref a 1) (aref b 3))))
                    (aset r 2 (+ (* (aref a 2) (aref b 0))
                                 (* (aref a 3) (aref b 2))))
                    (aset r 3 (+ (* (aref a 2) (aref b 1))
                                 (* (aref a 3) (aref b 3))))
                    r))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_vconcat_flatten_nested() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Flatten a list of vectors into one
    let form = "(let ((chunks '([1 2] [3 4 5] [6])))
                  (let ((result []))
                    (dolist (chunk chunks)
                      (setq result (vconcat result chunk)))
                    result))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// proptest: number-sequence length
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_number_sequence_length(
        from in 0i64..20,
        count in 1usize..10,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let to = from + count as i64 - 1;
        let form = format!(
            "(length (number-sequence {} {}))",
            from, to
        );
        let (oracle, neovm) = eval_oracle_and_neovm_with_bootstrap(&form);
        let expected = format!("OK {}", count);
        prop_assert_eq!(neovm.as_str(), expected.as_str());
        prop_assert_eq!(oracle.as_str(), expected.as_str());
    }
}
