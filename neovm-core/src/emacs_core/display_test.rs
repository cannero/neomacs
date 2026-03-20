use super::*;
use crate::emacs_core::dispnew::pure::{
    builtin_internal_show_cursor, builtin_internal_show_cursor_eval,
    builtin_internal_show_cursor_p, builtin_internal_show_cursor_p_eval, builtin_open_termscript,
    builtin_redraw_frame, builtin_redraw_frame_eval, builtin_send_string_to_terminal,
    builtin_send_string_to_terminal_eval, reset_dispnew_thread_locals,
};
use crate::emacs_core::intern::resolve_sym;
use crate::emacs_core::terminal::pure::{
    builtin_controlling_tty_p, builtin_controlling_tty_p_eval, builtin_frame_terminal,
    builtin_frame_terminal_eval, builtin_resume_tty, builtin_resume_tty_eval,
    builtin_selected_terminal, builtin_set_terminal_parameter, builtin_set_terminal_parameter_eval,
    builtin_suspend_tty, builtin_suspend_tty_eval, builtin_terminal_live_p,
    builtin_terminal_live_p_eval, builtin_terminal_name, builtin_terminal_name_eval,
    builtin_terminal_parameter, builtin_terminal_parameter_eval, builtin_terminal_parameters,
    builtin_terminal_parameters_eval, builtin_tty_top_frame, builtin_tty_top_frame_eval,
    builtin_tty_type, builtin_tty_type_eval, reset_terminal_thread_locals, terminal_handle_value,
};

fn clear_terminal_parameters() {
    reset_terminal_thread_locals();
}

#[test]
fn x_window_system_active_falls_back_to_window_system_when_initial_is_nil() {
    let mut eval = crate::emacs_core::Evaluator::new();
    eval.set_variable("initial-window-system", Value::Nil);
    eval.set_variable("window-system", Value::symbol(gui_window_system_symbol()));

    assert!(x_window_system_active(&eval));
    assert!(x_window_system_active_in_state(
        &eval.obarray,
        &eval.dynamic
    ));
}

#[test]
fn terminal_parameter_exposes_oracle_defaults() {
    clear_terminal_parameters();
    let normal =
        builtin_terminal_parameter(vec![Value::Nil, Value::symbol("normal-erase-is-backspace")])
            .unwrap();
    assert_eq!(normal, Value::Int(0));

    let keyboard = builtin_terminal_parameter(vec![
        Value::Nil,
        Value::symbol("keyboard-coding-saved-meta-mode"),
    ])
    .unwrap();
    assert_eq!(keyboard, Value::list(vec![Value::True]));

    let missing =
        builtin_terminal_parameter(vec![Value::Nil, Value::symbol("neovm-param")]).unwrap();
    assert!(missing.is_nil());
}

#[test]
fn terminal_parameter_round_trips() {
    clear_terminal_parameters();
    let set_result = builtin_set_terminal_parameter(vec![
        Value::Nil,
        Value::symbol("neovm-param"),
        Value::Int(42),
    ])
    .unwrap();
    assert!(set_result.is_nil());

    let get_result =
        builtin_terminal_parameter(vec![Value::Nil, Value::symbol("neovm-param")]).unwrap();
    assert_eq!(get_result, Value::Int(42));
}

#[test]
fn set_terminal_parameter_returns_previous_default_values() {
    clear_terminal_parameters();
    let previous_normal = builtin_set_terminal_parameter(vec![
        Value::Nil,
        Value::symbol("normal-erase-is-backspace"),
        Value::Int(9),
    ])
    .unwrap();
    assert_eq!(previous_normal, Value::Int(0));

    let previous_keyboard = builtin_set_terminal_parameter(vec![
        Value::Nil,
        Value::symbol("keyboard-coding-saved-meta-mode"),
        Value::Nil,
    ])
    .unwrap();
    assert_eq!(previous_keyboard, Value::list(vec![Value::True]));
}

#[test]
fn terminal_parameter_distinct_keys_do_not_alias() {
    clear_terminal_parameters();
    builtin_set_terminal_parameter(vec![Value::Nil, Value::symbol("k1"), Value::Int(1)]).unwrap();
    builtin_set_terminal_parameter(vec![Value::Nil, Value::symbol("k2"), Value::Int(2)]).unwrap();

    let first = builtin_terminal_parameter(vec![Value::Nil, Value::symbol("k1")]).unwrap();
    let second = builtin_terminal_parameter(vec![Value::Nil, Value::symbol("k2")]).unwrap();
    assert_eq!(first, Value::Int(1));
    assert_eq!(second, Value::Int(2));
}

#[test]
fn terminal_parameter_rejects_non_symbol_key() {
    clear_terminal_parameters();
    let result = builtin_terminal_parameter(vec![Value::Nil, Value::string("k")]);
    assert!(result.is_err());
}

#[test]
fn set_terminal_parameter_ignores_non_symbol_key() {
    clear_terminal_parameters();
    let set_result =
        builtin_set_terminal_parameter(vec![Value::Nil, Value::string("k"), Value::Int(9)])
            .unwrap();
    assert!(set_result.is_nil());

    let second_result =
        builtin_set_terminal_parameter(vec![Value::Nil, Value::string("k"), Value::Int(1)])
            .unwrap();
    assert!(second_result.is_nil());

    let get_result = builtin_terminal_parameter(vec![Value::Nil, Value::symbol("k")]).unwrap();
    assert!(get_result.is_nil());
}

#[test]
fn set_terminal_parameter_returns_previous_for_repeat_non_symbol_key() {
    clear_terminal_parameters();
    let first =
        builtin_set_terminal_parameter(vec![Value::Nil, Value::Int(1), Value::Int(9)]).unwrap();
    assert!(first.is_nil());

    let second =
        builtin_set_terminal_parameter(vec![Value::Nil, Value::Int(1), Value::Int(1)]).unwrap();
    assert_eq!(second, Value::Int(9));
}

#[test]
fn terminal_parameter_rejects_non_terminal_designator() {
    clear_terminal_parameters();
    let result = builtin_terminal_parameter(vec![Value::Int(1), Value::symbol("k")]);
    assert!(result.is_err());
}

#[test]
fn terminal_parameters_lists_mutated_symbol_entries() {
    clear_terminal_parameters();
    let _ = builtin_set_terminal_parameter(vec![Value::Nil, Value::symbol("k1"), Value::Int(1)])
        .unwrap();
    let _ = builtin_set_terminal_parameter(vec![Value::Nil, Value::symbol("k2"), Value::Int(2)])
        .unwrap();

    let params = builtin_terminal_parameters(vec![Value::Nil]).unwrap();
    let entries = list_to_vec(&params).expect("parameter alist");
    assert!(entries.len() >= 4);
    assert!(
        entries
            .iter()
            .any(|entry| matches!(entry, Value::Cons(cell) if {
                let pair = read_cons(*cell);
                pair.car == Value::symbol("normal-erase-is-backspace") && pair.cdr == Value::Int(0)
            }))
    );
    assert!(
        entries
            .iter()
            .any(|entry| matches!(entry, Value::Cons(cell) if {
                let pair = read_cons(*cell);
                pair.car == Value::symbol("keyboard-coding-saved-meta-mode")
                    && pair.cdr == Value::list(vec![Value::True])
            }))
    );
    assert!(
        entries
            .iter()
            .any(|entry| matches!(entry, Value::Cons(cell) if {
                let pair = read_cons(*cell);
                pair.car == Value::symbol("k1") && pair.cdr == Value::Int(1)
            }))
    );
    assert!(
        entries
            .iter()
            .any(|entry| matches!(entry, Value::Cons(cell) if {
                let pair = read_cons(*cell);
                pair.car == Value::symbol("k2") && pair.cdr == Value::Int(2)
            }))
    );

    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let via_frame = builtin_terminal_parameters_eval(&mut eval, vec![Value::Int(frame_id)])
        .expect("eval terminal-parameters");
    let eval_entries = list_to_vec(&via_frame).expect("parameter alist");
    assert!(eval_entries.len() >= 4);
}

#[test]
fn set_terminal_parameter_rejects_non_terminal_designator() {
    clear_terminal_parameters();
    let result =
        builtin_set_terminal_parameter(vec![Value::Int(1), Value::symbol("k"), Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn eval_terminal_parameter_accepts_live_frame_designator() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    builtin_set_terminal_parameter_eval(
        &mut eval,
        vec![
            Value::Int(frame_id),
            Value::symbol("neovm-frame-param"),
            Value::Int(7),
        ],
    )
    .unwrap();
    let value = builtin_terminal_parameter_eval(
        &mut eval,
        vec![Value::Int(frame_id), Value::symbol("neovm-frame-param")],
    )
    .unwrap();
    assert_eq!(value, Value::Int(7));
}

#[test]
fn terminal_live_p_reflects_designator_shape() {
    let live_nil = builtin_terminal_live_p(vec![Value::Nil]).unwrap();
    let live_handle = builtin_terminal_live_p(vec![terminal_handle_value()]).unwrap();
    let live_string = builtin_terminal_live_p(vec![Value::string("initial_terminal")]).unwrap();
    let live_int = builtin_terminal_live_p(vec![Value::Int(1)]).unwrap();
    assert_eq!(live_nil, Value::True);
    assert_eq!(live_handle, Value::True);
    assert!(live_string.is_nil());
    assert!(live_int.is_nil());
}

#[test]
fn eval_terminal_live_p_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let live = builtin_terminal_live_p_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap();
    assert_eq!(live, Value::True);

    let stale = builtin_terminal_live_p_eval(&mut eval, vec![Value::Int(999_999)]).unwrap();
    assert!(stale.is_nil());
}

#[test]
fn terminal_name_rejects_invalid_designator() {
    let result = builtin_terminal_name(vec![Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn eval_terminal_name_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let result = builtin_terminal_name_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap();
    assert_eq!(result, Value::string("initial_terminal"));
}

#[test]
fn frame_terminal_rejects_non_frame_designator() {
    let result = builtin_frame_terminal(vec![Value::string("not-a-frame")]);
    assert!(result.is_err());
}

#[test]
fn frame_terminal_accepts_frame_id() {
    let result = builtin_frame_terminal(vec![Value::Int(1)]);
    assert!(result.is_ok());
    let handle = result.unwrap();
    let live = builtin_terminal_live_p(vec![handle]).unwrap();
    assert_eq!(live, Value::True);
}

#[test]
fn frame_terminal_returns_live_terminal_handle() {
    let handle = builtin_frame_terminal(vec![Value::Nil]).unwrap();
    let live = builtin_terminal_live_p(vec![handle]).unwrap();
    assert_eq!(live, Value::True);
}

#[test]
fn selected_terminal_returns_live_terminal_handle() {
    let handle = builtin_selected_terminal(vec![]).unwrap();
    let live = builtin_terminal_live_p(vec![handle]).unwrap();
    assert_eq!(live, Value::True);
}

#[test]
fn selected_terminal_arity() {
    assert!(builtin_selected_terminal(vec![Value::Nil]).is_err());
}

#[test]
fn eval_frame_terminal_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let handle = builtin_frame_terminal_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap();
    let live = builtin_terminal_live_p(vec![handle]).unwrap();
    assert_eq!(live, Value::True);
}

#[test]
fn redraw_frame_rejects_non_frame_designator() {
    let result = builtin_redraw_frame(vec![Value::string("not-a-frame")]);
    assert!(result.is_err());
}

#[test]
fn eval_redraw_frame_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let result = builtin_redraw_frame_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn frame_edges_string_designator_uses_unquoted_live_frame_error_message() {
    let result = builtin_frame_edges(vec![Value::string("x")]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("x is not a live frame")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn eval_frame_edges_numeric_designator_reports_numeric_message() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let result = builtin_frame_edges_eval(&mut eval, vec![Value::Int(999_999)]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("999999 is not a live frame")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn eval_frame_edges_live_window_designator_includes_buffer_context() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let window =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();
    let result = builtin_frame_edges_eval(&mut eval, vec![window]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            let message = match sig.data.as_slice() {
                [val] => val.as_str().expect("expected string payload").to_string(),
                other => panic!("expected single error message payload, got {other:?}"),
            };
            assert!(message.starts_with("#<window "));
            assert!(message.contains(" on "));
            assert!(message.ends_with(" is not a live frame"));
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn open_termscript_uses_batch_tty_error_payload() {
    let result = builtin_open_termscript(vec![Value::Nil]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Current frame is not on a tty device")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn send_string_to_terminal_rejects_invalid_terminal_designator() {
    let result = builtin_send_string_to_terminal(vec![Value::string(""), Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn send_string_to_terminal_accepts_live_terminal_handle() {
    let handle = terminal_handle_value();
    let result = builtin_send_string_to_terminal(vec![Value::string(""), handle]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn eval_send_string_to_terminal_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let result = builtin_send_string_to_terminal_eval(
        &mut eval,
        vec![Value::string(""), Value::Int(frame_id)],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_show_cursor_tracks_visibility_state() {
    reset_dispnew_thread_locals();
    let default_visible = builtin_internal_show_cursor_p(vec![]).unwrap();
    assert_eq!(default_visible, Value::True);

    builtin_internal_show_cursor(vec![Value::Nil, Value::Nil]).unwrap();
    let hidden = builtin_internal_show_cursor_p(vec![]).unwrap();
    assert!(hidden.is_nil());

    builtin_internal_show_cursor(vec![Value::Nil, Value::True]).unwrap();
    let visible = builtin_internal_show_cursor_p(vec![]).unwrap();
    assert_eq!(visible, Value::True);
}

#[test]
fn internal_show_cursor_rejects_non_window_designator() {
    let result = builtin_internal_show_cursor(vec![Value::Int(1), Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn eval_internal_show_cursor_accepts_live_window_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let window =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();
    let result = builtin_internal_show_cursor_eval(&mut eval, vec![window, Value::True]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn eval_internal_show_cursor_p_accepts_live_window_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let window =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();
    let result = builtin_internal_show_cursor_p_eval(&mut eval, vec![window]).unwrap();
    assert!(matches!(result, Value::True | Value::Nil));
}

#[test]
fn eval_internal_show_cursor_tracks_per_window_state() {
    reset_dispnew_thread_locals();
    let mut eval = crate::emacs_core::Evaluator::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let selected =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();
    let other = crate::emacs_core::builtins::dispatch_builtin(
        &mut eval,
        "split-window-internal",
        vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil],
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        builtin_internal_show_cursor_p_eval(&mut eval, vec![selected]).unwrap(),
        Value::True
    );
    assert_eq!(
        builtin_internal_show_cursor_p_eval(&mut eval, vec![other]).unwrap(),
        Value::True
    );

    builtin_internal_show_cursor_eval(&mut eval, vec![Value::Nil, Value::Nil]).unwrap();
    assert!(
        builtin_internal_show_cursor_p_eval(&mut eval, vec![selected])
            .unwrap()
            .is_nil()
    );
    assert_eq!(
        builtin_internal_show_cursor_p_eval(&mut eval, vec![other]).unwrap(),
        Value::True
    );
    assert!(
        builtin_internal_show_cursor_p_eval(&mut eval, vec![])
            .unwrap()
            .is_nil()
    );

    builtin_internal_show_cursor_eval(&mut eval, vec![other, Value::True]).unwrap();
    assert!(
        builtin_internal_show_cursor_p_eval(&mut eval, vec![selected])
            .unwrap()
            .is_nil()
    );
    assert_eq!(
        builtin_internal_show_cursor_p_eval(&mut eval, vec![other]).unwrap(),
        Value::True
    );
    assert!(
        builtin_internal_show_cursor_p_eval(&mut eval, vec![])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn tty_queries_reject_invalid_terminal_designator() {
    let tty_type = builtin_tty_type(vec![Value::Int(1)]);
    let tty_top_frame = builtin_tty_top_frame(vec![Value::Int(1)]);
    let controlling = builtin_controlling_tty_p(vec![Value::Int(1)]);
    assert!(tty_type.is_err());
    assert!(tty_top_frame.is_err());
    assert!(controlling.is_err());
}

#[test]
fn eval_tty_queries_accept_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    assert!(
        builtin_tty_type_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_tty_top_frame_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_controlling_tty_p_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn suspend_tty_signals_non_text_terminal_error() {
    for args in [vec![], vec![Value::Nil], vec![terminal_handle_value()]] {
        let result = builtin_suspend_tty(args);
        match result {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string(
                        "Attempt to suspend a non-text terminal device"
                    )]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }
}

#[test]
fn eval_suspend_resume_accept_live_frame_and_signal_non_text_terminal_error() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let suspend = builtin_suspend_tty_eval(&mut eval, vec![Value::Int(frame_id)]);
    let resume = builtin_resume_tty_eval(&mut eval, vec![Value::Int(frame_id)]);
    assert!(suspend.is_err());
    assert!(resume.is_err());
}

#[test]
fn resume_tty_signals_non_text_terminal_error() {
    for args in [vec![], vec![Value::Nil], vec![terminal_handle_value()]] {
        let result = builtin_resume_tty(args);
        match result {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string(
                        "Attempt to resume a non-text terminal device"
                    )]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }
}

#[test]
fn x_open_connection_requires_string_display_arg() {
    let bad = builtin_x_open_connection(vec![Value::Nil]);
    assert!(bad.is_err());
}

#[test]
fn x_open_connection_eval_accepts_x_host_startup() {
    let mut eval = crate::emacs_core::Evaluator::new();
    eval.set_variable("initial-window-system", Value::symbol("x"));
    assert!(
        builtin_x_open_connection_eval(&mut eval, vec![Value::Nil])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn x_window_system_resource_queries_return_nil() {
    let mut eval = crate::emacs_core::Evaluator::new();
    eval.set_variable("initial-window-system", Value::symbol("x"));

    assert!(
        builtin_x_apply_session_resources_eval(&mut eval, vec![])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_get_resource_eval(
            &mut eval,
            vec![Value::string("geometry"), Value::string("Geometry")]
        )
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_x_list_fonts_eval(&mut eval, vec![Value::string("*")])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn x_open_connection_arity_errors() {
    let x_open_none = builtin_x_open_connection(vec![]);
    let x_open_four = builtin_x_open_connection(vec![
        Value::string("foo"),
        Value::string("xrm"),
        Value::t(),
        Value::Nil,
    ]);
    assert!(x_open_none.is_err());
    assert!(x_open_four.is_err());
    match x_open_none {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
        }
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match x_open_four {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
        }
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_close_connection_argument_shape_errors() {
    let x_nil = builtin_x_close_connection(vec![Value::Nil]);
    let x_int = builtin_x_close_connection(vec![Value::Int(1)]);
    let x_str = builtin_x_close_connection(vec![Value::string("")]);
    let x_term = builtin_x_close_connection(vec![terminal_handle_value()]);
    let x_close_none = builtin_x_close_connection(vec![]);
    let x_close_two = builtin_x_close_connection(vec![Value::string("foo"), Value::Nil]);
    assert!(x_nil.is_err());
    assert!(x_int.is_err());
    assert!(x_str.is_err());
    assert!(x_close_none.is_err());
    assert!(x_close_two.is_err());
    match x_close_none {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
        }
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match x_close_two {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
        }
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match x_term {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Terminal 0 is not an X display")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn eval_x_close_connection_live_frame_uses_window_system_error() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;

    let result = builtin_x_close_connection_eval(&mut eval, vec![Value::Int(frame_id)]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn x_display_pixel_size_errors_match_batch_shapes() {
    let width_none = builtin_x_display_pixel_width(vec![]);
    let width_int = builtin_x_display_pixel_width(vec![Value::Int(1)]);
    let width_str = builtin_x_display_pixel_width(vec![Value::string("")]);
    let width_term = builtin_x_display_pixel_width(vec![terminal_handle_value()]);
    let height_none = builtin_x_display_pixel_height(vec![]);
    let height_int = builtin_x_display_pixel_height(vec![Value::Int(1)]);
    let height_str = builtin_x_display_pixel_height(vec![Value::string("")]);
    let height_term = builtin_x_display_pixel_height(vec![terminal_handle_value()]);
    assert!(width_none.is_err());
    assert!(width_int.is_err());
    assert!(width_str.is_err());
    assert!(height_none.is_err());
    assert!(height_int.is_err());
    assert!(height_str.is_err());
    match width_term {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Terminal 0 is not an X display")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match height_term {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Terminal 0 is not an X display")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn x_missing_optional_display_queries_match_batch_no_x_shapes() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let term = terminal_handle_value();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;

    type PureXQuery = fn(Vec<Value>) -> EvalResult;
    type EvalXQuery = fn(&mut crate::emacs_core::eval::Evaluator, Vec<Value>) -> EvalResult;
    for (pure, eval_query) in [
        (
            builtin_x_display_backing_store as PureXQuery,
            builtin_x_display_backing_store_eval as EvalXQuery,
        ),
        (
            builtin_x_display_color_cells,
            builtin_x_display_color_cells_eval,
        ),
        (
            builtin_x_display_mm_height,
            builtin_x_display_mm_height_eval,
        ),
        (builtin_x_display_mm_width, builtin_x_display_mm_width_eval),
        (
            builtin_x_display_monitor_attributes_list,
            builtin_x_display_monitor_attributes_list_eval,
        ),
        (builtin_x_display_planes, builtin_x_display_planes_eval),
        (
            builtin_x_display_save_under,
            builtin_x_display_save_under_eval,
        ),
        (builtin_x_display_screens, builtin_x_display_screens_eval),
        (
            builtin_x_display_visual_class,
            builtin_x_display_visual_class_eval,
        ),
        (
            builtin_x_server_input_extension_version,
            builtin_x_server_input_extension_version_eval,
        ),
        (builtin_x_server_vendor, builtin_x_server_vendor_eval),
    ] {
        match pure(vec![]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("X windows are not in use or not initialized")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }

        match pure(vec![term]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                // Terminal ID may vary; just check the message pattern.
                let msg = sig.data[0].as_str().unwrap_or_default();
                assert!(
                    msg.contains("is not an X display") || msg.contains("X windows are not in use"),
                    "expected terminal error, got: {msg}"
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }

        match pure(vec![Value::string("x")]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Display x can\u{2019}t be opened")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }

        match pure(vec![Value::Int(1)]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
            }
            other => panic!("expected wrong-type-argument signal, got {other:?}"),
        }

        match eval_query(&mut eval, vec![Value::Int(frame_id)]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Window system frame should be used")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }
}

#[test]
fn x_gui_display_queries_accept_nil_and_live_frames_when_x_is_active() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame =
        Value::Int(crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64);
    eval.set_variable("initial-window-system", Value::Nil);
    eval.set_variable("window-system", Value::symbol(gui_window_system_symbol()));

    assert_eq!(
        builtin_x_display_grayscale_p_eval(&mut eval, vec![]).unwrap(),
        Value::True
    );
    assert_eq!(
        builtin_x_display_grayscale_p_eval(&mut eval, vec![frame]).unwrap(),
        Value::True
    );
    assert_eq!(
        builtin_x_display_color_cells_eval(&mut eval, vec![Value::Nil]).unwrap(),
        Value::Int(16_777_216)
    );
    assert_eq!(
        builtin_x_display_color_cells_eval(&mut eval, vec![frame]).unwrap(),
        Value::Int(16_777_216)
    );
    assert_eq!(
        builtin_x_display_planes_eval(&mut eval, vec![Value::Nil]).unwrap(),
        Value::Int(24)
    );
    assert_eq!(
        builtin_x_display_planes_eval(&mut eval, vec![frame]).unwrap(),
        Value::Int(24)
    );
    assert_eq!(
        builtin_x_display_visual_class_eval(&mut eval, vec![Value::Nil]).unwrap(),
        Value::symbol("true-color")
    );
    assert_eq!(
        builtin_x_display_visual_class_eval(&mut eval, vec![frame]).unwrap(),
        Value::symbol("true-color")
    );
}

#[test]
fn x_display_set_last_user_time_batch_semantics() {
    match builtin_x_display_set_last_user_time(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("X windows are not in use or not initialized")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(vec![Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("X windows are not in use or not initialized")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(vec![Value::string("x"), Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("X windows are not in use or not initialized")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(vec![Value::Nil, Value::string("x")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Display x can’t be opened")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(vec![Value::Nil, terminal_handle_value()]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Terminal 0 is not an X display")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(vec![Value::Nil, Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(vec![Value::Nil, Value::Int(1), Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_display_set_last_user_time_eval_uses_user_time_designator_payloads() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let term = terminal_handle_value();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;

    for display in [
        Value::Nil,
        Value::string("display"),
        Value::Int(1),
        Value::symbol("foo"),
        Value::Int(frame_id),
        term,
    ] {
        match builtin_x_display_set_last_user_time_eval(
            &mut eval,
            vec![display, Value::string("x")],
        ) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(sig.data, vec![Value::string("Display x can’t be opened")]);
            }
            other => panic!("expected error signal, got {other:?}"),
        }

        match builtin_x_display_set_last_user_time_eval(
            &mut eval,
            vec![display, Value::Int(frame_id)],
        ) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Window system frame should be used")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }

        match builtin_x_display_set_last_user_time_eval(&mut eval, vec![display, term]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Terminal 0 is not an X display")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }
}

#[test]
fn x_selection_queries_and_old_gtk_dialog_batch_semantics() {
    assert!(builtin_x_selection_exists_p(vec![]).unwrap().is_nil());
    assert!(builtin_x_selection_owner_p(vec![]).unwrap().is_nil());
    assert!(
        builtin_x_selection_exists_p(vec![Value::symbol("PRIMARY"), Value::symbol("STRING")])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_selection_owner_p(vec![Value::symbol("PRIMARY"), Value::Int(1)])
            .unwrap()
            .is_nil()
    );
    match builtin_x_selection_exists_p(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_selection_owner_p(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("symbolp"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    assert!(builtin_x_uses_old_gtk_dialog(vec![]).unwrap().is_nil());
    match builtin_x_uses_old_gtk_dialog(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_geometry_fonts_and_resource_batch_semantics() {
    assert_eq!(
        builtin_x_parse_geometry(vec![Value::string("80x24+10+20")]).unwrap(),
        Value::list(vec![
            Value::cons(Value::symbol("height"), Value::Int(24)),
            Value::cons(Value::symbol("width"), Value::Int(80)),
            Value::cons(Value::symbol("top"), Value::Int(20)),
            Value::cons(Value::symbol("left"), Value::Int(10)),
        ])
    );
    assert_eq!(
        builtin_x_parse_geometry(vec![Value::string("80x24")]).unwrap(),
        Value::list(vec![
            Value::cons(Value::symbol("height"), Value::Int(24)),
            Value::cons(Value::symbol("width"), Value::Int(80)),
        ])
    );
    assert!(
        builtin_x_parse_geometry(vec![Value::string("x")])
            .unwrap()
            .is_nil()
    );
    match builtin_x_parse_geometry(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    assert!(builtin_x_family_fonts(vec![]).unwrap().is_nil());
    assert!(
        builtin_x_family_fonts(vec![Value::string("abc"), Value::Nil])
            .unwrap()
            .is_nil()
    );
    match builtin_x_family_fonts(vec![Value::Int(1), Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_family_fonts(vec![Value::Int(1), Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    match builtin_x_list_fonts(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "Window system is not in use or not initialized"
                )]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_get_resource(vec![Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "Window system is not in use or not initialized"
                )]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_get_resource(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_property_and_frame_arg_batch_semantics() {
    for args in [vec![], vec![Value::Nil], vec![Value::Frame(1)]] {
        match builtin_x_backspace_delete_keys_p(args) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Window system frame should be used")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }
    match builtin_x_backspace_delete_keys_p(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    match builtin_x_get_atom_name(vec![Value::symbol("WM_CLASS")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_get_atom_name(vec![Value::symbol("WM_CLASS"), Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    match builtin_x_window_property(vec![Value::string("WM_NAME")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_window_property(vec![Value::string("WM_NAME"), Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_window_property(vec![
        Value::string("WM_NAME"),
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    match builtin_x_window_property_attributes(vec![Value::string("WM_NAME")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_window_property_attributes(vec![Value::string("WM_NAME"), Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_window_property_attributes(vec![
        Value::string("WM_NAME"),
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_coordinate_sync_and_message_batch_semantics() {
    let term = terminal_handle_value();

    for args in [
        vec![Value::Nil],
        vec![Value::Nil, Value::Nil],
        vec![Value::Frame(1)],
        vec![Value::Int(1), Value::Nil],
        vec![Value::string("x"), Value::Nil],
        vec![term, Value::Nil],
    ] {
        match builtin_x_synchronize(args) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("X windows are not in use or not initialized")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }
    match builtin_x_synchronize(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    match builtin_x_translate_coordinates(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("X windows are not in use or not initialized")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_translate_coordinates(vec![Value::Frame(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_translate_coordinates(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_translate_coordinates(vec![Value::string("x")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Display x can’t be opened")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_translate_coordinates(vec![term]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Terminal 0 is not an X display")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_translate_coordinates(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_translate_coordinates(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    match builtin_x_frame_list_z_order(vec![]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("X windows are not in use or not initialized")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_frame_list_z_order(vec![Value::Frame(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_frame_list_z_order(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_frame_list_z_order(vec![Value::string("x")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Display x can’t be opened")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_frame_list_z_order(vec![term]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Terminal 0 is not an X display")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_frame_list_z_order(vec![Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    match builtin_x_send_client_message(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("X windows are not in use or not initialized")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_send_client_message(vec![
        Value::Frame(1),
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_send_client_message(vec![
        Value::Int(1),
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_send_client_message(vec![
        Value::string("x"),
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Display x can’t be opened")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_send_client_message(vec![
        term,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Terminal 0 is not an X display")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_send_client_message(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_popup_dialog_and_menu_batch_semantics() {
    let term = terminal_handle_value();

    match builtin_x_popup_dialog(vec![Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("windowp"), Value::Nil]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_popup_dialog(vec![Value::Frame(1), Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Nil]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_popup_dialog(vec![Value::Frame(1), Value::list(vec![Value::string("A")])]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("consp"), Value::Nil]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    assert!(
        builtin_x_popup_dialog(vec![
            Value::Frame(1),
            Value::list(vec![
                Value::string("Title"),
                Value::cons(Value::string("Yes"), Value::True),
            ]),
        ])
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_x_popup_dialog(vec![
            Value::Frame(1),
            Value::list(vec![Value::string("A"), Value::Int(1)]),
        ])
        .unwrap()
        .is_nil()
    );
    for arg in [Value::string("x"), Value::Int(1), term] {
        match builtin_x_popup_dialog(vec![arg, Value::Nil]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("windowp"), Value::Nil]);
            }
            other => panic!("expected wrong-type-argument signal, got {other:?}"),
        }
    }
    match builtin_x_popup_dialog(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_popup_dialog(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_popup_dialog(vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    let assert_wta = |result: EvalResult, pred: &str, arg: Value| match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol(pred), arg]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    };
    let basic_menu = Value::list(vec![
        Value::string("A"),
        Value::cons(Value::string("Yes"), Value::True),
    ]);

    assert!(
        builtin_x_popup_menu(vec![Value::Nil, Value::Nil])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_popup_menu(vec![Value::Nil, basic_menu])
            .unwrap()
            .is_nil()
    );
    for pos in [Value::Frame(1), Value::string("x"), Value::Int(1), term] {
        assert_wta(builtin_x_popup_menu(vec![pos, Value::Nil]), "listp", pos);
    }

    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Int(0), Value::Int(0)]),
            Value::Nil,
        ]),
        "listp",
        Value::Int(0),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Int(0), Value::Int(0)]),
            basic_menu,
        ]),
        "listp",
        Value::Int(0),
    );
    assert_wta(
        builtin_x_popup_menu(vec![Value::list(vec![Value::Nil]), Value::Nil]),
        "stringp",
        Value::Nil,
    );
    assert_wta(
        builtin_x_popup_menu(vec![Value::list(vec![Value::Nil]), basic_menu]),
        "consp",
        Value::True,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::symbol("menu-bar")]),
            Value::Nil,
        ]),
        "stringp",
        Value::Nil,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::symbol("menu-bar")]),
            basic_menu,
        ]),
        "consp",
        Value::True,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::symbol("mouse-1")]),
            Value::Nil,
        ]),
        "stringp",
        Value::Nil,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::symbol("mouse-1")]),
            basic_menu,
        ]),
        "consp",
        Value::True,
    );

    assert_wta(
        builtin_x_popup_menu(vec![Value::list(vec![Value::Nil, Value::Nil]), Value::Nil]),
        "stringp",
        Value::Nil,
    );
    assert_wta(
        builtin_x_popup_menu(vec![Value::list(vec![Value::Nil, Value::Nil]), basic_menu]),
        "consp",
        Value::True,
    );
    assert!(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::list(vec![Value::string("A")]),
        ])
        .unwrap()
        .is_nil()
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::list(vec![Value::string("A"), Value::Int(1)]),
        ]),
        "listp",
        Value::Int(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::list(vec![
                Value::Int(1),
                Value::cons(Value::string("Yes"), Value::True),
            ]),
        ]),
        "stringp",
        Value::Int(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::list(vec![Value::cons(Value::string("A"), Value::True)]),
        ]),
        "stringp",
        Value::cons(Value::string("A"), Value::True),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::Int(1),
        ]),
        "listp",
        Value::Int(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::string("x"),
        ]),
        "listp",
        Value::string("x"),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::list(vec![Value::string("A"), Value::Nil]),
        ]),
        "stringp",
        Value::Nil,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::list(vec![
                Value::string("A"),
                Value::list(vec![Value::string("Pane")]),
            ]),
        ]),
        "consp",
        Value::Nil,
    );
    assert!(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::list(vec![
                Value::string("A"),
                Value::list(vec![Value::string("Pane"), Value::Nil]),
            ]),
        ])
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::list(vec![
                Value::string("A"),
                Value::list(vec![
                    Value::string("Pane"),
                    Value::cons(Value::string("Y"), Value::True),
                ]),
            ]),
        ])
        .unwrap()
        .is_nil()
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::list(vec![
                Value::string("A"),
                Value::cons(Value::string("Pane"), Value::Int(1)),
            ]),
        ]),
        "consp",
        Value::Int(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::Nil, Value::Nil]),
            Value::list(vec![
                Value::string("A"),
                Value::cons(Value::Int(1), Value::Int(2)),
            ]),
        ]),
        "stringp",
        Value::Int(1),
    );

    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::list(vec![Value::Int(0), Value::Int(0)])]),
            Value::Nil,
        ]),
        "windowp",
        Value::Nil,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::list(vec![Value::Int(0), Value::Int(0)])]),
            basic_menu,
        ]),
        "windowp",
        Value::Nil,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![
                Value::list(vec![Value::Int(0), Value::Int(0)]),
                Value::Int(1),
            ]),
            Value::Nil,
        ]),
        "windowp",
        Value::Int(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![
                Value::list(vec![Value::Int(0), Value::Int(0)]),
                Value::Int(1),
            ]),
            basic_menu,
        ]),
        "windowp",
        Value::Int(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::cons(
                Value::list(vec![Value::Int(0), Value::Int(0)]),
                Value::Int(0),
            ),
            Value::Nil,
        ]),
        "listp",
        Value::Int(0),
    );
    match builtin_x_popup_menu(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_popup_menu(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_popup_menu(vec![Value::Nil, Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_clipboard_input_context_batch_semantics() {
    let term = terminal_handle_value();
    let frame = Value::Frame(1);

    let assert_wrong_type = |result: EvalResult, pred: &str, arg: Value| match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol(pred), arg]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    };
    let assert_error = |result: EvalResult, msg: &str| match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string(msg)]);
        }
        other => panic!("expected error signal, got {other:?}"),
    };
    let assert_wrong_number = |result: EvalResult| match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    };

    assert!(builtin_x_get_clipboard(vec![]).unwrap().is_nil());
    assert_wrong_number(builtin_x_get_clipboard(vec![Value::Nil]));

    assert_error(
        builtin_x_get_modifier_masks(vec![]),
        "X windows are not in use or not initialized",
    );
    assert_error(
        builtin_x_get_modifier_masks(vec![Value::Nil]),
        "X windows are not in use or not initialized",
    );
    assert_error(
        builtin_x_get_modifier_masks(vec![term]),
        "Terminal 0 is not an X display",
    );
    assert_wrong_type(
        builtin_x_get_modifier_masks(vec![Value::Int(1)]),
        "frame-live-p",
        Value::Int(1),
    );
    assert_error(
        builtin_x_get_modifier_masks(vec![Value::string("x")]),
        "Display x can’t be opened",
    );
    assert_error(
        builtin_x_get_modifier_masks(vec![frame]),
        "Window system frame should be used",
    );
    assert_wrong_number(builtin_x_get_modifier_masks(vec![Value::Nil, Value::Nil]));

    assert!(builtin_x_hide_tip(vec![]).unwrap().is_nil());
    assert_wrong_number(builtin_x_hide_tip(vec![Value::Nil]));

    assert_wrong_type(
        builtin_x_setup_function_keys(vec![Value::Nil]),
        "frame-live-p",
        Value::Nil,
    );
    assert_wrong_type(
        builtin_x_setup_function_keys(vec![term]),
        "frame-live-p",
        term,
    );
    assert_wrong_type(
        builtin_x_setup_function_keys(vec![Value::Int(1)]),
        "terminal-live-p",
        Value::Int(1),
    );
    assert_wrong_type(
        builtin_x_setup_function_keys(vec![Value::string("x")]),
        "terminal-live-p",
        Value::string("x"),
    );
    assert!(builtin_x_setup_function_keys(vec![frame]).unwrap().is_nil());
    assert_wrong_number(builtin_x_setup_function_keys(vec![]));
    assert_wrong_number(builtin_x_setup_function_keys(vec![Value::Nil, Value::Nil]));

    for arg in [Value::Nil, term, Value::Int(1), Value::string("x"), frame] {
        assert!(
            builtin_x_internal_focus_input_context(vec![arg])
                .unwrap()
                .is_nil()
        );
    }
    assert_wrong_number(builtin_x_internal_focus_input_context(vec![]));
    assert_wrong_number(builtin_x_internal_focus_input_context(vec![
        Value::Nil,
        Value::Nil,
    ]));

    assert_error(
        builtin_x_wm_set_size_hint(vec![]),
        "Window system frame should be used",
    );
    assert_error(
        builtin_x_wm_set_size_hint(vec![Value::Nil]),
        "Window system frame should be used",
    );
    assert_wrong_type(
        builtin_x_wm_set_size_hint(vec![term]),
        "frame-live-p",
        terminal_handle_value(),
    );
    assert_wrong_type(
        builtin_x_wm_set_size_hint(vec![Value::Int(1)]),
        "frame-live-p",
        Value::Int(1),
    );
    assert_wrong_type(
        builtin_x_wm_set_size_hint(vec![Value::string("x")]),
        "frame-live-p",
        Value::string("x"),
    );
    assert_error(
        builtin_x_wm_set_size_hint(vec![frame]),
        "Window system frame should be used",
    );
    assert_wrong_number(builtin_x_wm_set_size_hint(vec![Value::Nil, Value::Nil]));
}

#[test]
fn x_win_suspend_error_is_not_dispatch_builtin() {
    assert!(!super::super::builtin_registry::is_dispatch_builtin_name(
        "x-win-suspend-error"
    ));
}

#[test]
fn x_clipboard_yank_is_not_dispatch_builtin() {
    assert!(!super::super::builtin_registry::is_dispatch_builtin_name(
        "x-clipboard-yank"
    ));
}

#[test]
fn x_clear_preedit_text_is_not_dispatch_builtin() {
    assert!(!super::super::builtin_registry::is_dispatch_builtin_name(
        "x-clear-preedit-text"
    ));
}

#[test]
fn x_preedit_text_is_not_dispatch_builtin() {
    assert!(!super::super::builtin_registry::is_dispatch_builtin_name(
        "x-preedit-text"
    ));
}

#[test]
fn x_device_class_is_not_dispatch_builtin() {
    assert!(!super::super::builtin_registry::is_dispatch_builtin_name(
        "x-device-class"
    ));
}

#[test]
fn x_get_input_coding_system_is_not_dispatch_builtin() {
    assert!(!super::super::builtin_registry::is_dispatch_builtin_name(
        "x-get-input-coding-system"
    ));
}

#[test]
fn x_selection_property_tip_batch_semantics() {
    let assert_wrong_type = |result: EvalResult, pred: &str, arg: Value| match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol(pred), arg]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    };
    let assert_error = |result: EvalResult, msg: &str| match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string(msg)]);
        }
        other => panic!("expected error signal, got {other:?}"),
    };
    let assert_wrong_number = |result: EvalResult| match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    };

    assert_error(
        builtin_x_apply_session_resources(vec![]),
        "Window system is not in use or not initialized",
    );
    assert_wrong_number(builtin_x_apply_session_resources(vec![Value::Nil]));

    assert_error(
        builtin_x_change_window_property(vec![Value::string("P"), Value::string("V")]),
        "Window system frame should be used",
    );
    assert_error(
        builtin_x_change_window_property(vec![Value::string("P"), Value::string("V"), Value::Nil]),
        "Window system frame should be used",
    );
    assert_error(
        builtin_x_change_window_property(vec![
            Value::string("P"),
            Value::string("V"),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ]),
        "Window system frame should be used",
    );
    assert_wrong_number(builtin_x_change_window_property(vec![
        Value::string("P"),
        Value::string("V"),
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]));

    assert_error(
        builtin_x_delete_window_property(vec![Value::string("P")]),
        "Window system frame should be used",
    );
    assert_error(
        builtin_x_delete_window_property(vec![Value::string("P"), Value::Nil]),
        "Window system frame should be used",
    );
    assert_error(
        builtin_x_delete_window_property(vec![Value::string("P"), Value::Nil, Value::Nil]),
        "Window system frame should be used",
    );
    assert_wrong_number(builtin_x_delete_window_property(vec![
        Value::string("P"),
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]));

    assert!(
        builtin_x_disown_selection_internal(vec![Value::Nil])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_disown_selection_internal(vec![Value::Nil, Value::Nil])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_disown_selection_internal(vec![Value::Nil, Value::Nil, Value::Nil])
            .unwrap()
            .is_nil()
    );
    assert_wrong_number(builtin_x_disown_selection_internal(vec![]));
    assert_wrong_number(builtin_x_disown_selection_internal(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]));

    assert_wrong_type(builtin_x_get_local_selection(vec![]), "consp", Value::Nil);
    assert_wrong_type(
        builtin_x_get_local_selection(vec![Value::Nil]),
        "consp",
        Value::Nil,
    );
    assert_wrong_type(
        builtin_x_get_local_selection(vec![Value::Nil, Value::Nil]),
        "consp",
        Value::Nil,
    );
    assert_wrong_number(builtin_x_get_local_selection(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]));

    assert_error(
        builtin_x_get_selection_internal(vec![Value::Nil, Value::Nil]),
        "X selection unavailable for this frame",
    );
    assert_error(
        builtin_x_get_selection_internal(vec![Value::Nil, Value::Nil, Value::Nil]),
        "X selection unavailable for this frame",
    );
    assert_error(
        builtin_x_get_selection_internal(vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil]),
        "X selection unavailable for this frame",
    );
    assert_wrong_number(builtin_x_get_selection_internal(vec![]));
    assert_wrong_number(builtin_x_get_selection_internal(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]));

    assert_error(
        builtin_x_own_selection_internal(vec![Value::Nil, Value::Nil]),
        "X selection unavailable for this frame",
    );
    assert_error(
        builtin_x_own_selection_internal(vec![Value::Nil, Value::Nil, Value::Nil]),
        "X selection unavailable for this frame",
    );
    assert_wrong_number(builtin_x_own_selection_internal(vec![Value::Nil]));
    assert_wrong_number(builtin_x_own_selection_internal(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]));

    assert_error(
        builtin_x_show_tip(vec![Value::string("m")]),
        "Window system frame should be used",
    );
    assert_wrong_type(
        builtin_x_show_tip(vec![Value::Int(1)]),
        "stringp",
        Value::Int(1),
    );
    assert_error(
        builtin_x_show_tip(vec![
            Value::string("m"),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ]),
        "Window system frame should be used",
    );
    assert_wrong_number(builtin_x_show_tip(vec![]));
    assert_wrong_number(builtin_x_show_tip(vec![
        Value::string("m"),
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]));
}

#[test]
fn gui_selection_batch_semantics() {
    let assert_error = |result: EvalResult, msg: &str| match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string(msg)]);
        }
        other => panic!("expected error signal, got {other:?}"),
    };
    let assert_wrong_number = |result: EvalResult| match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    };

    assert!(builtin_gui_get_selection(vec![]).unwrap().is_nil());
    assert!(
        builtin_gui_get_selection(vec![Value::Nil])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_gui_get_selection(vec![Value::Nil, Value::Nil])
            .unwrap()
            .is_nil()
    );
    assert_wrong_number(builtin_gui_get_selection(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]));

    assert_error(
        builtin_gui_get_primary_selection(vec![]),
        "No selection is available",
    );
    assert_wrong_number(builtin_gui_get_primary_selection(vec![Value::Nil]));

    assert!(
        builtin_gui_select_text(vec![Value::string("a")])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_gui_select_text(vec![Value::Int(1)])
            .unwrap()
            .is_nil()
    );
    assert_wrong_number(builtin_gui_select_text(vec![
        Value::string("a"),
        Value::Nil,
    ]));

    assert!(builtin_gui_selection_value(vec![]).unwrap().is_nil());
    assert_wrong_number(builtin_gui_selection_value(vec![Value::Nil]));

    assert!(
        builtin_gui_set_selection(vec![Value::Nil, Value::Nil])
            .unwrap()
            .is_nil()
    );
    assert_wrong_number(builtin_gui_set_selection(vec![
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]));
}

#[test]
fn x_frame_restack_safe_arity_surface() {
    match builtin_x_frame_restack(vec![Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_frame_restack(vec![Value::Nil, Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_frame_restack(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_frame_restack(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_frame_restack(vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_frame_mouse_and_dnd_batch_semantics() {
    let term = terminal_handle_value();

    for args in [
        vec![],
        vec![Value::Nil],
        vec![Value::Frame(1)],
        vec![Value::Nil, Value::Nil],
    ] {
        match builtin_x_export_frames(args) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Window system frame should be used")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }
    for arg in [Value::Int(1), Value::string("x"), term] {
        match builtin_x_export_frames(vec![arg]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), arg]);
            }
            other => panic!("expected wrong-type-argument signal, got {other:?}"),
        }
    }
    match builtin_x_export_frames(vec![Value::Nil, Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    for args in [
        vec![Value::Nil],
        vec![Value::Frame(1)],
        vec![Value::Nil, Value::Nil],
    ] {
        match builtin_x_focus_frame(args) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Window system frame should be used")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }
    for arg in [Value::Int(1), Value::string("x"), term] {
        match builtin_x_focus_frame(vec![arg]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), arg]);
            }
            other => panic!("expected wrong-type-argument signal, got {other:?}"),
        }
    }
    match builtin_x_focus_frame(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    assert!(builtin_x_frame_edges(vec![]).unwrap().is_nil());
    assert!(builtin_x_frame_edges(vec![Value::Nil]).unwrap().is_nil());
    assert!(
        builtin_x_frame_edges(vec![Value::Frame(1)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_frame_edges(vec![Value::Nil, Value::Nil])
            .unwrap()
            .is_nil()
    );
    match builtin_x_frame_edges(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_frame_edges(vec![Value::string("x")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::string("x")]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_frame_edges(vec![Value::Nil, Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    assert!(builtin_x_frame_geometry(vec![]).unwrap().is_nil());
    assert!(builtin_x_frame_geometry(vec![Value::Nil]).unwrap().is_nil());
    assert!(
        builtin_x_frame_geometry(vec![Value::Frame(1)])
            .unwrap()
            .is_nil()
    );
    match builtin_x_frame_geometry(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::Int(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_frame_geometry(vec![Value::string("x")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::string("x")]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_frame_geometry(vec![Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    assert!(
        builtin_x_mouse_absolute_pixel_position(vec![])
            .unwrap()
            .is_nil()
    );
    match builtin_x_mouse_absolute_pixel_position(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    assert!(
        builtin_x_set_mouse_absolute_pixel_position(vec![Value::Nil, Value::Nil])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_set_mouse_absolute_pixel_position(vec![Value::Int(1), Value::Int(2)])
            .unwrap()
            .is_nil()
    );
    match builtin_x_set_mouse_absolute_pixel_position(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_set_mouse_absolute_pixel_position(vec![Value::Nil, Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    for args in [
        vec![Value::Nil],
        vec![Value::Frame(1)],
        vec![Value::Int(1)],
        vec![terminal_handle_value()],
        vec![Value::Nil, Value::Nil],
    ] {
        match builtin_x_register_dnd_atom(args) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Window system frame should be used")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }
    match builtin_x_register_dnd_atom(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_register_dnd_atom(vec![Value::Nil, Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn eval_x_display_queries_accept_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;

    let width = builtin_x_display_pixel_width_eval(&mut eval, vec![Value::Int(frame_id)]);
    let height = builtin_x_display_pixel_height_eval(&mut eval, vec![Value::Int(frame_id)]);
    assert!(width.is_err());
    assert!(height.is_err());
}

#[test]
fn eval_monitor_attributes_include_bootstrapped_frame() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let list = builtin_display_monitor_attributes_list_eval(&mut eval, vec![]).unwrap();
    let monitors = list_to_vec(&list).expect("monitor list");
    let attrs = list_to_vec(monitors.first().expect("first monitor")).expect("monitor attrs");

    let mut frames_value = Value::Nil;
    for attr in attrs {
        if let Value::Cons(cell) = attr {
            let pair = read_cons(cell);
            if matches!(&pair.car, Value::Symbol(id) if resolve_sym(*id) == "frames") {
                frames_value = pair.cdr;
                break;
            }
        }
    }

    let frames = list_to_vec(&frames_value).expect("frames list");
    assert_eq!(frames.len(), 1);
    assert!(matches!(frames.first(), Some(Value::Frame(_))));
    assert!(!frames[0].is_integer());
    assert_eq!(
        crate::emacs_core::window_cmds::builtin_framep(&mut eval, vec![frames[0]]).unwrap(),
        Value::True
    );
    assert_eq!(
        crate::emacs_core::window_cmds::builtin_frame_live_p(&mut eval, vec![frames[0]]).unwrap(),
        Value::True
    );
}

#[test]
fn eval_monitor_queries_accept_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;

    let list = builtin_display_monitor_attributes_list_eval(&mut eval, vec![Value::Int(frame_id)])
        .unwrap();
    let monitors = list_to_vec(&list).expect("monitor list");
    assert_eq!(monitors.len(), 1);

    let attrs =
        builtin_frame_monitor_attributes_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap();
    let attr_list = list_to_vec(&attrs).expect("monitor attrs");
    assert!(!attr_list.is_empty());
}

#[test]
fn eval_monitor_queries_accept_frame_handle_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let list = builtin_display_monitor_attributes_list_eval(&mut eval, vec![]).unwrap();
    let monitors = list_to_vec(&list).expect("monitor list");
    let attrs = list_to_vec(monitors.first().expect("first monitor")).expect("monitor attrs");

    let mut frame = Value::Nil;
    for attr in attrs {
        if let Value::Cons(cell) = attr {
            let pair = read_cons(cell);
            if matches!(&pair.car, Value::Symbol(id) if resolve_sym(*id) == "frames") {
                let frames = list_to_vec(&pair.cdr).expect("frames list");
                frame = frames.first().cloned().expect("first frame");
                break;
            }
        }
    }
    assert!(matches!(frame, Value::Frame(_)));

    let by_display = builtin_display_monitor_attributes_list_eval(&mut eval, vec![frame]).unwrap();
    let display_list = list_to_vec(&by_display).expect("monitor list");
    assert_eq!(display_list.len(), 1);

    let by_frame = builtin_frame_monitor_attributes_eval(&mut eval, vec![frame]).unwrap();
    let frame_attrs = list_to_vec(&by_frame).expect("monitor attrs");
    assert!(!frame_attrs.is_empty());
}

#[test]
fn eval_display_queries_accept_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;

    assert!(
        builtin_display_graphic_p_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert_eq!(
        builtin_display_pixel_width_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap(),
        Value::Int(80)
    );
    assert_eq!(
        builtin_display_pixel_height_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap(),
        Value::Int(25)
    );
    assert!(
        builtin_display_mm_width_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_mm_height_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert_eq!(
        builtin_display_screens_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        builtin_display_color_cells_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap(),
        Value::Int(0)
    );
    assert_eq!(
        builtin_display_planes_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap(),
        Value::Int(3)
    );
    assert_eq!(
        builtin_display_visual_class_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap(),
        Value::symbol("static-gray")
    );
    assert_eq!(
        builtin_display_backing_store_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap(),
        Value::symbol("not-useful")
    );
    assert_eq!(
        builtin_display_save_under_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap(),
        Value::symbol("not-useful")
    );
    assert!(
        builtin_display_selections_p_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_images_p_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_supports_face_attributes_p_eval(
            &mut eval,
            vec![Value::list(vec![
                Value::symbol(":weight"),
                Value::symbol("bold")
            ])]
        )
        .unwrap()
        .is_nil()
    );
}

#[test]
fn window_system_prefers_selected_frame_then_global_fallback() {
    let mut eval = crate::emacs_core::Evaluator::new();

    assert_eq!(
        builtin_window_system_eval(&mut eval, vec![]).unwrap(),
        Value::Nil
    );

    eval.set_variable("window-system", Value::symbol("tty"));
    assert_eq!(
        builtin_window_system_eval(&mut eval, vec![]).unwrap(),
        Value::symbol("tty")
    );

    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    eval.frames
        .get_mut(frame_id)
        .expect("selected frame")
        .parameters
        .insert("window-system".to_string(), Value::symbol("x"));

    assert_eq!(
        builtin_window_system_eval(&mut eval, vec![]).unwrap(),
        Value::symbol("x")
    );
    assert_eq!(
        builtin_window_system_eval(&mut eval, vec![Value::Int(frame_id.0 as i64)]).unwrap(),
        Value::symbol("x")
    );

    let err = builtin_window_system_eval(&mut eval, vec![Value::string("x")]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("framep"), Value::string("x")]);
        }
        other => panic!("expected wrong-type-argument, got {other:?}"),
    }
}

#[test]
fn eval_display_queries_reject_invalid_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let result = builtin_display_pixel_width_eval(&mut eval, vec![Value::Int(999_999)]);
    assert!(result.is_err());
}

#[test]
fn eval_display_queries_string_designator_reports_missing_display() {
    fn assert_missing_display(result: EvalResult) {
        match result {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(sig.data, vec![Value::string("Display x does not exist")]);
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }

    let mut eval = crate::emacs_core::Evaluator::new();
    assert_missing_display(builtin_display_graphic_p_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_pixel_width_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_pixel_height_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_mm_width_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_mm_height_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_screens_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_color_cells_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_planes_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_visual_class_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_backing_store_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_save_under_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_selections_p_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_images_p_eval(
        &mut eval,
        vec![Value::string("x")],
    ));
}

#[test]
fn eval_display_monitor_errors_render_window_designators() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let window =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();

    let list_err = builtin_display_monitor_attributes_list_eval(&mut eval, vec![window])
        .expect_err("window designator should be rejected");
    let frame_err = builtin_frame_monitor_attributes_eval(&mut eval, vec![window])
        .expect_err("window designator should be rejected");

    for err in [list_err, frame_err] {
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "error");
                match sig.data.as_slice() {
                    [val] => {
                        let msg = val.as_str().expect("expected string payload").to_string();
                        assert!(msg.contains("get-device-terminal"));
                        assert!(msg.contains("#<window"));
                        assert!(msg.contains("*scratch*"));
                    }
                    other => panic!("unexpected signal payload: {other:?}"),
                }
            }
            other => panic!("expected signal, got {other:?}"),
        }
    }
}

#[test]
fn get_device_terminal_formatter_keeps_integer_literals() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let window =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();

    let rendered_window = format_get_device_terminal_arg_eval(&eval, &window);
    assert!(rendered_window.contains("#<window"));
    assert!(rendered_window.contains("*scratch*"));

    let rendered_integer = format_get_device_terminal_arg_eval(&eval, &Value::Int(1));
    assert_eq!(rendered_integer, "1");
}

#[test]
fn display_images_p_shapes_and_errors() {
    assert!(builtin_display_images_p(vec![]).unwrap().is_nil());
    assert!(builtin_display_images_p(vec![Value::Nil]).unwrap().is_nil());

    match builtin_display_images_p(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Invalid argument 1 in ‘get-device-terminal’")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_display_images_p(vec![Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {other:?}"),
    }
}

#[test]
fn display_save_under_and_display_selections_p_shapes_and_errors() {
    assert_eq!(
        builtin_display_save_under(vec![]).unwrap(),
        Value::symbol("not-useful")
    );
    assert_eq!(
        builtin_display_save_under(vec![Value::Nil]).unwrap(),
        Value::symbol("not-useful")
    );
    assert!(builtin_display_selections_p(vec![]).unwrap().is_nil());
    assert!(
        builtin_display_selections_p(vec![Value::Nil])
            .unwrap()
            .is_nil()
    );

    match builtin_display_save_under(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Invalid argument 1 in ‘get-device-terminal’")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_display_selections_p(vec![Value::Int(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Invalid argument 1 in ‘get-device-terminal’")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_display_save_under(vec![Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {other:?}"),
    }

    match builtin_display_selections_p(vec![Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {other:?}"),
    }
}

#[test]
fn display_optional_capability_queries_match_color_shapes() {
    for query in [
        builtin_display_grayscale_p as fn(Vec<Value>) -> EvalResult,
        builtin_display_mouse_p,
        builtin_display_popup_menus_p,
        builtin_display_symbol_keys_p,
    ] {
        assert!(query(vec![]).unwrap().is_nil());
        assert!(query(vec![Value::Nil]).unwrap().is_nil());
        assert!(query(vec![terminal_handle_value()]).unwrap().is_nil());

        match query(vec![Value::Int(1)]) {
            Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "error"),
            other => panic!("expected error signal, got {other:?}"),
        }

        match query(vec![Value::string("x")]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(sig.data, vec![Value::string("Display x does not exist")]);
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }

    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    assert!(
        builtin_display_grayscale_p_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_mouse_p_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_popup_menus_p_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_symbol_keys_p_eval(&mut eval, vec![Value::Int(frame_id)])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn display_supports_face_attributes_p_arity_and_nil_result() {
    let attrs = Value::list(vec![Value::symbol(":weight"), Value::symbol("bold")]);
    assert!(
        builtin_display_supports_face_attributes_p(vec![attrs])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_supports_face_attributes_p(vec![attrs, Value::Int(999_999)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_supports_face_attributes_p(vec![Value::Int(1)])
            .unwrap()
            .is_nil()
    );

    match builtin_display_supports_face_attributes_p(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {other:?}"),
    }
    match builtin_display_supports_face_attributes_p(vec![attrs, Value::Nil, Value::Nil]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {other:?}"),
    }
}
