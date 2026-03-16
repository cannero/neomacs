//! Oracle parity tests for GNU labeled restriction semantics.

use super::common::assert_oracle_parity_with_bootstrap;
use super::common::return_if_neovm_enable_oracle_proptest_not_set;

#[test]
fn oracle_prop_with_restriction_label_restores_stack_and_widen_behavior() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(with-temp-buffer
                    (insert "abcdef")
                    (with-restriction 2 5 :label 'tag
                      (list (point-min) (point-max)
                            (save-restriction
                              (without-restriction :label 'tag
                                (list (point-min) (point-max))))
                            (point-min) (point-max)
                            (progn (widen) (list (point-min) (point-max)))
                            (progn (without-restriction :label 'tag
                                     (list (point-min) (point-max)))))))"#;
    assert_oracle_parity_with_bootstrap(form);
}
