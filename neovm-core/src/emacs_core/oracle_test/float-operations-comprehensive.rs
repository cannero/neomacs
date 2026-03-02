//! Comprehensive oracle parity tests for floating-point operations:
//! `float`, `truncate`, `floor`, `ceiling`, `round`, `ffloor`, `fceiling`,
//! `fround`, `ftruncate`, `isnan`, `frexp`, `ldexp`, `copysign`, `logb`,
//! special values (infinity, NaN, negative zero), and arithmetic edge cases.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, assert_oracle_parity, eval_oracle_and_neovm};

// ---------------------------------------------------------------------------
// float coercion
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_float_coercion_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Integer to float
    assert_oracle_parity("(float 0)");
    assert_oracle_parity("(float 1)");
    assert_oracle_parity("(float -1)");
    assert_oracle_parity("(float 42)");
    assert_oracle_parity("(float most-positive-fixnum)");
    assert_oracle_parity("(float most-negative-fixnum)");
    // Float to float (idempotent)
    assert_oracle_parity("(float 3.14)");
    assert_oracle_parity("(float -0.0)");
    assert_oracle_parity("(float 1.0e+INF)");
    assert_oracle_parity("(float -1.0e+INF)");
    // Verify type
    assert_oracle_parity("(floatp (float 7))");
    assert_oracle_parity("(integerp (float 7))");
}

// ---------------------------------------------------------------------------
// truncate with all parameter variations
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_truncate_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Single argument: toward zero
    assert_oracle_parity("(truncate 2.7)");
    assert_oracle_parity("(truncate -2.7)");
    assert_oracle_parity("(truncate 2.3)");
    assert_oracle_parity("(truncate -2.3)");
    assert_oracle_parity("(truncate 0.0)");
    assert_oracle_parity("(truncate -0.0)");
    assert_oracle_parity("(truncate 0.5)");
    assert_oracle_parity("(truncate -0.5)");
    assert_oracle_parity("(truncate 1.0e10)");
    // Two-argument division + truncate
    assert_oracle_parity("(truncate 10 3)");
    assert_oracle_parity("(truncate -10 3)");
    assert_oracle_parity("(truncate 10 -3)");
    assert_oracle_parity("(truncate -10 -3)");
    assert_oracle_parity("(truncate 7.5 2.5)");
    assert_oracle_parity("(truncate 1 3)");
    // Integer input (no-op)
    assert_oracle_parity("(truncate 5)");
    assert_oracle_parity("(truncate -5)");
}

// ---------------------------------------------------------------------------
// floor, ceiling, round — all parameter forms
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_floor_ceiling_round_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // floor: toward negative infinity
    assert_oracle_parity("(floor 2.7)");
    assert_oracle_parity("(floor -2.7)");
    assert_oracle_parity("(floor 2.5)");
    assert_oracle_parity("(floor -2.5)");
    assert_oracle_parity("(floor 10 3)");
    assert_oracle_parity("(floor -10 3)");
    assert_oracle_parity("(floor 10 -3)");
    assert_oracle_parity("(floor -10 -3)");
    assert_oracle_parity("(floor 7.0 2.0)");

    // ceiling: toward positive infinity
    assert_oracle_parity("(ceiling 2.3)");
    assert_oracle_parity("(ceiling -2.3)");
    assert_oracle_parity("(ceiling 2.5)");
    assert_oracle_parity("(ceiling -2.5)");
    assert_oracle_parity("(ceiling 10 3)");
    assert_oracle_parity("(ceiling -10 3)");
    assert_oracle_parity("(ceiling 10 -3)");
    assert_oracle_parity("(ceiling -10 -3)");

    // round: banker's rounding (to even)
    assert_oracle_parity("(round 2.5)");
    assert_oracle_parity("(round 3.5)");
    assert_oracle_parity("(round -2.5)");
    assert_oracle_parity("(round -3.5)");
    assert_oracle_parity("(round 0.5)");
    assert_oracle_parity("(round 1.5)");
    assert_oracle_parity("(round 2.49999)");
    assert_oracle_parity("(round 10 3)");
    assert_oracle_parity("(round -10 3)");
}

// ---------------------------------------------------------------------------
// ffloor, fceiling, fround, ftruncate (return float, not int)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ffloor_fceiling_fround_ftruncate() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // ffloor
    assert_oracle_parity("(ffloor 2.7)");
    assert_oracle_parity("(ffloor -2.7)");
    assert_oracle_parity("(ffloor 2.0)");
    assert_oracle_parity("(floatp (ffloor 2.7))");

    // fceiling
    assert_oracle_parity("(fceiling 2.3)");
    assert_oracle_parity("(fceiling -2.3)");
    assert_oracle_parity("(fceiling 2.0)");
    assert_oracle_parity("(floatp (fceiling 2.3))");

    // fround
    assert_oracle_parity("(fround 2.5)");
    assert_oracle_parity("(fround 3.5)");
    assert_oracle_parity("(fround -0.5)");
    assert_oracle_parity("(floatp (fround 2.5))");

    // ftruncate
    assert_oracle_parity("(ftruncate 2.7)");
    assert_oracle_parity("(ftruncate -2.7)");
    assert_oracle_parity("(ftruncate 0.0)");
    assert_oracle_parity("(floatp (ftruncate 2.7))");
}

// ---------------------------------------------------------------------------
// isnan
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_isnan_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(isnan 0.0e+NaN)");
    assert_oracle_parity("(isnan 0.0)");
    assert_oracle_parity("(isnan 1.0)");
    assert_oracle_parity("(isnan -0.0)");
    assert_oracle_parity("(isnan 1.0e+INF)");
    assert_oracle_parity("(isnan -1.0e+INF)");
    assert_oracle_parity("(isnan (/ 0.0 0.0))");
    assert_oracle_parity("(isnan (- 1.0e+INF 1.0e+INF))");
}

// ---------------------------------------------------------------------------
// frexp and ldexp
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_frexp_ldexp_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // frexp returns (significand . exponent) where 0.5 <= |sig| < 1.0
    assert_oracle_parity("(frexp 1.0)");
    assert_oracle_parity("(frexp 2.0)");
    assert_oracle_parity("(frexp 0.5)");
    assert_oracle_parity("(frexp -4.0)");
    assert_oracle_parity("(frexp 0.0)");
    assert_oracle_parity("(frexp 1024.0)");
    assert_oracle_parity("(frexp 0.125)");

    // ldexp: significand * 2^exponent
    assert_oracle_parity("(ldexp 0.5 1)");
    assert_oracle_parity("(ldexp 0.5 2)");
    assert_oracle_parity("(ldexp 0.75 10)");
    assert_oracle_parity("(ldexp 1.0 0)");
    assert_oracle_parity("(ldexp -0.5 3)");
    assert_oracle_parity("(ldexp 0.0 100)");

    // Round-trip: (ldexp (car (frexp x)) (cdr (frexp x))) == x
    let form = r#"(let* ((x 42.5)
                          (fr (frexp x)))
                     (= (ldexp (car fr) (cdr fr)) x))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// copysign
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_copysign_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(copysign 1.0 -1.0)");
    assert_oracle_parity("(copysign 1.0 1.0)");
    assert_oracle_parity("(copysign -1.0 1.0)");
    assert_oracle_parity("(copysign -1.0 -1.0)");
    assert_oracle_parity("(copysign 0.0 -1.0)");
    assert_oracle_parity("(copysign 0.0 1.0)");
    assert_oracle_parity("(copysign 3.14 -0.0)");
    assert_oracle_parity("(copysign 1.0e+INF -1.0)");
    assert_oracle_parity("(copysign 1.0e+INF 1.0)");
}

// ---------------------------------------------------------------------------
// logb
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_logb_comprehensive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity("(logb 1.0)");
    assert_oracle_parity("(logb 2.0)");
    assert_oracle_parity("(logb 4.0)");
    assert_oracle_parity("(logb 0.5)");
    assert_oracle_parity("(logb 0.25)");
    assert_oracle_parity("(logb 1024.0)");
    assert_oracle_parity("(logb 3.0)");
    assert_oracle_parity("(logb 1.0e+INF)");
    assert_oracle_parity("(logb 10)");
}

// ---------------------------------------------------------------------------
// Special values: infinity, NaN, negative zero
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_float_special_values() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Infinity arithmetic
    assert_oracle_parity("(+ 1.0e+INF 1.0)");
    assert_oracle_parity("(+ 1.0e+INF 1.0e+INF)");
    assert_oracle_parity("(- 1.0e+INF 1.0e+INF)");
    assert_oracle_parity("(* 1.0e+INF 2.0)");
    assert_oracle_parity("(* 1.0e+INF 0.0)");
    assert_oracle_parity("(* 1.0e+INF -1.0)");
    assert_oracle_parity("(/ 1.0 0.0)");
    assert_oracle_parity("(/ -1.0 0.0)");
    assert_oracle_parity("(/ 0.0 0.0)");

    // NaN propagation
    assert_oracle_parity("(+ 0.0e+NaN 1.0)");
    assert_oracle_parity("(* 0.0e+NaN 0.0)");
    assert_oracle_parity("(- 0.0e+NaN 0.0e+NaN)");

    // Negative zero
    assert_oracle_parity("(+ 0.0 -0.0)");
    assert_oracle_parity("(- 0.0)");
    assert_oracle_parity("(* -1.0 0.0)");
    assert_oracle_parity("(eql 0.0 -0.0)");
    assert_oracle_parity("(= 0.0 -0.0)");
    assert_oracle_parity("(equal 0.0 -0.0)");
}

// ---------------------------------------------------------------------------
// Comparison edge cases with floats
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_float_comparison_edge_cases() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // NaN comparisons: NaN is not equal to anything, not even itself
    assert_oracle_parity("(= 0.0e+NaN 0.0e+NaN)");
    assert_oracle_parity("(< 0.0e+NaN 0.0)");
    assert_oracle_parity("(> 0.0e+NaN 0.0)");
    assert_oracle_parity("(<= 0.0e+NaN 0.0)");
    assert_oracle_parity("(>= 0.0e+NaN 0.0)");
    assert_oracle_parity("(/= 0.0e+NaN 0.0e+NaN)");

    // Infinity comparisons
    assert_oracle_parity("(< 1.0e+INF 1.0e+INF)");
    assert_oracle_parity("(<= 1.0e+INF 1.0e+INF)");
    assert_oracle_parity("(> -1.0e+INF 1.0e+INF)");
    assert_oracle_parity("(< -1.0e+INF 1.0e+INF)");
    assert_oracle_parity("(= 1.0e+INF 1.0e+INF)");

    // Mixed int/float comparisons
    assert_oracle_parity("(= 1 1.0)");
    assert_oracle_parity("(eql 1 1.0)");
    assert_oracle_parity("(equal 1 1.0)");
    assert_oracle_parity("(< 1 1.0000000000001)");
    assert_oracle_parity("(> most-positive-fixnum (float most-positive-fixnum))");
}

// ---------------------------------------------------------------------------
// Chained float rounding combined with arithmetic
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_float_chained_rounding_arithmetic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Combine rounding modes in expressions
    assert_oracle_parity("(+ (floor 2.7) (ceiling 2.3))");
    assert_oracle_parity("(- (round 3.5) (truncate -2.7))");
    assert_oracle_parity("(* (ffloor 2.7) (fceiling 2.3))");
    assert_oracle_parity("(/ (fround 10.0) (ftruncate 3.7))");

    // Nested rounding
    assert_oracle_parity("(floor (ceiling 2.3))");
    assert_oracle_parity("(round (floor -2.7))");
    assert_oracle_parity("(truncate (fround 3.5))");

    // Complex expression with type mixing
    let form = r#"(let* ((a 2.7)
                          (b -3.2)
                          (f (floor a))
                          (c (ceiling b))
                          (r (round (+ a b)))
                          (t2 (truncate (* a b))))
                     (list f c r t2
                           (floatp (ffloor a))
                           (integerp (floor a))))"#;
    assert_oracle_parity(form);
}

// ---------------------------------------------------------------------------
// Two-argument division forms: comprehensive remainder behavior
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_float_two_arg_division_remainder() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Emacs (floor x y) returns quotient; remainder via mod/% semantics
    // Verify quotient * divisor + remainder == dividend
    let form = r#"(let* ((a 17) (b 5)
                          (q-floor (floor a b))
                          (q-ceil (ceiling a b))
                          (q-round (round a b))
                          (q-trunc (truncate a b)))
                     (list q-floor q-ceil q-round q-trunc
                           (- a (* q-floor b))
                           (- a (* q-ceil b))
                           (- a (* q-round b))
                           (- a (* q-trunc b))))"#;
    assert_oracle_parity(form);

    // Negative dividend
    let form2 = r#"(let* ((a -17) (b 5))
                      (list (floor a b) (ceiling a b)
                            (round a b) (truncate a b)))"#;
    assert_oracle_parity(form2);

    // Float arguments
    let form3 = r#"(let* ((a 17.0) (b 3.0))
                      (list (floor a b) (ceiling a b)
                            (round a b) (truncate a b)))"#;
    assert_oracle_parity(form3);

    // Mixed int/float
    assert_oracle_parity("(floor 10 3.0)");
    assert_oracle_parity("(ceiling 10.0 3)");
    assert_oracle_parity("(round 7 2.0)");
    assert_oracle_parity("(truncate 7.0 2)");
}
