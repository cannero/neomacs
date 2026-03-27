use super::pure::*;
use crate::emacs_core::error::Flow;
use crate::emacs_core::eval::Context;
use crate::emacs_core::value::Value;

#[test]
fn terminal_name_returns_string() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let result = builtin_terminal_name(&mut eval, vec![]).unwrap();
    assert_eq!(result, Value::string(TERMINAL_NAME));
}

#[test]
fn terminal_name_accepts_nil() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let result = builtin_terminal_name(&mut eval, vec![Value::Nil]).unwrap();
    assert_eq!(result, Value::string(TERMINAL_NAME));
}

#[test]
fn terminal_list_returns_singleton_list() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let result = builtin_terminal_list(vec![]).unwrap();
    let items = crate::emacs_core::value::list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1);
    let live = builtin_terminal_live_p(&mut eval, vec![items[0]]).unwrap();
    assert_eq!(live, Value::True);
}

#[test]
fn terminal_live_p_nil_is_live() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    assert_eq!(
        builtin_terminal_live_p(&mut eval, vec![Value::Nil]).unwrap(),
        Value::True
    );
}

#[test]
fn terminal_live_p_int_is_not_live() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let result = builtin_terminal_live_p(&mut eval, vec![Value::Int(42)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn terminal_parameter_roundtrip() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let prev = builtin_set_terminal_parameter(&mut eval, vec![
        Value::Nil,
        Value::symbol("test-param"),
        Value::Int(99),
    ])
    .unwrap();
    assert!(prev.is_nil());

    let val = builtin_terminal_parameter(&mut eval, vec![Value::Nil, Value::symbol("test-param")]).unwrap();
    assert_eq!(val, Value::Int(99));
}

#[test]
fn terminal_parameter_defaults() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let normal =
        builtin_terminal_parameter(&mut eval, vec![Value::Nil, Value::symbol("normal-erase-is-backspace")])
            .unwrap();
    assert_eq!(normal, Value::Int(0));
}

#[test]
fn tty_type_returns_nil() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    assert!(builtin_tty_type(&mut eval, vec![]).unwrap().is_nil());
}

#[test]
fn tty_runtime_can_report_terminal_type_and_color_capability() {
    reset_terminal_thread_locals();
    configure_terminal_runtime(TerminalRuntimeConfig::interactive(
        Some("xterm-256color".to_string()),
        256,
    ));

    let mut eval = Context::new();
    assert_eq!(
        builtin_tty_type(&mut eval, vec![]).unwrap(),
        Value::string("xterm-256color")
    );
    assert_eq!(builtin_tty_display_color_p(&mut eval, vec![]).unwrap(), Value::True);
    assert_eq!(
        builtin_tty_display_color_cells(&mut eval, vec![]).unwrap(),
        Value::Int(256)
    );
    assert_eq!(builtin_controlling_tty_p(&mut eval, vec![]).unwrap(), Value::True);
}

#[test]
fn tty_display_color_cells_returns_zero() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    assert_eq!(
        builtin_tty_display_color_cells(&mut eval, vec![]).unwrap(),
        Value::Int(0)
    );
}

#[test]
fn tty_top_frame_tracks_selected_frame_when_tty_runtime_is_active() {
    reset_terminal_thread_locals();
    configure_terminal_runtime(TerminalRuntimeConfig::interactive(
        Some("xterm-256color".to_string()),
        256,
    ));

    let mut eval = Context::new();
    let scratch = eval.buffer_manager_mut().create_buffer("*scratch*");
    let frame_id = eval.frame_manager_mut().create_frame("F1", 80, 25, scratch);

    assert_eq!(
        builtin_tty_top_frame(&mut eval, vec![]).unwrap(),
        Value::Frame(frame_id.0)
    );
}

#[test]
fn suspend_tty_signals_error() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    match builtin_suspend_tty(&mut eval, vec![]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn resume_tty_signals_error() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    match builtin_resume_tty(&mut eval, vec![]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn delete_terminal_nil_signals_sole_terminal_error() {
    reset_terminal_thread_locals();
    match builtin_delete_terminal(vec![]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "Attempt to delete the sole active display terminal"
                )]
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn make_terminal_frame_signals_unknown_type() {
    reset_terminal_thread_locals();
    match builtin_make_terminal_frame(vec![Value::Nil]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Unknown terminal type")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn selected_terminal_returns_live_handle() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let handle = builtin_selected_terminal(vec![]).unwrap();
    let live = builtin_terminal_live_p(&mut eval, vec![handle]).unwrap();
    assert_eq!(live, Value::True);
}

#[test]
fn frame_terminal_returns_live_handle() {
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let handle = builtin_frame_terminal(&mut eval, vec![Value::Nil]).unwrap();
    let live = builtin_terminal_live_p(&mut eval, vec![handle]).unwrap();
    assert_eq!(live, Value::True);
}
