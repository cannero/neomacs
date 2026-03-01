//! Oracle parity tests for advice functions.

use super::common::{
    assert_oracle_parity_with_bootstrap, return_if_neovm_enable_oracle_proptest_not_set,
};

#[test]
fn oracle_prop_advice_add_remove_member_lifecycle() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((target 'neovm--adv-target) (adv 'neovm--adv-fn)) (fset target (lambda (x) x)) (fset adv (lambda (&rest _) nil)) (unwind-protect (progn (advice-add target :before adv) (list (not (null (advice-member-p adv target))) (progn (advice-remove target adv) (not (null (advice-member-p adv target)))))) (fmakunbound target) (fmakunbound adv)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_advice_unknown_where_keyword_error_shape() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(
        "(condition-case err (advice-add 'car :neovm-unknown #'ignore) (error err))",
    );
}

#[test]
fn oracle_prop_advice_wrong_arity_error_shapes() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(condition-case err (advice-add 'car :before) (error err))");
    assert_oracle_parity_with_bootstrap("(condition-case err (advice-remove 'car) (error err))");
    assert_oracle_parity_with_bootstrap("(condition-case err (advice-member-p 'ignore) (error err))");
}

#[test]
fn oracle_prop_advice_target_type_error_shape() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(condition-case err (advice-add 1 :before #'ignore) (error err))");
}

#[test]
fn oracle_prop_advice_before_observes_call_arguments() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((target 'neovm--adv-target) (before 'neovm--adv-before) (log nil)) (fset target (lambda (x) (setq log (cons (list 'orig x) log)) x)) (fset before (lambda (&rest args) (setq log (cons (cons 'before args) log)))) (unwind-protect (progn (advice-add target :before before) (funcall target 7) (nreverse log)) (fmakunbound target) (fmakunbound before)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_advice_around_wraps_original_result() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((target 'neovm--adv-around-target) (around 'neovm--adv-around)) (fset target (lambda (x) (* x 2))) (fset around (lambda (orig x) (+ 10 (funcall orig (1+ x))))) (unwind-protect (progn (advice-add target :around around) (funcall target 3)) (fmakunbound target) (fmakunbound around)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_advice_override_replaces_original_function() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((target 'neovm--adv-override-target) (override 'neovm--adv-override)) (fset target (lambda (x) (+ x 1))) (fset override (lambda (&rest _) 'override-hit)) (unwind-protect (progn (advice-add target :override override) (funcall target 11)) (fmakunbound target) (fmakunbound override)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_advice_filter_args_rewrites_argument_list() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((target 'neovm--adv-filter-args-target) (filter 'neovm--adv-filter-args)) (fset target (lambda (a b) (+ a b))) (fset filter (lambda (args) (list (* 2 (car args)) (* 3 (car (cdr args)))))) (unwind-protect (progn (advice-add target :filter-args filter) (funcall target 2 5)) (fmakunbound target) (fmakunbound filter)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_advice_filter_return_rewrites_result() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((target 'neovm--adv-filter-ret-target) (filter 'neovm--adv-filter-ret)) (fset target (lambda (x) (* x 2))) (fset filter (lambda (ret) (+ ret 9))) (unwind-protect (progn (advice-add target :filter-return filter) (funcall target 3)) (fmakunbound target) (fmakunbound filter)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_advice_runs_when_target_is_called_via_apply() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((target 'neovm--adv-apply-target) (before 'neovm--adv-apply-before) (log nil)) (fset target (lambda (a b) (setq log (cons (list 'orig a b) log)) (+ a b))) (fset before (lambda (&rest args) (setq log (cons (cons 'before args) log)))) (unwind-protect (progn (advice-add target :before before) (list (apply target '(4 9)) (nreverse log))) (fmakunbound target) (fmakunbound before)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_advice_remove_restores_unadvised_behavior() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((target 'neovm--adv-restore-target) (before 'neovm--adv-restore-before) (log nil)) (fset target (lambda (x) (setq log (cons (list 'orig x) log)) x)) (fset before (lambda (&rest args) (setq log (cons (cons 'before args) log)))) (unwind-protect (progn (advice-add target :before before) (funcall target 1) (advice-remove target before) (funcall target 2) (nreverse log)) (fmakunbound target) (fmakunbound before)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_advice_before_and_after_ordering() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((target 'neovm--adv-order-target) (before 'neovm--adv-order-before) (after 'neovm--adv-order-after) (log nil)) (fset target (lambda (x) (setq log (cons (list 'orig x) log)) x)) (fset before (lambda (&rest args) (setq log (cons (cons 'before args) log)))) (fset after (lambda (&rest args) (setq log (cons (cons 'after args) log)))) (unwind-protect (progn (advice-add target :before before) (advice-add target :after after) (funcall target 5) (nreverse log)) (fmakunbound target) (fmakunbound before) (fmakunbound after)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_advice_non_callable_advice_function_error_shape() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(condition-case err (advice-add 'car :before 1) (error err))");
}
