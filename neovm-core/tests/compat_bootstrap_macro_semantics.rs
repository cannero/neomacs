use neovm_core::emacs_core::eval::Context;
use neovm_core::emacs_core::format_eval_result_with_eval;
use neovm_core::emacs_core::parser::parse_forms;
use neovm_core::emacs_core::value::Value;

#[test]
fn compat_bootstrap_macro_cells_exist_in_fresh_context() {
    let ctx = Context::new();
    for name in ["eval-and-compile"] {
        let function = ctx
            .obarray()
            .symbol_function(name)
            .copied()
            .unwrap_or(Value::Nil);
        assert!(
            matches!(function, Value::Macro(_)),
            "{name} should be a bootstrap macro cell"
        );
    }

    for name in [
        "eval-when-compile",
        "defvar-local",
        "track-mouse",
        "with-current-buffer",
        "with-temp-buffer",
        "with-output-to-string",
        "with-syntax-table",
        "with-mutex",
    ] {
        let function = ctx
            .obarray()
            .symbol_function(name)
            .copied()
            .unwrap_or(Value::Nil);
        assert!(
            !matches!(function, Value::Macro(_)),
            "{name} should not be a source-bootstrap macro cell"
        );
    }
}

#[test]
fn compat_bootstrap_macro_cells_execute_before_loadup() {
    let mut ctx = Context::new();
    let forms = parse_forms("(eval-and-compile 42)").expect("parse");
    let result = ctx.eval_expr(&forms[0]);
    let formatted = format_eval_result_with_eval(&ctx, &result);
    assert_eq!(formatted, "OK 42");
}
