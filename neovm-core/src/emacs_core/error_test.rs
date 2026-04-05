use super::EvalError;
use crate::emacs_core::{
    Context, Value, print_value_bytes_with_eval, print_value_with_eval,
};

#[test]
fn list_prints_buffers_with_names_in_eval_context() -> Result<(), EvalError> {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let stale = Value::make_buffer(eval.buffers.create_buffer("stale-win-buf"));
    eval.set_variable("vm-stale-win-buf", stale);
    let value = eval.eval_str(
        "(let ((b vm-stale-win-buf)
           (w (selected-window)))
  (set-window-buffer nil b)
  (kill-buffer b)
  (list (window-buffer) (window-start) (window-point)))",
    )?;

    assert_eq!(
        print_value_with_eval(&eval, &value),
        "(#<buffer *scratch*> 1 1)"
    );
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &value)).unwrap(),
        "(#<buffer *scratch*> 1 1)"
    );

    Ok(())
}

#[test]
fn eval_context_printer_renders_killed_buffer_handles() -> Result<(), EvalError> {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let value = eval.eval_str(
        "(with-temp-buffer
           (condition-case err
               (key-binding 1 nil nil 0)
             (error err)))",
    )?;

    assert_eq!(
        print_value_with_eval(&eval, &value),
        "(args-out-of-range #<killed buffer> 0)"
    );
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &value)).unwrap(),
        "(args-out-of-range #<killed buffer> 0)"
    );

    Ok(())
}

#[test]
fn eval_context_printer_renders_mutex_handles_consistently() -> Result<(), EvalError> {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let value = eval.eval_str(r#"(make-mutex "error-printer-mutex")"#)?;
    let printed = print_value_with_eval(&eval, &value);

    assert!(printed.starts_with("#<mutex "));
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &value)).unwrap(),
        printed
    );

    Ok(())
}

#[test]
fn eval_context_printer_renders_condvar_handles_consistently() -> Result<(), EvalError> {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let value = eval.eval_str(
        r#"(let ((m (make-mutex "error-printer-mutex")))
           (make-condition-variable m "error-printer-condvar"))"#,
    )?;
    let printed = print_value_with_eval(&eval, &value);

    assert!(printed.starts_with("#<condvar "));
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &value)).unwrap(),
        printed
    );

    Ok(())
}

#[test]
fn eval_context_printer_renders_frame_window_handles_consistently() -> Result<(), EvalError> {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let value = eval.eval_str("(list (selected-frame) (selected-window))")?;
    let printed = print_value_with_eval(&eval, &value);

    assert!(printed.starts_with("(#<frame"));
    assert!(printed.contains("#<window"));
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &value)).unwrap(),
        printed
    );

    Ok(())
}

#[test]
fn eval_context_printer_renders_window_handles_with_buffer_names() -> Result<(), EvalError> {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let value = eval.eval_str(
        "(list (selected-window)
               (condition-case err (frame-terminal (selected-window)) (error err))
               (condition-case err (tty-type (selected-window)) (error err))
               (condition-case err (terminal-name (selected-window)) (error err)))",
    )?;
    let printed = print_value_with_eval(&eval, &value);

    assert!(printed.contains("on *scratch*>"));
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &value)).unwrap(),
        printed
    );

    Ok(())
}

#[test]
fn eval_context_printer_renders_terminal_thread_handles_consistently() -> Result<(), EvalError> {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let value = eval.eval_str("(list (car (terminal-list)) (current-thread))")?;
    let printed = print_value_with_eval(&eval, &value);

    assert!(printed.starts_with("(#<terminal"));
    assert!(printed.contains("#<thread"));
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &value)).unwrap(),
        printed
    );

    Ok(())
}

#[test]
fn eval_context_printer_matches_gnu_backquote_shorthand_rules() {
    crate::test_utils::init_test_tracing();
    let eval = Context::new();
    let raw_unquote = Value::list(vec![Value::symbol(","), Value::symbol("x")]);
    let nested = Value::list(vec![
        Value::symbol("`"),
        Value::list(vec![Value::symbol("a"), raw_unquote]),
    ]);
    assert_eq!(print_value_with_eval(&eval, &nested), "(\\` (a (\\, x)))");
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &nested)).unwrap(),
        "(\\` (a (\\, x)))"
    );
}
