use super::pure::*;
use crate::emacs_core::error::Flow;
use crate::emacs_core::eval::Context;
use crate::emacs_core::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

struct RecordingTerminalHost {
    log: Rc<RefCell<Vec<&'static str>>>,
}

struct FailingDeleteTerminalHost;

impl TerminalHost for RecordingTerminalHost {
    fn suspend_tty(&mut self) -> Result<(), String> {
        self.log.borrow_mut().push("suspend");
        Ok(())
    }

    fn resume_tty(&mut self) -> Result<(), String> {
        self.log.borrow_mut().push("resume");
        Ok(())
    }

    fn delete_terminal(&mut self) -> Result<(), String> {
        self.log.borrow_mut().push("delete");
        Ok(())
    }
}

impl TerminalHost for FailingDeleteTerminalHost {
    fn suspend_tty(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn resume_tty(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn delete_terminal(&mut self) -> Result<(), String> {
        Err("terminal already disappeared".to_string())
    }
}

#[test]
fn terminal_name_returns_string() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let result = builtin_terminal_name(&mut eval, vec![]).unwrap();
    assert_eq!(result, Value::string(TERMINAL_NAME));
}

#[test]
fn terminal_name_accepts_nil() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let result = builtin_terminal_name(&mut eval, vec![Value::NIL]).unwrap();
    assert_eq!(result, Value::string(TERMINAL_NAME));
}

#[test]
fn terminal_list_returns_singleton_list() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let result = builtin_terminal_list(vec![]).unwrap();
    let items = crate::emacs_core::value::list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1);
    let live = builtin_terminal_live_p(&mut eval, vec![items[0]]).unwrap();
    assert_eq!(live, Value::T);
}

#[test]
fn terminal_live_p_nil_is_live() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    assert_eq!(
        builtin_terminal_live_p(&mut eval, vec![Value::NIL]).unwrap(),
        Value::T
    );
}

#[test]
fn terminal_live_p_int_is_not_live() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let result = builtin_terminal_live_p(&mut eval, vec![Value::fixnum(42)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn terminal_parameter_roundtrip() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let prev = builtin_set_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::symbol("test-param"), Value::fixnum(99)],
    )
    .unwrap();
    assert!(prev.is_nil());

    let val = builtin_terminal_parameter(&mut eval, vec![Value::NIL, Value::symbol("test-param")])
        .unwrap();
    assert_eq!(val, Value::fixnum(99));
}

#[test]
fn terminal_parameter_defaults() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let normal = builtin_terminal_parameter(
        &mut eval,
        vec![Value::NIL, Value::symbol("normal-erase-is-backspace")],
    )
    .unwrap();
    assert_eq!(normal, Value::fixnum(0));
}

#[test]
fn tty_type_returns_nil() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    assert!(builtin_tty_type(&mut eval, vec![]).unwrap().is_nil());
}

#[test]
fn tty_runtime_can_report_terminal_type_and_color_capability() {
    crate::test_utils::init_test_tracing();
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
    assert_eq!(
        builtin_tty_display_color_p(&mut eval, vec![]).unwrap(),
        Value::T
    );
    assert_eq!(
        builtin_tty_display_color_cells(&mut eval, vec![]).unwrap(),
        Value::fixnum(256)
    );
    assert_eq!(
        builtin_controlling_tty_p(&mut eval, vec![]).unwrap(),
        Value::T
    );
}

#[test]
fn tty_display_color_cells_returns_zero() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    assert_eq!(
        builtin_tty_display_color_cells(&mut eval, vec![]).unwrap(),
        Value::fixnum(0)
    );
}

#[test]
fn tty_top_frame_tracks_selected_frame_when_tty_runtime_is_active() {
    crate::test_utils::init_test_tracing();
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
        Value::make_frame(frame_id.0)
    );
}

#[test]
fn suspend_tty_signals_error() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
fn suspend_tty_runs_hook_and_invokes_terminal_host() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    configure_terminal_runtime(TerminalRuntimeConfig::interactive(
        Some("xterm-256color".to_string()),
        256,
    ));
    let log = Rc::new(RefCell::new(Vec::new()));
    set_terminal_host(Box::new(RecordingTerminalHost {
        log: Rc::clone(&log),
    }));
    eval.eval_str(
        r#"
(setq suspend-log nil)
(setq suspend-tty-functions
      (list (lambda (term) (setq suspend-log term))))
"#,
    )
    .expect("install suspend hook setup");

    assert_eq!(builtin_suspend_tty(&mut eval, vec![]).unwrap(), Value::NIL);
    assert_eq!(log.borrow().as_slice(), &["suspend"]);
    assert_eq!(
        eval.eval_str("suspend-log").expect("suspend-log value"),
        terminal_handle_value()
    );
}

#[test]
fn resume_tty_runs_hook_after_terminal_host_resume() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    configure_terminal_runtime(TerminalRuntimeConfig::interactive(
        Some("xterm-256color".to_string()),
        256,
    ));
    let log = Rc::new(RefCell::new(Vec::new()));
    set_terminal_host(Box::new(RecordingTerminalHost {
        log: Rc::clone(&log),
    }));
    builtin_suspend_tty(&mut eval, vec![]).expect("suspend tty");
    eval.eval_str(
        r#"
(setq resume-log nil)
(setq resume-tty-functions
      (list (lambda (term) (setq resume-log term))))
"#,
    )
    .expect("install resume hook setup");

    assert_eq!(builtin_resume_tty(&mut eval, vec![]).unwrap(), Value::NIL);
    assert_eq!(log.borrow().as_slice(), &["suspend", "resume"]);
    assert_eq!(
        eval.eval_str("resume-log").expect("resume-log value"),
        terminal_handle_value()
    );
}

#[test]
fn delete_terminal_nil_signals_sole_terminal_error() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    match builtin_delete_terminal(&mut eval, vec![]) {
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
fn delete_terminal_force_marks_terminal_dead_and_clears_terminal_list() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let handle = terminal_handle_value();

    assert_eq!(
        builtin_delete_terminal(&mut eval, vec![Value::NIL, Value::T]).unwrap(),
        Value::NIL
    );
    assert!(
        builtin_terminal_live_p(&mut eval, vec![handle])
            .unwrap()
            .is_nil(),
        "deleted terminal should no longer be live"
    );
    let terminals = builtin_terminal_list(vec![]).unwrap();
    assert!(
        crate::emacs_core::value::list_to_vec(&terminals)
            .expect("terminal-list result")
            .is_empty(),
        "deleted terminal should be removed from terminal-list"
    );
}

#[test]
fn delete_terminal_force_runs_hook_and_deletes_frames_on_terminal() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let scratch = eval.buffer_manager_mut().create_buffer("*scratch*");
    let _ = eval
        .frame_manager_mut()
        .create_frame_on_terminal("F1", TERMINAL_ID, 80, 25, scratch);
    let handle = terminal_handle_value();
    eval.eval_str(r#"
(setq deleted-terminal-log nil)
(setq delete-terminal-functions
      (list (lambda (term) (setq deleted-terminal-log term))))
"#)
    .expect("install hook setup");

    assert_eq!(
        builtin_delete_terminal(&mut eval, vec![Value::NIL, Value::T]).unwrap(),
        Value::NIL
    );
    assert!(
        eval.frame_manager().frame_list().is_empty(),
        "delete-terminal should remove frames on the terminal"
    );
    assert_eq!(
        eval.eval_str("deleted-terminal-log")
        .expect("deleted-terminal-log value"),
        handle
    );
}

#[test]
fn delete_terminal_force_defers_frame_hooks_until_pending_safe_funcalls_flush() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let scratch = eval.buffer_manager_mut().create_buffer("*scratch*");
    let _keep =
        eval.frame_manager_mut()
            .create_frame_on_terminal("F1", TERMINAL_ID, 80, 25, scratch);
    let terminal = ensure_terminal_runtime_owner(1, "secondary", TerminalRuntimeConfig::inactive());
    let doomed = eval
        .frame_manager_mut()
        .create_frame_on_terminal("F2", 1, 80, 25, scratch);
    eval.eval_str(r#"
(setq hook-log nil)
(setq delete-terminal-functions
      (list (lambda (term)
              (setq hook-log
                    (cons (list 'terminal (terminal-live-p term)) hook-log)))))
(setq delete-frame-functions
      (list (lambda (frame)
              (setq hook-log
                    (cons (list 'before (frame-live-p frame)) hook-log)))))
(setq after-delete-frame-functions
      (list (lambda (frame)
              (setq hook-log
                    (cons (list 'after (frame-live-p frame)) hook-log)))))
"#)
    .expect("install hook setup");

    assert_eq!(
        builtin_delete_terminal(&mut eval, vec![terminal, Value::T]).unwrap(),
        Value::NIL
    );
    assert!(
        eval.frames.get(doomed).is_none(),
        "delete-terminal should remove frames on that terminal immediately"
    );
    assert_eq!(
        eval.eval_str("hook-log")
            .expect("hook-log after delete-terminal"),
        Value::list(vec![Value::list(vec![Value::symbol("terminal"), Value::T])])
    );

    eval.flush_pending_safe_funcalls();

    let post_flush = eval.eval_str("(nreverse hook-log)")
        .expect("hook-log after flush");
    assert_eq!(
        format!("{}", post_flush),
        "((terminal t) (after nil) (before nil))"
    );
}

#[test]
fn delete_terminal_force_invokes_terminal_host_delete_hook() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    configure_terminal_runtime(TerminalRuntimeConfig::interactive(
        Some("xterm-256color".to_string()),
        256,
    ));
    let log = Rc::new(RefCell::new(Vec::new()));
    set_terminal_host(Box::new(RecordingTerminalHost {
        log: Rc::clone(&log),
    }));

    let mut eval = Context::new();
    assert_eq!(
        builtin_delete_terminal(&mut eval, vec![Value::NIL, Value::T]).unwrap(),
        Value::NIL
    );
    assert_eq!(log.borrow().as_slice(), &["delete"]);
}

#[test]
fn delete_terminal_noelisp_bypasses_sole_terminal_check_and_defers_hooks() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let scratch = eval.buffer_manager_mut().create_buffer("*scratch*");
    eval.buffer_manager_mut().set_current(scratch);
    let _frame =
        eval.frame_manager_mut()
            .create_frame_on_terminal("F1", TERMINAL_ID, 80, 25, scratch);
    eval.eval_str(r#"
(setq hook-log nil)
(setq delete-terminal-functions
      (list (lambda (term)
              (setq hook-log
                    (cons (list 'terminal (terminal-live-p term)) hook-log)))))
(setq delete-frame-functions
      (list (lambda (frame)
              (setq hook-log
                    (cons (list 'before (frame-live-p frame)) hook-log)))))
(setq after-delete-frame-functions
      (list (lambda (frame)
              (setq hook-log
                    (cons (list 'after (frame-live-p frame)) hook-log)))))
"#)
    .expect("install hook setup");

    assert_eq!(
        delete_terminal_noelisp_owned(&mut eval, TERMINAL_ID).unwrap(),
        Value::NIL
    );
    assert!(eval.frame_manager().frame_list().is_empty());
    assert!(
        builtin_terminal_live_p(&mut eval, vec![terminal_handle_value()])
            .unwrap()
            .is_nil(),
        "noelisp delete should mark the terminal dead even when it is the sole terminal"
    );
    assert_eq!(
        eval.eval_str("hook-log")
            .expect("hook-log before flush"),
        Value::NIL
    );

    eval.flush_pending_safe_funcalls();

    let post_flush = eval.eval_str("(nreverse hook-log)")
        .expect("hook-log after flush");
    assert_eq!(
        format!("{}", post_flush),
        "((after nil) (before nil) (terminal nil))"
    );
}

#[test]
fn delete_terminal_noelisp_ignores_host_delete_failures() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    configure_terminal_runtime(TerminalRuntimeConfig::interactive(
        Some("xterm-256color".to_string()),
        256,
    ));
    set_terminal_host(Box::new(FailingDeleteTerminalHost));

    let mut eval = Context::new();
    assert_eq!(
        delete_terminal_noelisp_owned(&mut eval, TERMINAL_ID).unwrap(),
        Value::NIL
    );
    assert!(
        builtin_terminal_live_p(&mut eval, vec![terminal_handle_value()])
            .unwrap()
            .is_nil(),
        "noelisp delete should finish even if the host is already gone"
    );
}

#[test]
fn make_terminal_frame_signals_unknown_type() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    match builtin_make_terminal_frame(vec![Value::NIL]) {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data, vec![Value::string("Unknown terminal type")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn selected_terminal_returns_live_handle() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let handle = builtin_selected_terminal(vec![]).unwrap();
    let live = builtin_terminal_live_p(&mut eval, vec![handle]).unwrap();
    assert_eq!(live, Value::T);
}

#[test]
fn frame_terminal_returns_live_handle() {
    crate::test_utils::init_test_tracing();
    reset_terminal_thread_locals();
    let mut eval = Context::new();
    let handle = builtin_frame_terminal(&mut eval, vec![Value::NIL]).unwrap();
    let live = builtin_terminal_live_p(&mut eval, vec![handle]).unwrap();
    assert_eq!(live, Value::T);
}
