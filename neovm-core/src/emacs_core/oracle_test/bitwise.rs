//! Oracle parity tests for bitwise operations: `logand`, `logior`,
//! `logxor`, `lognot`, `ash`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{
    ORACLE_PROP_CASES, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
};

// ---------------------------------------------------------------------------
// logand
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_logand_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(logand #xff #x0f)");
    assert_oracle_parity_with_bootstrap("(logand #b1010 #b1100)");
    assert_oracle_parity_with_bootstrap("(logand 255 0)");
    assert_oracle_parity_with_bootstrap("(logand -1 42)");
}

#[test]
fn oracle_prop_logand_multiple_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(logand #xff #x0f #x03)");
    assert_oracle_parity_with_bootstrap("(logand 255 127 63 31)");
    assert_oracle_parity_with_bootstrap("(logand)");
    assert_oracle_parity_with_bootstrap("(logand 42)");
}

#[test]
fn oracle_prop_logand_negative() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(logand -1 -1)");
    assert_oracle_parity_with_bootstrap("(logand -256 255)");
    assert_oracle_parity_with_bootstrap("(logand -128 127)");
}

// ---------------------------------------------------------------------------
// logior
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_logior_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(logior #x0f #xf0)");
    assert_oracle_parity_with_bootstrap("(logior #b1010 #b0101)");
    assert_oracle_parity_with_bootstrap("(logior 0 0)");
}

#[test]
fn oracle_prop_logior_multiple_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(logior 1 2 4 8 16)");
    assert_oracle_parity_with_bootstrap("(logior)");
    assert_oracle_parity_with_bootstrap("(logior 42)");
}

#[test]
fn oracle_prop_logior_negative() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(logior -1 0)");
    assert_oracle_parity_with_bootstrap("(logior -128 64)");
}

// ---------------------------------------------------------------------------
// logxor
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_logxor_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(logxor #xff #x0f)");
    assert_oracle_parity_with_bootstrap("(logxor #b1010 #b1100)");
    assert_oracle_parity_with_bootstrap("(logxor 42 42)");
    assert_oracle_parity_with_bootstrap("(logxor 0 0)");
}

#[test]
fn oracle_prop_logxor_multiple_args() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(logxor 1 2 4)");
    assert_oracle_parity_with_bootstrap("(logxor)");
    assert_oracle_parity_with_bootstrap("(logxor 42)");
}

#[test]
fn oracle_prop_logxor_self_inverse() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // XOR with itself is always 0
    let form = "(let ((x 12345))
                  (logxor (logxor x 99999) 99999))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("12345", &o, &n);
}

// ---------------------------------------------------------------------------
// lognot
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_lognot_basic() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(lognot 0)");
    assert_oracle_parity_with_bootstrap("(lognot -1)");
    assert_oracle_parity_with_bootstrap("(lognot 1)");
    assert_oracle_parity_with_bootstrap("(lognot 255)");
}

#[test]
fn oracle_prop_lognot_double_negation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(lognot (lognot 42))");
    assert_ok_eq("42", &o, &n);
}

// ---------------------------------------------------------------------------
// ash (arithmetic shift)
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_ash_left_shift() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(ash 1 0)");
    assert_oracle_parity_with_bootstrap("(ash 1 1)");
    assert_oracle_parity_with_bootstrap("(ash 1 8)");
    assert_oracle_parity_with_bootstrap("(ash 1 16)");
    assert_oracle_parity_with_bootstrap("(ash 5 3)");
}

#[test]
fn oracle_prop_ash_right_shift() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(ash 256 -1)");
    assert_oracle_parity_with_bootstrap("(ash 256 -4)");
    assert_oracle_parity_with_bootstrap("(ash 256 -8)");
    assert_oracle_parity_with_bootstrap("(ash 255 -4)");
    assert_oracle_parity_with_bootstrap("(ash 1 -1)");
}

#[test]
fn oracle_prop_ash_negative_values() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Arithmetic shift preserves sign
    assert_oracle_parity_with_bootstrap("(ash -1 1)");
    assert_oracle_parity_with_bootstrap("(ash -256 -4)");
    assert_oracle_parity_with_bootstrap("(ash -128 -1)");
}

// ---------------------------------------------------------------------------
// Complex: bit manipulation patterns
// ---------------------------------------------------------------------------

#[test]
fn oracle_prop_bitwise_flag_manipulation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Set/clear/test flags pattern
    let form = "(let ((flags 0)
                      (read-flag 1)
                      (write-flag 2)
                      (exec-flag 4))
                  ;; Set read and write
                  (setq flags (logior flags read-flag write-flag))
                  (let ((has-read (not (= 0 (logand flags read-flag))))
                        (has-write (not (= 0 (logand flags write-flag))))
                        (has-exec (not (= 0 (logand flags exec-flag)))))
                    ;; Clear write flag
                    (setq flags (logand flags (lognot write-flag)))
                    (let ((after-clear-write
                           (not (= 0 (logand flags write-flag)))))
                      ;; Toggle exec flag
                      (setq flags (logxor flags exec-flag))
                      (let ((has-exec-now
                             (not (= 0 (logand flags exec-flag)))))
                        (list has-read has-write has-exec
                              after-clear-write has-exec-now
                              flags)))))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_bitwise_mask_extraction() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Extract bit fields from packed integer
    let form = "(let ((packed (logior (ash 5 8) (ash 3 4) 7)))
                  (let ((high (logand (ash packed -8) #xff))
                        (mid (logand (ash packed -4) #xf))
                        (low (logand packed #xf)))
                    (list high mid low)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_bitwise_population_count() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Count number of set bits
    let form = "(let ((popcount (lambda (n)
                    (let ((count 0) (val n))
                      (while (> val 0)
                        (when (= 1 (logand val 1))
                          (setq count (1+ count)))
                        (setq val (ash val -1)))
                      count))))
                  (list (funcall popcount 0)
                        (funcall popcount 1)
                        (funcall popcount 7)
                        (funcall popcount 255)
                        (funcall popcount 256)))";
    assert_oracle_parity_with_bootstrap(form);
}

// ---------------------------------------------------------------------------
// proptest: logand/logior/logxor identities
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_bitwise_demorgan(
        a in 0i64..65536,
        b in 0i64..65536,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        // De Morgan's law: ~(a & b) == (~a | ~b) — for positive range
        let form = format!(
            "(= (lognot (logand {} {}))
                (logior (lognot {}) (lognot {})))",
            a, b, a, b
        );
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        prop_assert_eq!(neovm.as_str(), "OK t");
        prop_assert_eq!(oracle.as_str(), "OK t");
    }
}
