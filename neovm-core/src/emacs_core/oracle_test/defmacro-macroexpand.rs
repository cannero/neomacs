//! Oracle parity tests for `defmacro` and `macroexpand`.

use proptest::prelude::*;
use std::sync::OnceLock;

use super::common::{
    ORACLE_PROP_CASES, assert_err_kind, assert_ok_eq, assert_oracle_parity_with_bootstrap,
    eval_oracle_and_neovm, return_if_neovm_enable_oracle_proptest_not_set,
};

fn oracle_defmacro_macroexpand_proptest_failure_path() -> &'static str {
    static PATH: OnceLock<&'static str> = OnceLock::new();
    PATH.get_or_init(|| {
        let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".to_string());
        Box::leak(
            format!("{target_dir}/proptest-regressions/emacs_core/oracle/defmacro-macroexpand.txt")
                .into_boxed_str(),
        )
    })
}

#[test]
fn oracle_prop_defmacro_basic_definition_and_invocation() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn (defmacro neovm--dm-basic (x) (list 'list x x)) (unwind-protect (neovm--dm-basic 4) (fmakunbound 'neovm--dm-basic)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(4 4)", &oracle, &neovm);
}

#[test]
fn oracle_prop_macroexpand_nonmacro_form_passthrough() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle, neovm) = eval_oracle_and_neovm("(macroexpand '(list 1 2 3))");
    assert_ok_eq("(list 1 2 3)", &oracle, &neovm);
}

#[test]
fn oracle_prop_macroexpand_recursively_expands_user_macros() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn (defmacro neovm--dm-a (x) (list 'neovm--dm-b x)) (defmacro neovm--dm-b (x) (list '+ x 1)) (unwind-protect (macroexpand '(neovm--dm-a 5)) (fmakunbound 'neovm--dm-a) (fmakunbound 'neovm--dm-b)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(+ 5 1)", &oracle, &neovm);
}

#[test]
fn oracle_prop_macroexpand_rest_macro_builds_list_form() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn (defmacro neovm--dm-list (&rest xs) (cons 'list xs)) (unwind-protect (macroexpand '(neovm--dm-list 1 2 3)) (fmakunbound 'neovm--dm-list)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(list 1 2 3)", &oracle, &neovm);
}

#[test]
fn oracle_prop_macroexpand_environment_shadow_and_override() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let shadow_form = "(progn (defmacro neovm--dm-shadow (x) (list '+ x 1)) (unwind-protect (macroexpand '(neovm--dm-shadow 9) '((neovm--dm-shadow . nil))) (fmakunbound 'neovm--dm-shadow)))";
    let (oracle_shadow, neovm_shadow) = eval_oracle_and_neovm(shadow_form);
    assert_ok_eq("(neovm--dm-shadow 9)", &oracle_shadow, &neovm_shadow);

    let override_form =
        "(macroexpand '(neovm--dm-env 7) '((neovm--dm-env . (lambda (x) (list 'quote x)))))";
    let (oracle_override, neovm_override) = eval_oracle_and_neovm(override_form);
    assert_ok_eq("'7", &oracle_override, &neovm_override);
}

#[test]
fn oracle_prop_defmacro_and_macroexpand_error_shapes() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap("(condition-case err (defmacro) (error err))");
    assert_oracle_parity_with_bootstrap("(condition-case err (defmacro 1 nil) (error err))");
    assert_oracle_parity_with_bootstrap(
        "(condition-case err (defmacro 'vm-oracle-dm nil 1) (error err))",
    );
    assert_oracle_parity_with_bootstrap(
        "(condition-case err (macroexpand '(when t 1) 1) (error err))",
    );
}

#[test]
fn oracle_prop_macrop_reflects_defmacro_bindings() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn (defmacro neovm--dm-macrop (x) x) (fset 'neovm--dm-fn (lambda (x) x)) (unwind-protect (list (macrop 'neovm--dm-macrop) (macrop 'neovm--dm-fn)) (fmakunbound 'neovm--dm-macrop) (fmakunbound 'neovm--dm-fn)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(t nil)", &oracle, &neovm);
}

#[test]
fn oracle_prop_macroexpand_improper_macro_call_error_shape() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn (defmacro neovm--dm-improper (x) x) (unwind-protect (condition-case err (macroexpand '(neovm--dm-improper . 1)) (error err)) (fmakunbound 'neovm--dm-improper)))";
    assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn oracle_prop_macroexpand_backquote_splice_shape() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn (defmacro neovm--dm-bq (head &rest tail) `(list ,head ,@tail)) (unwind-protect (macroexpand '(neovm--dm-bq 1 2 3 4)) (fmakunbound 'neovm--dm-bq)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("(list 1 2 3 4)", &oracle, &neovm);
}

#[test]
fn oracle_prop_macroexpand_environment_symbol_callable() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn (fset 'neovm--dm-env-expander (lambda (x) (list 'quote (list x x)))) (unwind-protect (macroexpand '(neovm--dm-env-sym 7) '((neovm--dm-env-sym . neovm--dm-env-expander))) (fmakunbound 'neovm--dm-env-expander)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("'(7 7)", &oracle, &neovm);
}

#[test]
fn oracle_prop_macroexpand_environment_invalid_callable_error_shape() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    assert_oracle_parity_with_bootstrap(
        "(condition-case err (macroexpand '(neovm--dm-bad-env 1) '((neovm--dm-bad-env . 1))) (error err))",
    );
}

#[test]
fn oracle_prop_macroexpand_fallback_macro_shadow_and_override_via_env() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let (oracle_shadow, neovm_shadow) =
        eval_oracle_and_neovm("(macroexpand '(when t 1 2) '((when . nil)))");
    assert_ok_eq("(when t 1 2)", &oracle_shadow, &neovm_shadow);

    let override_form = "(macroexpand '(when t 1 2) '((when . (lambda (cond &rest body) (list 'if cond (cons 'progn body) 'override-tail)))))";
    let (oracle_override, neovm_override) = eval_oracle_and_neovm(override_form);
    assert_ok_eq(
        "(if t (progn 1 2) override-tail)",
        &oracle_override,
        &neovm_override,
    );
}

#[test]
fn oracle_prop_macroexpand_quoted_nested_macro_payload_not_walked() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn (defmacro neovm--dm-inner (x) (list '+ x 1)) (defmacro neovm--dm-outer () ''(neovm--dm-inner 9)) (unwind-protect (macroexpand '(neovm--dm-outer)) (fmakunbound 'neovm--dm-inner) (fmakunbound 'neovm--dm-outer)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_ok_eq("'(neovm--dm-inner 9)", &oracle, &neovm);
}

#[test]
fn oracle_prop_defmacro_expansion_arity_error_shape() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(progn (defmacro neovm--dm-arity (x y) (list '+ x y)) (unwind-protect (macroexpand '(neovm--dm-arity 1)) (fmakunbound 'neovm--dm-arity)))";
    let (oracle, neovm) = eval_oracle_and_neovm(form);
    assert_err_kind(&oracle, &neovm, "wrong-number-of-arguments");
}

#[test]
fn oracle_prop_macroexpand_stops_at_fixpoint_with_identity_expander() {
    return_if_neovm_enable_oracle_proptest_not_set!();

    let form = "(let ((calls 0)) (fset 'neovm--dm-id (lambda (&rest form) (setq calls (1+ calls)) (cons 'neovm--dm-id form))) (unwind-protect (condition-case err (macroexpand '(neovm--dm-id 1)) (error (list 'err err calls))) (fmakunbound 'neovm--dm-id)))";
    assert_oracle_parity_with_bootstrap(form);
}

proptest! {
    #![proptest_config({
        let mut config = proptest::test_runner::Config::with_cases(ORACLE_PROP_CASES);
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(
                oracle_defmacro_macroexpand_proptest_failure_path(),
            ),
        ));
        config
    })]

    #[test]
    fn oracle_prop_macroexpand_rest_macro_shape(
        a in -10_000i64..10_000i64,
        b in -10_000i64..10_000i64,
        c in -10_000i64..10_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(progn (defmacro neovm--dm-prop-list (&rest xs) (cons 'list xs)) (unwind-protect (macroexpand '(neovm--dm-prop-list {} {} {})) (fmakunbound 'neovm--dm-prop-list)))",
            a, b, c
        );
        let expected = format!("(list {} {} {})", a, b, c);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_macroexpand_recursive_macro_shape(
        n in -10_000i64..10_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(progn (defmacro neovm--dm-prop-a (x) (list 'neovm--dm-prop-b x)) (defmacro neovm--dm-prop-b (x) (list '+ x 1)) (unwind-protect (macroexpand '(neovm--dm-prop-a {})) (fmakunbound 'neovm--dm-prop-a) (fmakunbound 'neovm--dm-prop-b)))",
            n
        );
        let expected = format!("(+ {} 1)", n);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }

    #[test]
    fn oracle_prop_macroexpand_env_symbol_callable_shape(
        n in -10_000i64..10_000i64,
    ) {
        return_if_neovm_enable_oracle_proptest_not_set!(Ok(()));

        let form = format!(
            "(progn (fset 'neovm--dm-prop-env-expander (lambda (x) (list 'quote (list x x)))) (unwind-protect (macroexpand '(neovm--dm-prop-env {}) '((neovm--dm-prop-env . neovm--dm-prop-env-expander))) (fmakunbound 'neovm--dm-prop-env-expander)))",
            n
        );
        let expected = format!("'({} {})", n, n);
        let (oracle, neovm) = eval_oracle_and_neovm(&form);
        assert_ok_eq(expected.as_str(), &oracle, &neovm);
    }
}
