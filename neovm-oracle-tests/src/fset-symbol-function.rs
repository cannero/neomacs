//! Oracle parity tests for `fset`, `symbol-function`, `fboundp`, `fmakunbound`.

use super::common::return_if_neovm_enable_oracle_proptest_not_set;

use super::common::{
    assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap, eval_oracle_and_neovm,
    run_neovm_eval, run_oracle_eval,
};

#[test]
fn oracle_prop_fset_and_funcall() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (fset 'neovm--test-fset-fn (lambda (x) (+ x 1)))
                  (unwind-protect
                      (funcall 'neovm--test-fset-fn 5)
                    (fmakunbound 'neovm--test-fset-fn)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("6", &o, &n);
}

#[test]
fn oracle_prop_symbol_function_retrieves_definition() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (fset 'neovm--test-sf-fn (lambda (x) (* x 2)))
                  (unwind-protect
                      (funcall (symbol-function 'neovm--test-sf-fn) 7)
                    (fmakunbound 'neovm--test-sf-fn)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("14", &o, &n);
}

#[test]
fn oracle_prop_fboundp_true() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (fset 'neovm--test-fbp-fn (lambda () 'hi))
                  (unwind-protect
                      (fboundp 'neovm--test-fbp-fn)
                    (fmakunbound 'neovm--test-fbp-fn)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("t", &o, &n);
}

#[test]
fn oracle_prop_fboundp_false() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (o, n) = eval_oracle_and_neovm("(fboundp 'neovm--surely-unbound-symbol-xyz)");
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_fmakunbound_makes_unbound() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (fset 'neovm--test-fmub-fn (lambda () 42))
                  (fmakunbound 'neovm--test-fmub-fn)
                  (fboundp 'neovm--test-fmub-fn))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("nil", &o, &n);
}

#[test]
fn oracle_prop_fset_overwrite() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn
                  (fset 'neovm--test-fow-fn (lambda (x) (+ x 1)))
                  (unwind-protect
                      (progn
                        (let ((r1 (funcall 'neovm--test-fow-fn 5)))
                          (fset 'neovm--test-fow-fn (lambda (x) (* x 10)))
                          (list r1 (funcall 'neovm--test-fow-fn 5))))
                    (fmakunbound 'neovm--test-fow-fn)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("(6 50)", &o, &n);
}

#[test]
fn oracle_prop_symbol_function_on_unbound() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(condition-case err
                  (symbol-function 'neovm--definitely-unbound-xyz)
                  (void-function (car err)))";
    // GNU Emacs `symbol-function` on an unbound symbol signals void-function,
    // and condition-case catches it. The oracle returns OK nil because
    // `(car err)` on a void-function error yields the symbol `void-function`,
    // but Emacs normalizes the condition-case result to nil in this context.
    // Both should agree on the result.
    let (o, n) = eval_oracle_and_neovm(form);
    assert_eq!(n, o, "neovm and oracle should match");
}

#[test]
fn oracle_prop_fset_with_builtin() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Assign a built-in function to a new symbol
    let form = "(progn
                  (fset 'neovm--test-alias (symbol-function '+))
                  (unwind-protect
                      (funcall 'neovm--test-alias 1 2 3)
                    (fmakunbound 'neovm--test-alias)))";
    let (o, n) = eval_oracle_and_neovm(form);
    assert_ok_eq("6", &o, &n);
}

#[test]
fn oracle_prop_indirect_function() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    // Function aliasing via fset to a symbol
    let form = "(progn
                  (fset 'neovm--test-orig (lambda (x) (+ x 100)))
                  (fset 'neovm--test-alias2 'neovm--test-orig)
                  (unwind-protect
                      (funcall 'neovm--test-alias2 5)
                    (fmakunbound 'neovm--test-orig)
                    (fmakunbound 'neovm--test-alias2)))";
    assert_oracle_parity_with_bootstrap(form);
}
