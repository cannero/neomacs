//! Oracle parity for `command-modes`.

use super::common::assert_oracle_parity_with_bootstrap;
use super::common::return_if_neovm_enable_oracle_proptest_not_set;

#[test]
fn oracle_prop_command_modes_symbol_property_lambda_and_bytecode() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"
(progn
  (fset 'neovm--test-cm-lambda
        '(lambda () (interactive "p" text-mode prog-mode) t))
  (fset 'neovm--test-cm-target '(lambda () t))
  (fset 'neovm--test-cm-alias 'neovm--test-cm-target)
  (put 'neovm--test-cm-alias 'command-modes '(foo-mode bar-mode))
  (let ((f (make-byte-code '() "" [] 0 nil [nil (rust-ts-mode c-mode)])))
    (fset 'neovm--test-cm-bytecode f))

  (unwind-protect
      (list
        (command-modes 'neovm--test-cm-lambda)
        (command-modes 'neovm--test-cm-alias)
        (command-modes 'neovm--test-cm-bytecode)
        (command-modes 'ignore)
        (command-modes 'car))
    (fmakunbound 'neovm--test-cm-lambda)
    (fmakunbound 'neovm--test-cm-target)
    (fmakunbound 'neovm--test-cm-alias)
    (fmakunbound 'neovm--test-cm-bytecode)))
"#;

    assert_oracle_parity_with_bootstrap(form);
}
