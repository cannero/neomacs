//! Oracle parity tests for core special-form semantics that are easy to miss:
//! `quote`, `function`, `defconst`, `save-current-buffer`, `interactive`, and `inline`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{eval_oracle_and_neovm, eval_oracle_and_neovm_with_bootstrap};

#[test]
fn oracle_prop_special_forms_semantics_quote() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  (quote (a b c))
  '(1 . 2)
  (eq 'foo (quote foo))
  (equal '(1 (2 3) "x") (quote (1 (2 3) "x")))
  (quote nil)
  (quote t))"#;
    let (oracle, neovm) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}

#[test]
fn oracle_prop_special_forms_semantics_function() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  (funcall (function (lambda (x) (+ x 1))) 41)
  (funcall (function car) '(9 8 7))
  (functionp (function car))
  (function 1)
  (function '(1 2 3)))"#;
    let (oracle, neovm) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}

#[test]
fn oracle_prop_special_forms_semantics_defconst() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (makunbound 'neovm--oracle-special-defconst)
  (let ((first (progn
                 (defconst neovm--oracle-special-defconst 10 "doc")
                 neovm--oracle-special-defconst))
        (second (progn
                  (defconst neovm--oracle-special-defconst 20 "doc2")
                  neovm--oracle-special-defconst))
        (is-bound (boundp 'neovm--oracle-special-defconst)))
    (makunbound 'neovm--oracle-special-defconst)
    (list first second is-bound)))"#;
    let (oracle, neovm) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}

#[test]
fn oracle_prop_special_forms_semantics_save_current_buffer() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(let ((orig (current-buffer))
      (a (generate-new-buffer " *neovm-oracle-scb-a*"))
      (b (generate-new-buffer " *neovm-oracle-scb-b*")))
  (unwind-protect
      (progn
        (set-buffer a)
        (insert "A")
        (set-buffer b)
        (insert "B")
        (set-buffer orig)
        (list
         (save-current-buffer
           (set-buffer a)
           (list (eq (current-buffer) a)
                 (buffer-string)))
         (eq (current-buffer) orig)
         (save-current-buffer
           (set-buffer b)
           (insert "!")
           (buffer-string))
         (with-current-buffer b (buffer-string))
         (eq (current-buffer) orig)))
    (kill-buffer a)
    (kill-buffer b)))"#;
    let (oracle, neovm) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}

#[test]
fn oracle_prop_special_forms_semantics_interactive() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(progn
  (fset 'neovm--oracle-interactive-cmd
        (lambda (n)
          (interactive "p")
          n))
  (let ((result
         (list
          (commandp 'neovm--oracle-interactive-cmd)
          (interactive-form 'neovm--oracle-interactive-cmd)
          (funcall 'neovm--oracle-interactive-cmd 7))))
    (fmakunbound 'neovm--oracle-interactive-cmd)
    result))"#;
    let (oracle, neovm) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}

#[test]
fn oracle_prop_special_forms_semantics_inline() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = r#"(list
  (condition-case err
      (progn (inline foo) 'ok)
    (error (list (car err) (cadr err))))
  (inline 'foo)
  (progn
    (defvar foo 1)
    (inline foo)
    'ok))"#;
    let (oracle, neovm) = eval_oracle_and_neovm_with_bootstrap(form);
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}
