//! Oracle parity tests for `while`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use proptest::prelude::*;

use super::common::{ORACLE_PROP_CASES, assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_while_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // counter accumulation
    let (o, n) = eval_oracle_and_neovm(
        "(let ((i 0) (sum 0)) (while (< i 5) (setq sum (+ sum i) i (1+ i))) sum)",
    );
    assert_ok_eq("10", &o, &n);

    // zero iterations
    let (o, n) = eval_oracle_and_neovm("(let ((x 99)) (while nil (setq x 0)) x)");
    assert_ok_eq("99", &o, &n);

    // single iteration
    let (o, n) = eval_oracle_and_neovm(
        "(let ((done nil) (count 0)) (while (not done) (setq count (1+ count) done t)) count)",
    );
    assert_ok_eq("1", &o, &n);

    // while returns nil
    let (o, n) = eval_oracle_and_neovm("(while nil)");
    assert_ok_eq("nil", &o, &n);

    // list consumption
    let (o, n) = eval_oracle_and_neovm(
        "(let ((xs '(10 20 30)) (total 0)) (while xs (setq total (+ total (car xs)) xs (cdr xs))) total)",
    );
    assert_ok_eq("60", &o, &n);
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES))]

    #[test]
    fn oracle_prop_while_countdown(
        limit in 1i64..15i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(let ((i {}) (c 0)) (while (> i 0) (setq c (1+ c) i (1- i))) c)",
            limit
        );
        let expected = format!("{}", limit);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(&expected, &oracle, &neovm);
    }
}
