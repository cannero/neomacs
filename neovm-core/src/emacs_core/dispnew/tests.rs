use super::pure::*;
use crate::emacs_core::error::Flow;
use crate::emacs_core::value::Value;

#[test]
fn redraw_frame_nil_returns_nil() {
    let result = builtin_redraw_frame_inner(vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn redraw_frame_rejects_non_frame_designator() {
    let result = builtin_redraw_frame_inner(vec![Value::string("not-a-frame")]);
    assert!(result.is_err());
}

#[test]
fn redraw_display_returns_nil() {
    let result = builtin_redraw_display(vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn ding_returns_nil() {
    let result = builtin_ding(vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn ding_with_arg_returns_nil() {
    let result = builtin_ding(vec![Value::True]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn open_termscript_signals_tty_error() {
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
fn send_string_to_terminal_rejects_non_string() {
    let result = builtin_send_string_to_terminal_inner(vec![Value::Int(42)]);
    assert!(result.is_err());
}

#[test]
fn send_string_to_terminal_accepts_string() {
    let result = builtin_send_string_to_terminal_inner(vec![Value::string("hello")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_show_cursor_tracks_visibility() {
    reset_dispnew_thread_locals();
    let default_visible = builtin_internal_show_cursor_p_inner(vec![]).unwrap();
    assert_eq!(default_visible, Value::True);

    builtin_internal_show_cursor_inner(vec![Value::Nil, Value::Nil]).unwrap();
    let hidden = builtin_internal_show_cursor_p_inner(vec![]).unwrap();
    assert!(hidden.is_nil());

    builtin_internal_show_cursor_inner(vec![Value::Nil, Value::True]).unwrap();
    let visible = builtin_internal_show_cursor_p_inner(vec![]).unwrap();
    assert_eq!(visible, Value::True);
}

#[test]
fn internal_show_cursor_rejects_non_window() {
    let result = builtin_internal_show_cursor_inner(vec![Value::Int(1), Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn force_window_update_no_arg_returns_t() {
    let result = builtin_force_window_update(vec![]).unwrap();
    assert_eq!(result, Value::True);
}

#[test]
fn force_window_update_with_arg_returns_nil() {
    let result = builtin_force_window_update(vec![Value::True]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn force_window_update_nil_arg_returns_t() {
    let result = builtin_force_window_update(vec![Value::Nil]).unwrap();
    assert_eq!(result, Value::True);
}

#[test]
fn eval_internal_show_cursor_per_window_state() {
    reset_dispnew_thread_locals();
    let mut eval = crate::emacs_core::Context::new();
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

    // Both start visible
    assert_eq!(
        builtin_internal_show_cursor_p(&mut eval, vec![selected]).unwrap(),
        Value::True
    );
    assert_eq!(
        builtin_internal_show_cursor_p(&mut eval, vec![other]).unwrap(),
        Value::True
    );

    // Hide selected window cursor
    builtin_internal_show_cursor(&mut eval, vec![Value::Nil, Value::Nil]).unwrap();
    assert!(
        builtin_internal_show_cursor_p(&mut eval, vec![selected])
            .unwrap()
            .is_nil()
    );
    assert_eq!(
        builtin_internal_show_cursor_p(&mut eval, vec![other]).unwrap(),
        Value::True
    );
}

#[test]
fn frame_z_order_lessp_returns_nil() {
    let result = builtin_frame_z_order_lessp(vec![Value::Nil, Value::Nil]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn frame_z_order_lessp_requires_two_args() {
    assert!(builtin_frame_z_order_lessp(vec![]).is_err());
    assert!(builtin_frame_z_order_lessp(vec![Value::Nil]).is_err());
}
