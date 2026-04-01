use super::*;
use crate::emacs_core::dispnew::pure::{
    builtin_internal_show_cursor, builtin_internal_show_cursor_p, builtin_open_termscript,
    builtin_redraw_frame, builtin_send_string_to_terminal, reset_dispnew_thread_locals,
};
use crate::emacs_core::intern::resolve_sym;
use crate::emacs_core::terminal::pure::{
    builtin_controlling_tty_p, builtin_frame_terminal, builtin_resume_tty,
    builtin_selected_terminal, builtin_set_terminal_parameter, builtin_suspend_tty,
    builtin_terminal_live_p, builtin_terminal_name, builtin_terminal_parameter,
    builtin_terminal_parameters, builtin_tty_top_frame, builtin_tty_type,
    reset_terminal_thread_locals, terminal_handle_value,
};
use crate::emacs_core::value::ValueKind;

fn clear_terminal_parameters() {
    reset_terminal_thread_locals();
}

#[test]
fn x_window_system_active_falls_back_to_window_system_when_initial_is_nil() {
    let mut eval = crate::emacs_core::Context::new();
    eval.set_variable("initial-window-system", Value::NIL);
    eval.set_variable("window-system", Value::symbol(gui_window_system_symbol()));

    assert!(x_window_system_active(&eval));
    assert!(x_window_system_active_in_state(&eval.obarray, &[]));
}

#[test]
fn terminal_parameter_exposes_oracle_defaults() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    let normal = builtin_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::symbol("normal-erase-is-backspace")],
    )
    .unwrap();
    assert_val_eq!(normal, Value::fixnum(0));

    let keyboard = builtin_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::symbol("keyboard-coding-saved-meta-mode")],
    )
    .unwrap();
    assert_val_eq!(keyboard, Value::list(vec![Value::T]));

    let missing =
        builtin_terminal_parameter(&mut eval, vec![Value::NIL, Value::symbol("neovm-param")])
            .unwrap();
    assert!(missing.is_nil());
}

#[test]
fn terminal_parameter_round_trips() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    let set_result = builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::symbol("neovm-param"), Value::fixnum(42)],
    )
    .unwrap();
    assert!(set_result.is_nil());

    let get_result =
        builtin_terminal_parameter(&mut eval, vec![Value::NIL, Value::symbol("neovm-param")])
            .unwrap();
    assert_val_eq!(get_result, Value::fixnum(42));
}

#[test]
fn set_terminal_parameter_returns_previous_default_values() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    let previous_normal = builtin_set_terminal_parameter(
        &mut eval,
        vec![
            Value::NIL,
            Value::symbol("normal-erase-is-backspace"),
            Value::fixnum(9),
        ],
    )
    .unwrap();
    assert_val_eq!(previous_normal, Value::fixnum(0));

    let previous_keyboard = builtin_set_terminal_parameter(
        &mut eval,
        vec![
            Value::NIL,
            Value::symbol("keyboard-coding-saved-meta-mode"),
            Value::NIL,
        ],
    )
    .unwrap();
    assert_val_eq!(previous_keyboard, Value::list(vec![Value::T]));
}

#[test]
fn terminal_parameter_distinct_keys_do_not_alias() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::symbol("k1"), Value::fixnum(1)],
    )
    .unwrap();
    builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::symbol("k2"), Value::fixnum(2)],
    )
    .unwrap();

    let first =
        builtin_terminal_parameter(&mut eval, vec![Value::NIL, Value::symbol("k1")]).unwrap();
    let second =
        builtin_terminal_parameter(&mut eval, vec![Value::NIL, Value::symbol("k2")]).unwrap();
    assert_val_eq!(first, Value::fixnum(1));
    assert_val_eq!(second, Value::fixnum(2));
}

#[test]
fn terminal_parameter_rejects_non_symbol_key() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    let result = builtin_terminal_parameter(&mut eval, vec![Value::NIL, Value::string("k")]);
    assert!(result.is_err());
}

#[test]
fn set_terminal_parameter_ignores_non_symbol_key() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    let set_result = builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::string("k"), Value::fixnum(9)],
    )
    .unwrap();
    assert!(set_result.is_nil());

    let second_result = builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::string("k"), Value::fixnum(1)],
    )
    .unwrap();
    assert!(second_result.is_nil());

    let get_result =
        builtin_terminal_parameter(&mut eval, vec![Value::NIL, Value::symbol("k")]).unwrap();
    assert!(get_result.is_nil());
}

#[test]
fn set_terminal_parameter_returns_previous_for_repeat_non_symbol_key() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    let first = builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::fixnum(1), Value::fixnum(9)],
    )
    .unwrap();
    assert!(first.is_nil());

    let second = builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::fixnum(1), Value::fixnum(1)],
    )
    .unwrap();
    assert_val_eq!(second, Value::fixnum(9));
}

#[test]
fn terminal_parameter_rejects_non_terminal_designator() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    let result = builtin_terminal_parameter(&mut eval, vec![Value::fixnum(1), Value::symbol("k")]);
    assert!(result.is_err());
}

#[test]
fn terminal_parameters_lists_mutated_symbol_entries() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    let _ = builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::symbol("k1"), Value::fixnum(1)],
    )
    .unwrap();
    let _ = builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::symbol("k2"), Value::fixnum(2)],
    )
    .unwrap();

    let params = builtin_terminal_parameters(&mut eval, vec![Value::NIL]).unwrap();
    let entries = list_to_vec(&params).expect("parameter alist");
    assert!(entries.len() >= 4);
    assert!(entries.iter().any(|entry| entry.is_cons() && {
        entry.cons_car() == Value::symbol("normal-erase-is-backspace")
            && entry.cons_cdr() == Value::fixnum(0)
    }));
    assert!(entries.iter().any(|entry| entry.is_cons() && {
        entry.cons_car() == Value::symbol("keyboard-coding-saved-meta-mode")
            && entry.cons_cdr() == Value::list(vec![Value::T])
    }));
    assert!(entries.iter().any(|entry| entry.is_cons() && {
        entry.cons_car() == Value::symbol("k1") && entry.cons_cdr() == Value::fixnum(1)
    }));
    assert!(entries.iter().any(|entry| entry.is_cons() && {
        entry.cons_car() == Value::symbol("k2") && entry.cons_cdr() == Value::fixnum(2)
    }));

    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;
    let via_frame = builtin_terminal_parameters(&mut eval, vec![Value::make_frame(frame_id)])
        .expect("eval terminal-parameters");
    let eval_entries = list_to_vec(&via_frame).expect("parameter alist");
    assert!(eval_entries.len() >= 4);
}

#[test]
fn set_terminal_parameter_rejects_non_terminal_designator() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    let result = builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::fixnum(1), Value::symbol("k"), Value::fixnum(1)],
    );
    assert!(result.is_err());
}

#[test]
fn eval_terminal_parameter_accepts_live_frame_designator() {
    clear_terminal_parameters();
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;
    builtin_set_terminal_parameter(
        &mut eval,
        vec![
            Value::make_frame(frame_id),
            Value::symbol("neovm-frame-param"),
            Value::fixnum(7),
        ],
    )
    .unwrap();
    let value = builtin_terminal_parameter(
        &mut eval,
        vec![
            Value::make_frame(frame_id),
            Value::symbol("neovm-frame-param"),
        ],
    )
    .unwrap();
    assert_val_eq!(value, Value::fixnum(7));
}

#[test]
fn terminal_live_p_reflects_designator_shape() {
    let mut eval = crate::emacs_core::Context::new();
    let live_nil = builtin_terminal_live_p(&mut eval, vec![Value::NIL]).unwrap();
    let live_handle = builtin_terminal_live_p(&mut eval, vec![terminal_handle_value()]).unwrap();
    let live_string =
        builtin_terminal_live_p(&mut eval, vec![Value::string("initial_terminal")]).unwrap();
    let live_int = builtin_terminal_live_p(&mut eval, vec![Value::fixnum(1)]).unwrap();
    assert_val_eq!(live_nil, Value::T);
    assert_val_eq!(live_handle, Value::T);
    assert!(live_string.is_nil());
    assert!(live_int.is_nil());
}

#[test]
fn eval_terminal_live_p_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;
    let live = builtin_terminal_live_p(&mut eval, vec![Value::make_frame(frame_id)]).unwrap();
    assert_val_eq!(live, Value::T);

    let stale = builtin_terminal_live_p(&mut eval, vec![Value::fixnum(999_999)]).unwrap();
    assert!(stale.is_nil());
}

#[test]
fn terminal_name_rejects_invalid_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let result = builtin_terminal_name(&mut eval, vec![Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn eval_terminal_name_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;
    let result = builtin_terminal_name(&mut eval, vec![Value::make_frame(frame_id)]).unwrap();
    assert_val_eq!(result, Value::string("initial_terminal"));
}

#[test]
fn frame_terminal_rejects_non_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let result = builtin_frame_terminal(&mut eval, vec![Value::string("not-a-frame")]);
    assert!(result.is_err());
}

#[test]
fn frame_terminal_accepts_frame_id() {
    let mut eval = crate::emacs_core::Context::new();
    let result = builtin_frame_terminal(&mut eval, vec![Value::fixnum(1)]);
    assert!(result.is_ok());
    let handle = result.unwrap();
    let live = builtin_terminal_live_p(&mut eval, vec![handle]).unwrap();
    assert_val_eq!(live, Value::T);
}

#[test]
fn frame_terminal_returns_live_terminal_handle() {
    let mut eval = crate::emacs_core::Context::new();
    let handle = builtin_frame_terminal(&mut eval, vec![Value::NIL]).unwrap();
    let live = builtin_terminal_live_p(&mut eval, vec![handle]).unwrap();
    assert_val_eq!(live, Value::T);
}

#[test]
fn selected_terminal_returns_live_terminal_handle() {
    let mut eval = crate::emacs_core::Context::new();
    let handle = builtin_selected_terminal(vec![]).unwrap();
    let live = builtin_terminal_live_p(&mut eval, vec![handle]).unwrap();
    assert_val_eq!(live, Value::T);
}

#[test]
fn selected_terminal_arity() {
    assert!(builtin_selected_terminal(vec![Value::NIL]).is_err());
}

#[test]
fn eval_frame_terminal_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;
    let handle = builtin_frame_terminal(&mut eval, vec![Value::make_frame(frame_id)]).unwrap();
    let live = builtin_terminal_live_p(&mut eval, vec![handle]).unwrap();
    assert_val_eq!(live, Value::T);
}

#[test]
fn redraw_frame_rejects_non_frame_designator() {
    let mut ctx = crate::emacs_core::Context::new();
    let result = builtin_redraw_frame(&mut ctx, vec![Value::string("not-a-frame")]);
    assert!(result.is_err());
}

#[test]
fn eval_redraw_frame_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;
    let result = builtin_redraw_frame(&mut eval, vec![Value::make_frame(frame_id)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn frame_edges_string_designator_uses_unquoted_live_frame_error_message() {
    let mut eval = crate::emacs_core::Context::new();
    let result = builtin_frame_edges(&mut eval, vec![Value::string("x")]);
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
    let mut eval = crate::emacs_core::Context::new();
    let result = builtin_frame_edges(&mut eval, vec![Value::fixnum(999_999)]);
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
    let mut eval = crate::emacs_core::Context::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let window =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();
    let result = builtin_frame_edges(&mut eval, vec![window]);
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
    let result = builtin_open_termscript(vec![Value::NIL]);
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
    let mut eval = crate::emacs_core::Context::new();
    let result =
        builtin_send_string_to_terminal(&mut eval, vec![Value::string(""), Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn send_string_to_terminal_accepts_live_terminal_handle() {
    let mut eval = crate::emacs_core::Context::new();
    let handle = terminal_handle_value();
    let result =
        builtin_send_string_to_terminal(&mut eval, vec![Value::string(""), handle]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn eval_send_string_to_terminal_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;
    let result = builtin_send_string_to_terminal(
        &mut eval,
        vec![Value::string(""), Value::make_frame(frame_id)],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_show_cursor_tracks_visibility_state() {
    reset_dispnew_thread_locals();
    let mut eval = crate::emacs_core::Context::new();
    let default_visible = builtin_internal_show_cursor_p(&mut eval, vec![]).unwrap();
    assert_val_eq!(default_visible, Value::T);

    builtin_internal_show_cursor(&mut eval, vec![Value::NIL, Value::NIL]).unwrap();
    let hidden = builtin_internal_show_cursor_p(&mut eval, vec![]).unwrap();
    assert!(hidden.is_nil());

    builtin_internal_show_cursor(&mut eval, vec![Value::NIL, Value::T]).unwrap();
    let visible = builtin_internal_show_cursor_p(&mut eval, vec![]).unwrap();
    assert_val_eq!(visible, Value::T);
}

#[test]
fn internal_show_cursor_rejects_non_window_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let result = builtin_internal_show_cursor(&mut eval, vec![Value::fixnum(1), Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn eval_internal_show_cursor_accepts_live_window_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let window =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();
    let result = builtin_internal_show_cursor(&mut eval, vec![window, Value::T]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn eval_internal_show_cursor_p_accepts_live_window_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let window =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();
    let result = builtin_internal_show_cursor_p(&mut eval, vec![window]).unwrap();
    assert!((result.is_t() || result.is_nil()));
}

#[test]
fn eval_internal_show_cursor_tracks_per_window_state() {
    reset_dispnew_thread_locals();
    let mut eval = crate::emacs_core::Context::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let selected =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();
    let other = crate::emacs_core::builtins::dispatch_builtin(
        &mut eval,
        "split-window-internal",
        vec![Value::NIL, Value::NIL, Value::NIL, Value::NIL],
    )
    .unwrap()
    .unwrap();

    assert_val_eq!(
        builtin_internal_show_cursor_p(&mut eval, vec![selected]).unwrap(),
        Value::T
    );
    assert_val_eq!(
        builtin_internal_show_cursor_p(&mut eval, vec![other]).unwrap(),
        Value::T
    );

    builtin_internal_show_cursor(&mut eval, vec![Value::NIL, Value::NIL]).unwrap();
    assert!(
        builtin_internal_show_cursor_p(&mut eval, vec![selected])
            .unwrap()
            .is_nil()
    );
    assert_val_eq!(
        builtin_internal_show_cursor_p(&mut eval, vec![other]).unwrap(),
        Value::T
    );
    assert!(
        builtin_internal_show_cursor_p(&mut eval, vec![])
            .unwrap()
            .is_nil()
    );

    builtin_internal_show_cursor(&mut eval, vec![other, Value::T]).unwrap();
    assert!(
        builtin_internal_show_cursor_p(&mut eval, vec![selected])
            .unwrap()
            .is_nil()
    );
    assert_val_eq!(
        builtin_internal_show_cursor_p(&mut eval, vec![other]).unwrap(),
        Value::T
    );
    assert!(
        builtin_internal_show_cursor_p(&mut eval, vec![])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn tty_queries_reject_invalid_terminal_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let tty_type = builtin_tty_type(&mut eval, vec![Value::fixnum(1)]);
    let tty_top_frame = builtin_tty_top_frame(&mut eval, vec![Value::fixnum(1)]);
    let controlling = builtin_controlling_tty_p(&mut eval, vec![Value::fixnum(1)]);
    assert!(tty_type.is_err());
    assert!(tty_top_frame.is_err());
    assert!(controlling.is_err());
}

#[test]
fn eval_tty_queries_accept_live_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;
    assert!(
        builtin_tty_type(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_tty_top_frame(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_controlling_tty_p(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn suspend_tty_signals_non_text_terminal_error() {
    let mut eval = crate::emacs_core::Context::new();
    for args in [vec![], vec![Value::NIL], vec![terminal_handle_value()]] {
        let result = builtin_suspend_tty(&mut eval, args);
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
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;
    let suspend = builtin_suspend_tty(&mut eval, vec![Value::make_frame(frame_id)]);
    let resume = builtin_resume_tty(&mut eval, vec![Value::make_frame(frame_id)]);
    assert!(suspend.is_err());
    assert!(resume.is_err());
}

#[test]
fn resume_tty_signals_non_text_terminal_error() {
    let mut eval = crate::emacs_core::Context::new();
    for args in [vec![], vec![Value::NIL], vec![terminal_handle_value()]] {
        let result = builtin_resume_tty(&mut eval, args);
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
    let mut eval = crate::emacs_core::Context::new();
    let bad = builtin_x_open_connection(&mut eval, vec![Value::NIL]);
    assert!(bad.is_err());
}

#[test]
fn x_open_connection_eval_accepts_x_host_startup() {
    let mut eval = crate::emacs_core::Context::new();
    eval.set_variable("initial-window-system", Value::symbol("x"));
    assert!(
        builtin_x_open_connection(&mut eval, vec![Value::NIL])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn x_window_system_resource_queries_return_nil() {
    let mut eval = crate::emacs_core::Context::new();
    eval.set_variable("initial-window-system", Value::symbol("x"));

    assert!(
        builtin_x_apply_session_resources(&mut eval, vec![])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_get_resource(
            &mut eval,
            vec![Value::string("geometry"), Value::string("Geometry")]
        )
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_x_list_fonts(&mut eval, vec![Value::string("*")])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn x_open_connection_arity_errors() {
    let mut eval = crate::emacs_core::Context::new();
    let x_open_none = builtin_x_open_connection(&mut eval, vec![]);
    let x_open_four = builtin_x_open_connection(
        &mut eval,
        vec![
            Value::string("foo"),
            Value::string("xrm"),
            Value::T,
            Value::NIL,
        ],
    );
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
    let mut eval = crate::emacs_core::Context::new();
    let x_nil = builtin_x_close_connection(&mut eval, vec![Value::NIL]);
    let x_int = builtin_x_close_connection(&mut eval, vec![Value::fixnum(1)]);
    let x_str = builtin_x_close_connection(&mut eval, vec![Value::string("")]);
    let x_term = builtin_x_close_connection(&mut eval, vec![terminal_handle_value()]);
    let x_close_none = builtin_x_close_connection(&mut eval, vec![]);
    let x_close_two = builtin_x_close_connection(&mut eval, vec![Value::string("foo"), Value::NIL]);
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
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;

    let result = builtin_x_close_connection(&mut eval, vec![Value::make_frame(frame_id)]);
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
    let mut eval = crate::emacs_core::Context::new();
    let width_none = builtin_x_display_pixel_width(&mut eval, vec![]);
    let width_int = builtin_x_display_pixel_width(&mut eval, vec![Value::fixnum(1)]);
    let width_str = builtin_x_display_pixel_width(&mut eval, vec![Value::string("")]);
    let width_term = builtin_x_display_pixel_width(&mut eval, vec![terminal_handle_value()]);
    let height_none = builtin_x_display_pixel_height(&mut eval, vec![]);
    let height_int = builtin_x_display_pixel_height(&mut eval, vec![Value::fixnum(1)]);
    let height_str = builtin_x_display_pixel_height(&mut eval, vec![Value::string("")]);
    let height_term = builtin_x_display_pixel_height(&mut eval, vec![terminal_handle_value()]);
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
    let mut eval = crate::emacs_core::Context::new();
    let term = terminal_handle_value();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;

    type EvalXQuery = fn(&mut crate::emacs_core::eval::Context, Vec<Value>) -> EvalResult;
    for eval_query in [
        builtin_x_display_backing_store as EvalXQuery,
        builtin_x_display_color_cells,
        builtin_x_display_mm_height,
        builtin_x_display_mm_width,
        builtin_x_display_monitor_attributes_list,
        builtin_x_display_planes,
        builtin_x_display_save_under,
        builtin_x_display_screens,
        builtin_x_display_visual_class,
        builtin_x_server_input_extension_version,
        builtin_x_server_vendor,
    ] {
        match eval_query(&mut eval, vec![]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("X windows are not in use or not initialized")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }

        match eval_query(&mut eval, vec![term]) {
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

        match eval_query(&mut eval, vec![Value::string("x")]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string("Display x can\u{2019}t be opened")]
                );
            }
            other => panic!("expected error signal, got {other:?}"),
        }

        match eval_query(&mut eval, vec![Value::fixnum(1)]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
                );
            }
            other => panic!("expected wrong-type-argument signal, got {other:?}"),
        }

        match eval_query(&mut eval, vec![Value::make_frame(frame_id)]) {
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
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let frame = Value::fixnum(frame_id.0 as i64);
    eval.set_variable("initial-window-system", Value::NIL);
    eval.set_variable("window-system", Value::symbol(gui_window_system_symbol()));
    eval.frames
        .get_mut(frame_id)
        .expect("selected frame")
        .set_window_system(Some(Value::symbol(gui_window_system_symbol())));

    assert_val_eq!(
        builtin_x_display_grayscale_p(&mut eval, vec![]).unwrap(),
        Value::T
    );
    assert_val_eq!(
        builtin_x_display_grayscale_p(&mut eval, vec![frame]).unwrap(),
        Value::T
    );
    assert_val_eq!(
        builtin_x_display_color_cells(&mut eval, vec![Value::NIL]).unwrap(),
        Value::fixnum(16_777_216)
    );
    assert_val_eq!(
        builtin_x_display_color_cells(&mut eval, vec![frame]).unwrap(),
        Value::fixnum(16_777_216)
    );
    assert_val_eq!(
        builtin_x_display_planes(&mut eval, vec![Value::NIL]).unwrap(),
        Value::fixnum(24)
    );
    assert_val_eq!(
        builtin_x_display_planes(&mut eval, vec![frame]).unwrap(),
        Value::fixnum(24)
    );
    assert_val_eq!(
        builtin_x_display_visual_class(&mut eval, vec![Value::NIL]).unwrap(),
        Value::symbol("true-color")
    );
    assert_val_eq!(
        builtin_x_display_visual_class(&mut eval, vec![frame]).unwrap(),
        Value::symbol("true-color")
    );
}

#[test]
fn display_queries_default_to_selected_frame_window_system_surface() {
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let frame = Value::fixnum(frame_id.0 as i64);

    eval.frames
        .get_mut(frame_id)
        .expect("selected frame")
        .set_window_system(Some(Value::symbol(gui_window_system_symbol())));
    eval.set_variable("initial-window-system", Value::NIL);
    eval.set_variable("window-system", Value::NIL);

    assert_val_eq!(
        builtin_display_graphic_p(&mut eval, vec![]).unwrap(),
        Value::T
    );
    assert_val_eq!(
        builtin_display_color_cells(&mut eval, vec![]).unwrap(),
        Value::fixnum(16_777_216)
    );
    assert_val_eq!(
        builtin_display_color_cells(&mut eval, vec![frame]).unwrap(),
        Value::fixnum(16_777_216)
    );
    assert_val_eq!(
        crate::emacs_core::builtins::symbols::builtin_xw_display_color_p_ctx(
            &eval,
            vec![Value::NIL],
        )
        .unwrap(),
        Value::T
    );
    assert_val_eq!(
        crate::emacs_core::builtins::symbols::builtin_xw_display_color_p_ctx(&eval, vec![frame],)
            .unwrap(),
        Value::T
    );
    assert_val_eq!(
        builtin_display_planes(&mut eval, vec![]).unwrap(),
        Value::fixnum(24)
    );
    assert_val_eq!(
        builtin_display_visual_class(&mut eval, vec![]).unwrap(),
        Value::symbol("true-color")
    );
    assert_val_eq!(
        builtin_x_display_color_cells(&mut eval, vec![Value::NIL]).unwrap(),
        Value::fixnum(16_777_216)
    );
    assert_val_eq!(
        builtin_x_display_visual_class(&mut eval, vec![frame]).unwrap(),
        Value::symbol("true-color")
    );
}

#[test]
fn x_display_set_last_user_time_batch_semantics() {
    let mut eval = crate::emacs_core::Context::new();

    match builtin_x_display_set_last_user_time(&mut eval, vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("X windows are not in use or not initialized")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(&mut eval, vec![Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("X windows are not in use or not initialized")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(&mut eval, vec![Value::string("x"), Value::NIL]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("X windows are not in use or not initialized")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(&mut eval, vec![Value::NIL, Value::string("x")]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Display x can’t be opened")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(&mut eval, vec![Value::NIL, terminal_handle_value()])
    {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Terminal 0 is not an X display")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(&mut eval, vec![Value::NIL, Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(&mut eval, vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    match builtin_x_display_set_last_user_time(
        &mut eval,
        vec![Value::NIL, Value::fixnum(1), Value::NIL],
    ) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_display_set_last_user_time_eval_uses_user_time_designator_payloads() {
    let mut eval = crate::emacs_core::Context::new();
    let term = terminal_handle_value();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;

    for display in [
        Value::NIL,
        Value::string("display"),
        Value::fixnum(1),
        Value::symbol("foo"),
        Value::make_frame(frame_id),
        term,
    ] {
        match builtin_x_display_set_last_user_time(&mut eval, vec![display, Value::string("x")]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(sig.data, vec![Value::string("Display x can’t be opened")]);
            }
            other => panic!("expected error signal, got {other:?}"),
        }

        match builtin_x_display_set_last_user_time(
            &mut eval,
            vec![display, Value::make_frame(frame_id)],
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

        match builtin_x_display_set_last_user_time(&mut eval, vec![display, term]) {
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
        builtin_x_selection_owner_p(vec![Value::symbol("PRIMARY"), Value::fixnum(1)])
            .unwrap()
            .is_nil()
    );
    match builtin_x_selection_exists_p(vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("symbolp"), Value::fixnum(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_selection_owner_p(vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("symbolp"), Value::fixnum(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    assert!(builtin_x_uses_old_gtk_dialog(vec![]).unwrap().is_nil());
    match builtin_x_uses_old_gtk_dialog(vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_geometry_fonts_and_resource_batch_semantics() {
    assert_val_eq!(
        builtin_x_parse_geometry(vec![Value::string("80x24+10+20")]).unwrap(),
        Value::list(vec![
            Value::cons(Value::symbol("height"), Value::fixnum(24)),
            Value::cons(Value::symbol("width"), Value::fixnum(80)),
            Value::cons(Value::symbol("top"), Value::fixnum(20)),
            Value::cons(Value::symbol("left"), Value::fixnum(10)),
        ])
    );
    assert_val_eq!(
        builtin_x_parse_geometry(vec![Value::string("80x24")]).unwrap(),
        Value::list(vec![
            Value::cons(Value::symbol("height"), Value::fixnum(24)),
            Value::cons(Value::symbol("width"), Value::fixnum(80)),
        ])
    );
    assert!(
        builtin_x_parse_geometry(vec![Value::string("x")])
            .unwrap()
            .is_nil()
    );
    match builtin_x_parse_geometry(vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    assert!(builtin_x_family_fonts(vec![]).unwrap().is_nil());
    assert!(
        builtin_x_family_fonts(vec![Value::string("abc"), Value::NIL])
            .unwrap()
            .is_nil()
    );
    match builtin_x_family_fonts(vec![Value::fixnum(1), Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_family_fonts(vec![Value::fixnum(1), Value::NIL]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }

    let mut eval = crate::emacs_core::Context::new();

    match builtin_x_list_fonts(&mut eval, vec![Value::NIL]) {
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

    match builtin_x_get_resource(&mut eval, vec![Value::NIL, Value::NIL]) {
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
    match builtin_x_get_resource(&mut eval, vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_property_and_frame_arg_batch_semantics() {
    for args in [vec![], vec![Value::NIL], vec![Value::make_frame(1)]] {
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
    match builtin_x_backspace_delete_keys_p(vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
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
    match builtin_x_get_atom_name(vec![Value::symbol("WM_CLASS"), Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
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
    match builtin_x_window_property(vec![Value::string("WM_NAME"), Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_window_property(vec![
        Value::string("WM_NAME"),
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
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
    match builtin_x_window_property_attributes(vec![Value::string("WM_NAME"), Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_window_property_attributes(vec![
        Value::string("WM_NAME"),
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_coordinate_sync_and_message_batch_semantics() {
    let term = terminal_handle_value();

    for args in [
        vec![Value::NIL],
        vec![Value::NIL, Value::NIL],
        vec![Value::make_frame(1)],
        vec![Value::fixnum(1), Value::NIL],
        vec![Value::string("x"), Value::NIL],
        vec![term, Value::NIL],
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

    match builtin_x_translate_coordinates(vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("X windows are not in use or not initialized")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_translate_coordinates(vec![Value::make_frame(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_translate_coordinates(vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
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
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
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
    match builtin_x_frame_list_z_order(vec![Value::make_frame(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_frame_list_z_order(vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
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
    match builtin_x_frame_list_z_order(vec![Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    match builtin_x_send_client_message(vec![
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
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
        Value::make_frame(1),
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
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
        Value::fixnum(1),
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_send_client_message(vec![
        Value::string("x"),
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Display x can’t be opened")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_send_client_message(vec![
        term,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
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
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_popup_dialog_and_menu_batch_semantics() {
    let term = terminal_handle_value();

    match builtin_x_popup_dialog(vec![Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("windowp"), Value::NIL]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_popup_dialog(vec![Value::make_frame(1), Value::NIL]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::NIL]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    match builtin_x_popup_dialog(vec![
        Value::make_frame(1),
        Value::list(vec![Value::string("A")]),
    ]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("consp"), Value::NIL]);
        }
        other => panic!("expected wrong-type-argument signal, got {other:?}"),
    }
    assert!(
        builtin_x_popup_dialog(vec![
            Value::make_frame(1),
            Value::list(vec![
                Value::string("Title"),
                Value::cons(Value::string("Yes"), Value::T),
            ]),
        ])
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_x_popup_dialog(vec![
            Value::make_frame(1),
            Value::list(vec![Value::string("A"), Value::fixnum(1)]),
        ])
        .unwrap()
        .is_nil()
    );
    for arg in [Value::string("x"), Value::fixnum(1), term] {
        match builtin_x_popup_dialog(vec![arg, Value::NIL]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("windowp"), Value::NIL]);
            }
            other => panic!("expected wrong-type-argument signal, got {other:?}"),
        }
    }
    match builtin_x_popup_dialog(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_popup_dialog(vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_popup_dialog(vec![Value::NIL, Value::NIL, Value::NIL, Value::NIL]) {
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
        Value::cons(Value::string("Yes"), Value::T),
    ]);

    assert!(
        builtin_x_popup_menu(vec![Value::NIL, Value::NIL])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_popup_menu(vec![Value::NIL, basic_menu])
            .unwrap()
            .is_nil()
    );
    for pos in [
        Value::make_frame(1),
        Value::string("x"),
        Value::fixnum(1),
        term,
    ] {
        assert_wta(builtin_x_popup_menu(vec![pos, Value::NIL]), "listp", pos);
    }

    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::fixnum(0), Value::fixnum(0)]),
            Value::NIL,
        ]),
        "listp",
        Value::fixnum(0),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::fixnum(0), Value::fixnum(0)]),
            basic_menu,
        ]),
        "listp",
        Value::fixnum(0),
    );
    assert_wta(
        builtin_x_popup_menu(vec![Value::list(vec![Value::NIL]), Value::NIL]),
        "stringp",
        Value::NIL,
    );
    assert_wta(
        builtin_x_popup_menu(vec![Value::list(vec![Value::NIL]), basic_menu]),
        "consp",
        Value::T,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::symbol("menu-bar")]),
            Value::NIL,
        ]),
        "stringp",
        Value::NIL,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::symbol("menu-bar")]),
            basic_menu,
        ]),
        "consp",
        Value::T,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::symbol("mouse-1")]),
            Value::NIL,
        ]),
        "stringp",
        Value::NIL,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::symbol("mouse-1")]),
            basic_menu,
        ]),
        "consp",
        Value::T,
    );

    assert_wta(
        builtin_x_popup_menu(vec![Value::list(vec![Value::NIL, Value::NIL]), Value::NIL]),
        "stringp",
        Value::NIL,
    );
    assert_wta(
        builtin_x_popup_menu(vec![Value::list(vec![Value::NIL, Value::NIL]), basic_menu]),
        "consp",
        Value::T,
    );
    assert!(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::list(vec![Value::string("A")]),
        ])
        .unwrap()
        .is_nil()
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::list(vec![Value::string("A"), Value::fixnum(1)]),
        ]),
        "listp",
        Value::fixnum(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::list(vec![
                Value::fixnum(1),
                Value::cons(Value::string("Yes"), Value::T),
            ]),
        ]),
        "stringp",
        Value::fixnum(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::list(vec![Value::cons(Value::string("A"), Value::T)]),
        ]),
        "stringp",
        Value::cons(Value::string("A"), Value::T),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::fixnum(1),
        ]),
        "listp",
        Value::fixnum(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::string("x"),
        ]),
        "listp",
        Value::string("x"),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::list(vec![Value::string("A"), Value::NIL]),
        ]),
        "stringp",
        Value::NIL,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::list(vec![
                Value::string("A"),
                Value::list(vec![Value::string("Pane")]),
            ]),
        ]),
        "consp",
        Value::NIL,
    );
    assert!(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::list(vec![
                Value::string("A"),
                Value::list(vec![Value::string("Pane"), Value::NIL]),
            ]),
        ])
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::list(vec![
                Value::string("A"),
                Value::list(vec![
                    Value::string("Pane"),
                    Value::cons(Value::string("Y"), Value::T),
                ]),
            ]),
        ])
        .unwrap()
        .is_nil()
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::list(vec![
                Value::string("A"),
                Value::cons(Value::string("Pane"), Value::fixnum(1)),
            ]),
        ]),
        "consp",
        Value::fixnum(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::NIL, Value::NIL]),
            Value::list(vec![
                Value::string("A"),
                Value::cons(Value::fixnum(1), Value::fixnum(2)),
            ]),
        ]),
        "stringp",
        Value::fixnum(1),
    );

    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::list(vec![Value::fixnum(0), Value::fixnum(0)])]),
            Value::NIL,
        ]),
        "windowp",
        Value::NIL,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![Value::list(vec![Value::fixnum(0), Value::fixnum(0)])]),
            basic_menu,
        ]),
        "windowp",
        Value::NIL,
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![
                Value::list(vec![Value::fixnum(0), Value::fixnum(0)]),
                Value::fixnum(1),
            ]),
            Value::NIL,
        ]),
        "windowp",
        Value::fixnum(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::list(vec![
                Value::list(vec![Value::fixnum(0), Value::fixnum(0)]),
                Value::fixnum(1),
            ]),
            basic_menu,
        ]),
        "windowp",
        Value::fixnum(1),
    );
    assert_wta(
        builtin_x_popup_menu(vec![
            Value::cons(
                Value::list(vec![Value::fixnum(0), Value::fixnum(0)]),
                Value::fixnum(0),
            ),
            Value::NIL,
        ]),
        "listp",
        Value::fixnum(0),
    );
    match builtin_x_popup_menu(vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_popup_menu(vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_popup_menu(vec![Value::NIL, Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_clipboard_input_context_batch_semantics() {
    let term = terminal_handle_value();
    let frame = Value::make_frame(1);

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
    assert_wrong_number(builtin_x_get_clipboard(vec![Value::NIL]));

    assert_error(
        builtin_x_get_modifier_masks(vec![]),
        "X windows are not in use or not initialized",
    );
    assert_error(
        builtin_x_get_modifier_masks(vec![Value::NIL]),
        "X windows are not in use or not initialized",
    );
    assert_error(
        builtin_x_get_modifier_masks(vec![term]),
        "Terminal 0 is not an X display",
    );
    assert_wrong_type(
        builtin_x_get_modifier_masks(vec![Value::fixnum(1)]),
        "frame-live-p",
        Value::fixnum(1),
    );
    assert_error(
        builtin_x_get_modifier_masks(vec![Value::string("x")]),
        "Display x can’t be opened",
    );
    assert_error(
        builtin_x_get_modifier_masks(vec![frame]),
        "Window system frame should be used",
    );
    assert_wrong_number(builtin_x_get_modifier_masks(vec![Value::NIL, Value::NIL]));

    assert!(builtin_x_hide_tip(vec![]).unwrap().is_nil());
    assert_wrong_number(builtin_x_hide_tip(vec![Value::NIL]));

    assert_wrong_type(
        builtin_x_setup_function_keys(vec![Value::NIL]),
        "frame-live-p",
        Value::NIL,
    );
    assert_wrong_type(
        builtin_x_setup_function_keys(vec![term]),
        "frame-live-p",
        term,
    );
    assert_wrong_type(
        builtin_x_setup_function_keys(vec![Value::fixnum(1)]),
        "terminal-live-p",
        Value::fixnum(1),
    );
    assert_wrong_type(
        builtin_x_setup_function_keys(vec![Value::string("x")]),
        "terminal-live-p",
        Value::string("x"),
    );
    assert!(builtin_x_setup_function_keys(vec![frame]).unwrap().is_nil());
    assert_wrong_number(builtin_x_setup_function_keys(vec![]));
    assert_wrong_number(builtin_x_setup_function_keys(vec![Value::NIL, Value::NIL]));

    for arg in [
        Value::NIL,
        term,
        Value::fixnum(1),
        Value::string("x"),
        frame,
    ] {
        assert!(
            builtin_x_internal_focus_input_context(vec![arg])
                .unwrap()
                .is_nil()
        );
    }
    assert_wrong_number(builtin_x_internal_focus_input_context(vec![]));
    assert_wrong_number(builtin_x_internal_focus_input_context(vec![
        Value::NIL,
        Value::NIL,
    ]));

    assert_error(
        builtin_x_wm_set_size_hint(vec![]),
        "Window system frame should be used",
    );
    assert_error(
        builtin_x_wm_set_size_hint(vec![Value::NIL]),
        "Window system frame should be used",
    );
    assert_wrong_type(
        builtin_x_wm_set_size_hint(vec![term]),
        "frame-live-p",
        terminal_handle_value(),
    );
    assert_wrong_type(
        builtin_x_wm_set_size_hint(vec![Value::fixnum(1)]),
        "frame-live-p",
        Value::fixnum(1),
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
    assert_wrong_number(builtin_x_wm_set_size_hint(vec![Value::NIL, Value::NIL]));
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

    let mut eval = crate::emacs_core::Context::new();

    assert_error(
        builtin_x_apply_session_resources(&mut eval, vec![]),
        "Window system is not in use or not initialized",
    );
    assert_wrong_number(builtin_x_apply_session_resources(
        &mut eval,
        vec![Value::NIL],
    ));

    assert_error(
        builtin_x_change_window_property(vec![Value::string("P"), Value::string("V")]),
        "Window system frame should be used",
    );
    assert_error(
        builtin_x_change_window_property(vec![Value::string("P"), Value::string("V"), Value::NIL]),
        "Window system frame should be used",
    );
    assert_error(
        builtin_x_change_window_property(vec![
            Value::string("P"),
            Value::string("V"),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ]),
        "Window system frame should be used",
    );
    assert_wrong_number(builtin_x_change_window_property(vec![
        Value::string("P"),
        Value::string("V"),
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]));

    assert_error(
        builtin_x_delete_window_property(vec![Value::string("P")]),
        "Window system frame should be used",
    );
    assert_error(
        builtin_x_delete_window_property(vec![Value::string("P"), Value::NIL]),
        "Window system frame should be used",
    );
    assert_error(
        builtin_x_delete_window_property(vec![Value::string("P"), Value::NIL, Value::NIL]),
        "Window system frame should be used",
    );
    assert_wrong_number(builtin_x_delete_window_property(vec![
        Value::string("P"),
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]));

    assert!(
        builtin_x_disown_selection_internal(vec![Value::NIL])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_disown_selection_internal(vec![Value::NIL, Value::NIL])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_disown_selection_internal(vec![Value::NIL, Value::NIL, Value::NIL])
            .unwrap()
            .is_nil()
    );
    assert_wrong_number(builtin_x_disown_selection_internal(vec![]));
    assert_wrong_number(builtin_x_disown_selection_internal(vec![
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]));

    assert_wrong_type(builtin_x_get_local_selection(vec![]), "consp", Value::NIL);
    assert_wrong_type(
        builtin_x_get_local_selection(vec![Value::NIL]),
        "consp",
        Value::NIL,
    );
    assert_wrong_type(
        builtin_x_get_local_selection(vec![Value::NIL, Value::NIL]),
        "consp",
        Value::NIL,
    );
    assert_wrong_number(builtin_x_get_local_selection(vec![
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]));

    assert_error(
        builtin_x_get_selection_internal(vec![Value::NIL, Value::NIL]),
        "X selection unavailable for this frame",
    );
    assert_error(
        builtin_x_get_selection_internal(vec![Value::NIL, Value::NIL, Value::NIL]),
        "X selection unavailable for this frame",
    );
    assert_error(
        builtin_x_get_selection_internal(vec![Value::NIL, Value::NIL, Value::NIL, Value::NIL]),
        "X selection unavailable for this frame",
    );
    assert_wrong_number(builtin_x_get_selection_internal(vec![]));
    assert_wrong_number(builtin_x_get_selection_internal(vec![
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]));

    assert_error(
        builtin_x_own_selection_internal(vec![Value::NIL, Value::NIL]),
        "X selection unavailable for this frame",
    );
    assert_error(
        builtin_x_own_selection_internal(vec![Value::NIL, Value::NIL, Value::NIL]),
        "X selection unavailable for this frame",
    );
    assert_wrong_number(builtin_x_own_selection_internal(vec![Value::NIL]));
    assert_wrong_number(builtin_x_own_selection_internal(vec![
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]));

    assert_error(
        builtin_x_show_tip(vec![Value::string("m")]),
        "Window system frame should be used",
    );
    assert_wrong_type(
        builtin_x_show_tip(vec![Value::fixnum(1)]),
        "stringp",
        Value::fixnum(1),
    );
    assert_error(
        builtin_x_show_tip(vec![
            Value::string("m"),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ]),
        "Window system frame should be used",
    );
    assert_wrong_number(builtin_x_show_tip(vec![]));
    assert_wrong_number(builtin_x_show_tip(vec![
        Value::string("m"),
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
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
        builtin_gui_get_selection(vec![Value::NIL])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_gui_get_selection(vec![Value::NIL, Value::NIL])
            .unwrap()
            .is_nil()
    );
    assert_wrong_number(builtin_gui_get_selection(vec![
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]));

    assert_error(
        builtin_gui_get_primary_selection(vec![]),
        "No selection is available",
    );
    assert_wrong_number(builtin_gui_get_primary_selection(vec![Value::NIL]));

    assert!(
        builtin_gui_select_text(vec![Value::string("a")])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_gui_select_text(vec![Value::fixnum(1)])
            .unwrap()
            .is_nil()
    );
    assert_wrong_number(builtin_gui_select_text(vec![
        Value::string("a"),
        Value::NIL,
    ]));

    assert!(builtin_gui_selection_value(vec![]).unwrap().is_nil());
    assert_wrong_number(builtin_gui_selection_value(vec![Value::NIL]));

    assert!(
        builtin_gui_set_selection(vec![Value::NIL, Value::NIL])
            .unwrap()
            .is_nil()
    );
    assert_wrong_number(builtin_gui_set_selection(vec![
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]));
}

#[test]
fn x_frame_restack_safe_arity_surface() {
    match builtin_x_frame_restack(vec![Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Window system frame should be used")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
    match builtin_x_frame_restack(vec![Value::NIL, Value::NIL, Value::NIL]) {
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
    match builtin_x_frame_restack(vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_frame_restack(vec![Value::NIL, Value::NIL, Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn x_frame_mouse_and_dnd_batch_semantics() {
    let term = terminal_handle_value();

    for args in [
        vec![],
        vec![Value::NIL],
        vec![Value::make_frame(1)],
        vec![Value::NIL, Value::NIL],
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
    for arg in [Value::fixnum(1), Value::string("x"), term] {
        match builtin_x_export_frames(vec![arg]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), arg]);
            }
            other => panic!("expected wrong-type-argument signal, got {other:?}"),
        }
    }
    match builtin_x_export_frames(vec![Value::NIL, Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    for args in [
        vec![Value::NIL],
        vec![Value::make_frame(1)],
        vec![Value::NIL, Value::NIL],
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
    for arg in [Value::fixnum(1), Value::string("x"), term] {
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
    assert!(builtin_x_frame_edges(vec![Value::NIL]).unwrap().is_nil());
    assert!(
        builtin_x_frame_edges(vec![Value::make_frame(1)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_frame_edges(vec![Value::NIL, Value::NIL])
            .unwrap()
            .is_nil()
    );
    match builtin_x_frame_edges(vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
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
    match builtin_x_frame_edges(vec![Value::NIL, Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    assert!(builtin_x_frame_geometry(vec![]).unwrap().is_nil());
    assert!(builtin_x_frame_geometry(vec![Value::NIL]).unwrap().is_nil());
    assert!(
        builtin_x_frame_geometry(vec![Value::make_frame(1)])
            .unwrap()
            .is_nil()
    );
    match builtin_x_frame_geometry(vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
            );
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
    match builtin_x_frame_geometry(vec![Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    assert!(
        builtin_x_mouse_absolute_pixel_position(vec![])
            .unwrap()
            .is_nil()
    );
    match builtin_x_mouse_absolute_pixel_position(vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    assert!(
        builtin_x_set_mouse_absolute_pixel_position(vec![Value::NIL, Value::NIL])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_x_set_mouse_absolute_pixel_position(vec![Value::fixnum(1), Value::fixnum(2)])
            .unwrap()
            .is_nil()
    );
    match builtin_x_set_mouse_absolute_pixel_position(vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
    match builtin_x_set_mouse_absolute_pixel_position(vec![Value::NIL, Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }

    for args in [
        vec![Value::NIL],
        vec![Value::make_frame(1)],
        vec![Value::fixnum(1)],
        vec![terminal_handle_value()],
        vec![Value::NIL, Value::NIL],
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
    match builtin_x_register_dnd_atom(vec![Value::NIL, Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments signal, got {other:?}"),
    }
}

#[test]
fn eval_x_display_queries_accept_live_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;

    let width = builtin_x_display_pixel_width(&mut eval, vec![Value::make_frame(frame_id)]);
    let height = builtin_x_display_pixel_height(&mut eval, vec![Value::make_frame(frame_id)]);
    assert!(width.is_err());
    assert!(height.is_err());
}

#[test]
fn eval_monitor_attributes_include_bootstrapped_frame() {
    let mut eval = crate::emacs_core::Context::new();
    let list = builtin_display_monitor_attributes_list(&mut eval, vec![]).unwrap();
    let monitors = list_to_vec(&list).expect("monitor list");
    let attrs = list_to_vec(monitors.first().expect("first monitor")).expect("monitor attrs");

    let mut frames_value = Value::NIL;
    for attr in attrs {
        if attr.is_cons() {
            let pair_car = attr.cons_car();
            let pair_cdr = attr.cons_cdr();
            if pair_car.is_symbol_named("frames") {
                frames_value = pair_cdr;
                break;
            }
        }
    }

    let frames = list_to_vec(&frames_value).expect("frames list");
    assert_eq!(frames.len(), 1);
    assert!(frames.first().map_or(false, |v| v.is_frame()));
    assert!(!frames[0].is_integer());
    assert_val_eq!(
        crate::emacs_core::window_cmds::builtin_framep(&mut eval, vec![frames[0]]).unwrap(),
        Value::T
    );
    assert_val_eq!(
        crate::emacs_core::window_cmds::builtin_frame_live_p(&mut eval, vec![frames[0]]).unwrap(),
        Value::T
    );
}

#[test]
fn eval_monitor_queries_accept_live_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;

    let list =
        builtin_display_monitor_attributes_list(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap();
    let monitors = list_to_vec(&list).expect("monitor list");
    assert_eq!(monitors.len(), 1);

    let attrs =
        builtin_frame_monitor_attributes(&mut eval, vec![Value::make_frame(frame_id)]).unwrap();
    let attr_list = list_to_vec(&attrs).expect("monitor attrs");
    assert!(!attr_list.is_empty());
}

#[test]
fn eval_monitor_queries_accept_frame_handle_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let list = builtin_display_monitor_attributes_list(&mut eval, vec![]).unwrap();
    let monitors = list_to_vec(&list).expect("monitor list");
    let attrs = list_to_vec(monitors.first().expect("first monitor")).expect("monitor attrs");

    let mut frame = Value::NIL;
    for attr in attrs {
        if attr.is_cons() {
            let pair_car = attr.cons_car();
            let pair_cdr = attr.cons_cdr();
            if pair_car.is_symbol_named("frames") {
                let frames = list_to_vec(&pair_cdr).expect("frames list");
                frame = frames.first().cloned().expect("first frame");
                break;
            }
        }
    }
    assert!(frame.is_frame());

    let by_display = builtin_display_monitor_attributes_list(&mut eval, vec![frame]).unwrap();
    let display_list = list_to_vec(&by_display).expect("monitor list");
    assert_eq!(display_list.len(), 1);

    let by_frame = builtin_frame_monitor_attributes(&mut eval, vec![frame]).unwrap();
    let frame_attrs = list_to_vec(&by_frame).expect("monitor attrs");
    assert!(!frame_attrs.is_empty());
}

#[test]
fn eval_display_queries_accept_live_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;

    assert!(
        builtin_display_graphic_p(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert_val_eq!(
        builtin_display_pixel_width(&mut eval, vec![Value::make_frame(frame_id)]).unwrap(),
        Value::fixnum(80)
    );
    assert_val_eq!(
        builtin_display_pixel_height(&mut eval, vec![Value::make_frame(frame_id)]).unwrap(),
        Value::fixnum(25)
    );
    assert!(
        builtin_display_mm_width(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_mm_height(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert_val_eq!(
        builtin_display_screens(&mut eval, vec![Value::make_frame(frame_id)]).unwrap(),
        Value::fixnum(1)
    );
    assert_val_eq!(
        builtin_display_color_cells(&mut eval, vec![Value::make_frame(frame_id)]).unwrap(),
        Value::fixnum(0)
    );
    assert_val_eq!(
        builtin_display_planes(&mut eval, vec![Value::make_frame(frame_id)]).unwrap(),
        Value::fixnum(3)
    );
    assert_val_eq!(
        builtin_display_visual_class(&mut eval, vec![Value::make_frame(frame_id)]).unwrap(),
        Value::symbol("static-gray")
    );
    assert_val_eq!(
        builtin_display_backing_store(&mut eval, vec![Value::make_frame(frame_id)]).unwrap(),
        Value::symbol("not-useful")
    );
    assert_val_eq!(
        builtin_display_save_under(&mut eval, vec![Value::make_frame(frame_id)]).unwrap(),
        Value::symbol("not-useful")
    );
    assert!(
        builtin_display_selections_p(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_images_p(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_supports_face_attributes_p(
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
    let mut eval = crate::emacs_core::Context::new();

    assert_val_eq!(
        builtin_window_system(&mut eval, vec![]).unwrap(),
        Value::NIL
    );
    assert!(
        eval.frames.frame_list().is_empty(),
        "window-system should not synthesize a frame when no frame exists"
    );

    eval.set_variable("window-system", Value::symbol("tty"));
    assert_val_eq!(
        builtin_window_system(&mut eval, vec![]).unwrap(),
        Value::symbol("tty")
    );
    assert!(
        eval.frames.frame_list().is_empty(),
        "window-system should use the global fallback without synthesizing a frame"
    );

    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    eval.frames
        .get_mut(frame_id)
        .expect("selected frame")
        .parameters
        .insert("window-system".to_string(), Value::symbol("x"));

    assert_val_eq!(
        builtin_window_system(&mut eval, vec![]).unwrap(),
        Value::symbol("x")
    );
    assert_val_eq!(
        builtin_window_system(&mut eval, vec![Value::fixnum(frame_id.0 as i64)]).unwrap(),
        Value::symbol("x")
    );

    let err = builtin_window_system(&mut eval, vec![Value::string("x")]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("framep"), Value::string("x")]);
        }
        other => panic!("expected wrong-type-argument, got {other:?}"),
    }
}

#[test]
fn display_graphic_p_uses_global_window_system_without_live_frame() {
    let mut eval = crate::emacs_core::Context::new();
    eval.set_variable("initial-window-system", Value::symbol("neo"));

    assert_val_eq!(
        builtin_display_graphic_p(&mut eval, vec![]).unwrap(),
        Value::T
    );
    assert!(
        eval.frames.frame_list().is_empty(),
        "display-graphic-p should not synthesize a frame when only the global backend is known"
    );
}

#[test]
fn eval_display_queries_reject_invalid_frame_designator() {
    let mut eval = crate::emacs_core::Context::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let result = builtin_display_pixel_width(&mut eval, vec![Value::fixnum(999_999)]);
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

    let mut eval = crate::emacs_core::Context::new();
    assert_missing_display(builtin_display_graphic_p(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_pixel_width(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_pixel_height(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_mm_width(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_mm_height(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_screens(&mut eval, vec![Value::string("x")]));
    assert_missing_display(builtin_display_color_cells(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_planes(&mut eval, vec![Value::string("x")]));
    assert_missing_display(builtin_display_visual_class(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_backing_store(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_save_under(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_selections_p(
        &mut eval,
        vec![Value::string("x")],
    ));
    assert_missing_display(builtin_display_images_p(
        &mut eval,
        vec![Value::string("x")],
    ));
}

#[test]
fn eval_display_monitor_errors_render_window_designators() {
    let mut eval = crate::emacs_core::Context::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let window =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();

    let list_err = builtin_display_monitor_attributes_list(&mut eval, vec![window])
        .expect_err("window designator should be rejected");
    let frame_err = builtin_frame_monitor_attributes(&mut eval, vec![window])
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
    let mut eval = crate::emacs_core::Context::new();
    let _ = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let window =
        crate::emacs_core::window_cmds::builtin_selected_window(&mut eval, vec![]).unwrap();

    let rendered_window = format_get_device_terminal_arg_eval(&eval, &window);
    assert!(rendered_window.contains("#<window"));
    assert!(rendered_window.contains("*scratch*"));

    let rendered_integer = format_get_device_terminal_arg_eval(&eval, &Value::fixnum(1));
    assert_eq!(rendered_integer, "1");
}

#[test]
fn display_images_p_shapes_and_errors() {
    let mut eval = crate::emacs_core::Context::new();
    assert!(
        builtin_display_images_p(&mut eval, vec![])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_images_p(&mut eval, vec![Value::NIL])
            .unwrap()
            .is_nil()
    );

    match builtin_display_images_p(&mut eval, vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Invalid argument 1 in ‘get-device-terminal’")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_display_images_p(&mut eval, vec![Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {other:?}"),
    }
}

#[test]
fn display_save_under_and_display_selections_p_shapes_and_errors() {
    let mut eval = crate::emacs_core::Context::new();

    assert_val_eq!(
        builtin_display_save_under(&mut eval, vec![]).unwrap(),
        Value::symbol("not-useful")
    );
    assert_val_eq!(
        builtin_display_save_under(&mut eval, vec![Value::NIL]).unwrap(),
        Value::symbol("not-useful")
    );
    assert!(
        builtin_display_selections_p(&mut eval, vec![])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_selections_p(&mut eval, vec![Value::NIL])
            .unwrap()
            .is_nil()
    );

    match builtin_display_save_under(&mut eval, vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Invalid argument 1 in ‘get-device-terminal’")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_display_selections_p(&mut eval, vec![Value::fixnum(1)]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Invalid argument 1 in ‘get-device-terminal’")]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }

    match builtin_display_save_under(&mut eval, vec![Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {other:?}"),
    }

    match builtin_display_selections_p(&mut eval, vec![Value::NIL, Value::NIL]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {other:?}"),
    }
}

#[test]
fn display_optional_capability_queries_match_color_shapes() {
    let mut eval = crate::emacs_core::Context::new();

    for query in [
        builtin_display_grayscale_p
            as fn(&mut crate::emacs_core::eval::Context, Vec<Value>) -> EvalResult,
        builtin_display_mouse_p,
        builtin_display_popup_menus_p,
        builtin_display_symbol_keys_p,
    ] {
        assert!(query(&mut eval, vec![]).unwrap().is_nil());
        assert!(query(&mut eval, vec![Value::NIL]).unwrap().is_nil());
        assert!(
            query(&mut eval, vec![terminal_handle_value()])
                .unwrap()
                .is_nil()
        );

        match query(&mut eval, vec![Value::fixnum(1)]) {
            Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "error"),
            other => panic!("expected error signal, got {other:?}"),
        }

        match query(&mut eval, vec![Value::string("x")]) {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(sig.data, vec![Value::string("Display x does not exist")]);
            }
            other => panic!("expected error signal, got {other:?}"),
        }
    }

    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0;
    assert!(
        builtin_display_grayscale_p(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_mouse_p(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_popup_menus_p(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_symbol_keys_p(&mut eval, vec![Value::make_frame(frame_id)])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn display_supports_face_attributes_p_arity_and_nil_result() {
    let mut eval = crate::emacs_core::Context::new();
    let attrs = Value::list(vec![Value::symbol(":weight"), Value::symbol("bold")]);
    assert!(
        builtin_display_supports_face_attributes_p(&mut eval, vec![attrs])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_supports_face_attributes_p(&mut eval, vec![attrs, Value::fixnum(999_999)])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_display_supports_face_attributes_p(&mut eval, vec![Value::fixnum(1)])
            .unwrap()
            .is_nil()
    );

    match builtin_display_supports_face_attributes_p(&mut eval, vec![]) {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {other:?}"),
    }
    match builtin_display_supports_face_attributes_p(&mut eval, vec![attrs, Value::NIL, Value::NIL])
    {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {other:?}"),
    }
}
