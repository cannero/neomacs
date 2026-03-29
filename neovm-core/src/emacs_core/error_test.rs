use super::EvalError;
use crate::emacs_core::intern::intern;
use crate::emacs_core::{
    Context, Value, parse_forms, print_value_bytes_with_eval, print_value_with_eval,
};

#[test]
fn list_prints_buffers_with_names_in_eval_context() -> Result<(), EvalError> {
    let mut eval = Context::new();
    let stale = Value::Buffer(eval.buffers.create_buffer("stale-win-buf"));
    eval.set_variable("vm-stale-win-buf", stale);
    let forms = parse_forms(
        "(let ((b vm-stale-win-buf)
           (w (selected-window)))
  (set-window-buffer nil b)
  (kill-buffer b)
  (list (window-buffer) (window-start) (window-point)))",
    )
    .map_err(|err| EvalError::Signal {
        symbol: intern("parse-error"),
        data: vec![Value::string(err.to_string())],
        raw_data: None,
    })?;
    let mut value = Value::Nil;
    for form in &forms {
        value = eval.eval_expr(form).expect("evaluation should succeed");
    }

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
    let mut eval = Context::new();
    let forms = parse_forms(
        "(with-temp-buffer
           (condition-case err
               (key-binding 1 nil nil 0)
             (error err)))",
    )
    .map_err(|err| EvalError::Signal {
        symbol: intern("parse-error"),
        data: vec![Value::string(err.to_string())],
        raw_data: None,
    })?;
    let value = eval.eval_expr(&forms[0])?;

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
    let mut eval = Context::new();
    let forms =
        parse_forms("(make-mutex \"error-printer-mutex\")").map_err(|err| EvalError::Signal {
            symbol: intern("parse-error"),
            data: vec![Value::string(err.to_string())],
            raw_data: None,
        })?;
    let value = eval.eval_expr(&forms[0])?;
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
    let mut eval = Context::new();
    let forms = parse_forms(
        "(let ((m (make-mutex \"error-printer-mutex\")))
           (make-condition-variable m \"error-printer-condvar\"))",
    )
    .map_err(|err| EvalError::Signal {
        symbol: intern("parse-error"),
        data: vec![Value::string(err.to_string())],
        raw_data: None,
    })?;
    let value = eval.eval_expr(&forms[0])?;
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
    let mut eval = Context::new();
    let forms = parse_forms("(list (selected-frame) (selected-window))").map_err(|err| {
        EvalError::Signal {
            symbol: intern("parse-error"),
            data: vec![Value::string(err.to_string())],
            raw_data: None,
        }
    })?;
    let value = eval.eval_expr(&forms[0])?;
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
    let mut eval = Context::new();
    let forms = parse_forms(
        "(list (selected-window)
               (condition-case err (frame-terminal (selected-window)) (error err))
               (condition-case err (tty-type (selected-window)) (error err))
               (condition-case err (terminal-name (selected-window)) (error err)))",
    )
    .map_err(|err| EvalError::Signal {
        symbol: intern("parse-error"),
        data: vec![Value::string(err.to_string())],
        raw_data: None,
    })?;
    let value = eval.eval_expr(&forms[0])?;
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
    let mut eval = Context::new();
    let forms = parse_forms("(list (car (terminal-list)) (current-thread))").map_err(|err| {
        EvalError::Signal {
            symbol: intern("parse-error"),
            data: vec![Value::string(err.to_string())],
            raw_data: None,
        }
    })?;
    let value = eval.eval_expr(&forms[0])?;
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
    let eval = Context::new();
    let raw_unquote = Value::list(vec![Value::symbol(","), Value::symbol("x")]);
    let nested = Value::list(vec![
        Value::symbol("`"),
        Value::list(vec![Value::symbol("a"), raw_unquote]),
    ]);

    assert_eq!(print_value_with_eval(&eval, &raw_unquote), "(\\, x)");
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &raw_unquote)).unwrap(),
        "(\\, x)"
    );
    assert_eq!(print_value_with_eval(&eval, &nested), "`(a ,x)");
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &nested)).unwrap(),
        "`(a ,x)"
    );
}

// --- signal / Flow tests ---

#[test]
fn signal_creates_signal_data() {
    use super::{Flow, signal};
    let flow = signal(
        "wrong-type-argument",
        vec![Value::symbol("stringp"), Value::Int(42)],
    );
    match flow {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data.len(), 2);
            assert!(sig.raw_data.is_none());
        }
        _ => panic!("expected Flow::Signal"),
    }
}

#[test]
fn signal_with_data_preserves_raw() {
    use super::{Flow, signal_with_data};
    let dotted = Value::cons(Value::symbol("foo"), Value::Int(1));
    let flow = signal_with_data("error", dotted);
    match flow {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert!(sig.raw_data.is_some());
        }
        _ => panic!("expected Flow::Signal"),
    }
}

#[test]
fn make_signal_binding_value_preserves_raw_payload_shape() {
    use super::{Flow, make_signal_binding_value, signal_with_data};

    let flow = signal_with_data("error", Value::Int(1));
    let Flow::Signal(sig) = flow else { panic!() };
    assert_eq!(
        make_signal_binding_value(&sig),
        Value::cons(Value::symbol("error"), Value::Int(1))
    );
}

#[test]
fn format_eval_result_preserves_raw_signal_payload_shape() {
    use super::{format_eval_result, map_flow, signal_with_data};

    let result = Err(map_flow(signal_with_data("error", Value::Int(1))));
    assert_eq!(format_eval_result(&result), "ERR (error 1)");
}

#[test]
fn signal_matches_symbol() {
    use super::signal_matches;
    use crate::emacs_core::expr::Expr;
    use crate::emacs_core::intern::intern;

    let pattern = Expr::Symbol(intern("wrong-type-argument"));
    // Exact match
    assert!(signal_matches(&pattern, "wrong-type-argument"));
    // "error" symbol matches any signal (catch-all)
    let error_pattern = Expr::Symbol(intern("error"));
    assert!(signal_matches(&error_pattern, "any-signal"));
}

#[test]
fn signal_matches_t_matches_all() {
    use super::signal_matches;
    use crate::emacs_core::expr::Expr;
    use crate::emacs_core::intern::intern;

    let t_pattern = Expr::Symbol(intern("t"));
    assert!(signal_matches(&t_pattern, "void-variable"));
    assert!(signal_matches(&t_pattern, "wrong-type-argument"));
}

#[test]
fn make_signal_binding_value_structure() {
    use super::{Flow, make_signal_binding_value, signal};
    use crate::emacs_core::value::list_to_vec;

    let flow = signal("void-variable", vec![Value::symbol("x")]);
    let Flow::Signal(sig) = flow else { panic!() };
    let binding = make_signal_binding_value(&sig);
    // Should be (void-variable x)
    let items = list_to_vec(&binding).expect("should be a proper list");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].as_symbol_name(), Some("void-variable"));
    assert_eq!(items[1].as_symbol_name(), Some("x"));
}

#[test]
fn eval_context_printer_renders_hash_s_literal_shorthand() {
    let eval = Context::new();
    let literal = Value::list(vec![
        Value::symbol("make-hash-table-from-literal"),
        Value::list(vec![
            Value::symbol("quote"),
            Value::list(vec![Value::symbol("x")]),
        ]),
    ]);
    assert_eq!(print_value_with_eval(&eval, &literal), "#s(x)");
    assert_eq!(
        String::from_utf8(print_value_bytes_with_eval(&eval, &literal)).unwrap(),
        "#s(x)"
    );
}
