use neovm_core::emacs_core::eval::Context;
use neovm_core::emacs_core::format_eval_result_with_eval;
use neovm_core::emacs_core::load::create_source_bootstrap_context;
use neovm_core::emacs_core::parser::parse_forms;
use neovm_core::emacs_core::value::Value;

#[test]
fn compat_bootstrap_macro_cells_are_scoped_to_source_bootstrap() {
    {
        let plain_ctx = Context::new();
        let plain_function = plain_ctx
            .obarray()
            .symbol_function("eval-and-compile")
            .copied()
            .unwrap_or(Value::NIL);
        assert!(
            !matches!(plain_function, Value::Macro(_)),
            "plain Context::new should not seed source-bootstrap macro cells"
        );
    }

    let bootstrap_ctx = create_source_bootstrap_context();

    for name in ["eval-and-compile"] {
        let function = bootstrap_ctx
            .obarray()
            .symbol_function(name)
            .copied()
            .unwrap_or(Value::NIL);
        assert!(
            matches!(function, Value::Macro(_)),
            "{name} should be a bootstrap macro cell"
        );
    }

    for name in [
        "declare",
        "eval-when-compile",
        "defvar-local",
        "track-mouse",
        "with-current-buffer",
        "with-temp-buffer",
        "with-output-to-string",
        "with-syntax-table",
        "with-mutex",
    ] {
        let function = bootstrap_ctx
            .obarray()
            .symbol_function(name)
            .copied()
            .unwrap_or(Value::NIL);
        assert!(
            !matches!(function, Value::Macro(_)),
            "{name} should not be a source-bootstrap macro cell"
        );
    }
}

#[test]
fn compat_bootstrap_macro_cells_execute_before_loadup() {
    let mut ctx = create_source_bootstrap_context();
    let forms = parse_forms("(eval-and-compile 42)").expect("parse");
    let result = ctx.eval_expr(&forms[0]);
    let formatted = format_eval_result_with_eval(&ctx, &result);
    assert_eq!(formatted, "OK 42");
}

#[test]
fn compat_source_bootstrap_context_stays_pre_subr_surface() {
    let bootstrap_ctx = create_source_bootstrap_context();

    for name in ["when", "macroexpand-1"] {
        let function = bootstrap_ctx
            .obarray()
            .symbol_function(name)
            .copied()
            .unwrap_or(Value::NIL);
        assert_eq!(
            function,
            Value::NIL,
            "{name} should remain unavailable before GNU Lisp bootstrap"
        );
    }
}
