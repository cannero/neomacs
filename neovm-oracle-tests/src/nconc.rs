//! Oracle parity tests for `nconc`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{assert_ok_eq, eval_oracle_and_neovm};

#[test]
fn oracle_prop_nconc_basics() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(nconc '(1 2) '(3 4))");
    assert_ok_eq("(1 2 3 4)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(nconc '(a b) '(c) '(d e f))");
    assert_ok_eq("(a b c d e f)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(nconc nil '(5 6))");
    assert_ok_eq("(5 6)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(nconc '(7 8) nil)");
    assert_ok_eq("(7 8)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(nconc nil)");
    assert_ok_eq("nil", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(nconc '(99))");
    assert_ok_eq("(99)", &o, &n);

    let (o, n) = eval_oracle_and_neovm("(nconc nil nil nil '(1))");
    assert_ok_eq("(1)", &o, &n);
}
