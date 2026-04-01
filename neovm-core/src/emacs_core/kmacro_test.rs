use super::super::intern::intern;
use super::*;
use crate::emacs_core::autoload::is_autoload_value;
use crate::emacs_core::eval::Context;
use crate::emacs_core::load::{
    apply_ldefs_boot_autoloads_for_names, apply_runtime_startup_state,
    create_bootstrap_evaluator_cached,
};
use crate::emacs_core::{format_eval_result, parse_forms};

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    eval.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn eval_with_ldefs_boot_autoloads(names: &[&str]) -> Context {
    let mut eval = Context::new();
    for name in names {
        eval.obarray_mut().fmakunbound(name);
    }
    apply_ldefs_boot_autoloads_for_names(&mut eval, names).expect("ldefs-boot autoload restore");
    eval
}

// -----------------------------------------------------------------------
// Kmacro metadata / keyboard runtime tests
// -----------------------------------------------------------------------

#[test]
fn new_manager_defaults() {
    let mgr = KmacroManager::new();
    assert!(mgr.macro_ring.is_empty());
    assert_eq!(mgr.counter, 0);
    assert_eq!(mgr.counter_format, "%d");
}

#[test]
fn keyboard_runtime_finalize_and_cancel_match_gnu_macro_boundary_shape() {
    let mut eval = Context::new();

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start");
    assert!(eval.command_loop.keyboard.kboard.defining_kbd_macro);
    assert_eq!(
        eval.eval_symbol("defining-kbd-macro")
            .expect("defining-kbd-macro"),
        Value::T
    );

    builtin_store_kbd_macro_event(&mut eval, vec![Value::char('a')]).expect("store a");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_store_kbd_macro_event(&mut eval, vec![Value::char('b')]).expect("store b");
    crate::emacs_core::builtins::builtin_cancel_kbd_macro_events(&mut eval, vec![])
        .expect("cancel current command events");
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end");

    assert_eq!(
        eval.command_loop.last_kbd_macro(),
        Some([Value::char('a')].as_slice())
    );
    assert_eq!(
        builtin_last_kbd_macro(&mut eval, vec![]).expect("last-kbd-macro"),
        Value::vector(vec![Value::char('a')])
    );
    assert_eq!(
        eval.eval_symbol("last-kbd-macro")
            .expect("last-kbd-macro var"),
        Value::vector(vec![Value::char('a')])
    );
    assert_eq!(
        eval.eval_symbol("defining-kbd-macro")
            .expect("defining-kbd-macro"),
        Value::NIL
    );
}

#[test]
fn macro_ring_pushes_previous_keyboard_runtime_macro() {
    let mut eval = Context::new();

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start first");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::char('a')]).expect("store a");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end first");
    assert!(eval.kmacro.macro_ring.is_empty());

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start second");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::char('b')]).expect("store b");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end second");
    assert_eq!(eval.kmacro.macro_ring, vec![vec![Value::char('a')]]);

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start third");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::char('c')]).expect("store c");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end third");
    assert_eq!(
        eval.kmacro.macro_ring,
        vec![vec![Value::char('a')], vec![Value::char('b')]]
    );
}

#[test]
fn format_counter_decimal() {
    let mgr = KmacroManager {
        counter: 42,
        counter_format: "%d".to_string(),
        ..KmacroManager::new()
    };
    assert_eq!(mgr.format_counter(), "42");
}

#[test]
fn format_counter_hex() {
    let mgr = KmacroManager {
        counter: 255,
        counter_format: "%x".to_string(),
        ..KmacroManager::new()
    };
    assert_eq!(mgr.format_counter(), "ff");
}

#[test]
fn format_counter_octal() {
    let mgr = KmacroManager {
        counter: 8,
        counter_format: "%o".to_string(),
        ..KmacroManager::new()
    };
    assert_eq!(mgr.format_counter(), "10");
}

#[test]
fn format_counter_with_prefix() {
    let mgr = KmacroManager {
        counter: 7,
        counter_format: "item-%d".to_string(),
        ..KmacroManager::new()
    };
    assert_eq!(mgr.format_counter(), "item-7");
}

#[test]
fn format_counter_unknown_format() {
    let mgr = KmacroManager {
        counter: 99,
        counter_format: "???".to_string(),
        ..KmacroManager::new()
    };
    // Fallback to plain decimal
    assert_eq!(mgr.format_counter(), "99");
}

// -----------------------------------------------------------------------
// Builtin-level tests
// -----------------------------------------------------------------------

#[test]
fn test_start_and_end_macro() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // Start recording
    let result = builtin_start_kbd_macro(&mut eval, vec![]);
    assert!(result.is_ok());
    assert!(eval.command_loop.keyboard.kboard.defining_kbd_macro);

    // Double-start should error
    let result = builtin_start_kbd_macro(&mut eval, vec![]);
    assert!(result.is_err());

    // Store some events
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::char('h')]);
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::char('i')]);
    eval.finalize_kbd_macro_runtime_chars();

    // End recording
    let result = builtin_end_kbd_macro(&mut eval, vec![]);
    assert!(result.is_ok());
    assert!(!eval.command_loop.keyboard.kboard.defining_kbd_macro);
    assert_eq!(
        eval.command_loop.last_kbd_macro(),
        Some([Value::char('h'), Value::char('i')].as_slice())
    );

    // Double-end should error
    let result = builtin_end_kbd_macro(&mut eval, vec![]);
    assert!(result.is_err());
}

#[test]
fn test_defining_kbd_macro_builtin_contract() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // Arity contract.
    assert!(builtin_defining_kbd_macro(&mut eval, vec![]).is_err());
    assert!(
        builtin_defining_kbd_macro(&mut eval, vec![Value::NIL, Value::NIL, Value::NIL]).is_err()
    );

    // APPEND with no prior macro should signal wrong-type-argument.
    let append_without_last = builtin_defining_kbd_macro(&mut eval, vec![Value::T]);
    assert!(append_without_last.is_err());

    // Fresh recording with APPEND=nil should succeed.
    assert_eq!(
        builtin_defining_kbd_macro(&mut eval, vec![Value::NIL]).unwrap(),
        Value::NIL
    );
    assert!(eval.command_loop.keyboard.kboard.defining_kbd_macro);

    // Re-entry while recording should signal `error`.
    let already = builtin_defining_kbd_macro(&mut eval, vec![Value::NIL, Value::T]);
    assert!(already.is_err());

    // Finish recording and ensure append path works once a last macro exists.
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::char('a')]);
    eval.finalize_kbd_macro_runtime_chars();
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);
    assert_eq!(
        eval.command_loop.last_kbd_macro(),
        Some([Value::char('a')].as_slice())
    );
    assert_eq!(
        builtin_defining_kbd_macro(&mut eval, vec![Value::T, Value::T]).unwrap(),
        Value::NIL
    );
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);
}

#[test]
fn test_start_with_append() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // Record a macro
    let _ = builtin_start_kbd_macro(&mut eval, vec![]);
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::char('a')]);
    eval.finalize_kbd_macro_runtime_chars();
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);

    // Append to it
    let _ = builtin_start_kbd_macro(&mut eval, vec![Value::T, Value::T]);
    assert_eq!(eval.command_loop.keyboard.kboard.kbd_macro_events.len(), 1);
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::char('b')]);
    eval.finalize_kbd_macro_runtime_chars();
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);

    assert_eq!(
        eval.command_loop.last_kbd_macro(),
        Some([Value::char('a'), Value::char('b')].as_slice())
    );
}

#[test]
fn test_start_with_append_reexecutes_last_macro_when_no_exec_is_nil() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let setup = parse_forms(
        "(progn
           (setq kmacro-append-count 0)
           (fset 'kmacro-append-bump
                 (lambda ()
                   (setq kmacro-append-count (1+ kmacro-append-count)))))",
    )
    .expect("parse setup");
    let _ = eval.eval_forms(&setup);

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::symbol("kmacro-append-bump")])
        .expect("store");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end");

    assert_eq!(
        eval.eval_symbol("kmacro-append-count")
            .expect("kmacro-append-count"),
        Value::fixnum(0)
    );

    builtin_start_kbd_macro(&mut eval, vec![Value::T, Value::NIL]).expect("append");
    assert_eq!(
        eval.eval_symbol("kmacro-append-count")
            .expect("kmacro-append-count"),
        Value::fixnum(1)
    );
    assert_eq!(
        eval.command_loop.keyboard.kboard.kbd_macro_events,
        vec![Value::symbol("kmacro-append-bump")]
    );
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end append");
    assert_eq!(
        eval.command_loop.last_kbd_macro(),
        Some([Value::symbol("kmacro-append-bump")].as_slice())
    );
}

#[test]
fn test_start_with_append_real_key_macro_reexecutes_via_command_loop_and_marks_append() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let setup = parse_forms(
        r#"(progn
             (setq kmacro-append-real-count 0)
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g "a"
                 (lambda ()
                   (interactive)
                   (setq kmacro-append-real-count (1+ kmacro-append-real-count))))))"#,
    )
    .expect("parse setup");
    let _ = eval.eval_forms(&setup);

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::char('a')]).expect("store a");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end");

    assert_eq!(
        eval.eval_symbol("kmacro-append-real-count")
            .expect("kmacro-append-real-count"),
        Value::fixnum(0)
    );

    builtin_start_kbd_macro(&mut eval, vec![Value::T, Value::NIL]).expect("append");
    assert_eq!(
        eval.eval_symbol("kmacro-append-real-count")
            .expect("kmacro-append-real-count"),
        Value::fixnum(1)
    );
    assert_eq!(
        eval.eval_symbol("defining-kbd-macro")
            .expect("defining-kbd-macro"),
        Value::symbol("append")
    );
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end append");
}

#[test]
fn test_start_with_append_no_exec_skips_reexecution() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let setup = parse_forms(
        "(progn
           (setq kmacro-no-exec-count 0)
           (fset 'kmacro-no-exec-bump
                 (lambda ()
                   (setq kmacro-no-exec-count (1+ kmacro-no-exec-count)))))",
    )
    .expect("parse setup");
    let _ = eval.eval_forms(&setup);

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::symbol("kmacro-no-exec-bump")])
        .expect("store");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end");

    builtin_start_kbd_macro(&mut eval, vec![Value::T, Value::T]).expect("append");
    assert_eq!(
        eval.eval_symbol("kmacro-no-exec-count")
            .expect("kmacro-no-exec-count"),
        Value::fixnum(0)
    );
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end append");
}

#[test]
fn test_call_last_macro_no_macro() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // No macro defined -- should error
    let result = builtin_call_last_kbd_macro(&mut eval, vec![]);
    assert!(result.is_err());
}

#[test]
fn test_store_event_wrong_args() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // Wrong arg count
    let result = builtin_store_kbd_macro_event(&mut eval, vec![]);
    assert!(result.is_err());
}

#[test]
fn test_defining_executing_kbd_macro_p_builtins() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    assert_eq!(
        builtin_defining_kbd_macro_p(&mut eval, vec![]).unwrap(),
        Value::NIL
    );
    assert_eq!(
        builtin_executing_kbd_macro_p(&mut eval, vec![]).unwrap(),
        Value::NIL
    );

    eval.start_kbd_macro_runtime(None, false).unwrap();
    assert_eq!(
        builtin_defining_kbd_macro_p(&mut eval, vec![]).unwrap(),
        Value::T
    );
    let _ = eval.end_kbd_macro_runtime().unwrap();

    eval.begin_executing_kbd_macro_runtime(vec![Value::char('x')]);
    assert_eq!(
        builtin_executing_kbd_macro_p(&mut eval, vec![]).unwrap(),
        Value::T
    );
    eval.finish_executing_kbd_macro_runtime();

    assert!(builtin_defining_kbd_macro_p(&mut eval, vec![Value::NIL]).is_err());
    assert!(builtin_executing_kbd_macro_p(&mut eval, vec![Value::NIL]).is_err());
}

#[test]
fn test_execute_kbd_macro_restores_outer_execution_state() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let outer = vec![Value::char('o'), Value::char('u')];
    eval.begin_executing_kbd_macro_runtime(outer.clone());
    eval.command_loop.keyboard.kboard.kbd_macro_index = 1;

    builtin_execute_kbd_macro(&mut eval, vec![Value::vector(vec![])])
        .expect("execute nested macro");

    assert_eq!(
        eval.command_loop
            .keyboard
            .kboard
            .executing_kbd_macro
            .as_deref(),
        Some(outer.as_slice())
    );
    assert_eq!(eval.command_loop.keyboard.kboard.kbd_macro_index, 1);
    assert_eq!(
        eval.eval_symbol("executing-kbd-macro-index")
            .expect("executing-kbd-macro-index"),
        Value::fixnum(1)
    );
}

#[test]
fn test_execute_kbd_macro_real_key_events_use_command_loop_dispatch() {
    let mut eval = Context::new();
    let forms = parse_forms(
        r#"(progn
             (setq kmacro-command-loop-count 0)
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g "a"
                 (lambda ()
                   (interactive)
                   (setq kmacro-command-loop-count (1+ kmacro-command-loop-count))))
               (execute-kbd-macro "a")
               kmacro-command-loop-count))"#,
    )
    .expect("parse");
    let results = eval.eval_forms(&forms);
    assert_eq!(results.len(), 1);
    assert_eq!(format_eval_result(&results[0]), "OK 1");
}

#[test]
fn test_execute_kbd_macro_symbol_events_use_command_loop_dispatch() {
    let mut eval = Context::new();
    let forms = parse_forms(
        r#"(progn
             (setq kmacro-symbol-event-count 0)
             (setq kmacro-ignore-direct-called nil)
             (fset 'ignore
                   (lambda ()
                     (setq kmacro-ignore-direct-called t)))
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g [ignore]
                 (lambda ()
                   (interactive)
                   (setq kmacro-symbol-event-count (1+ kmacro-symbol-event-count))))
               (execute-kbd-macro [ignore])
               (list kmacro-symbol-event-count kmacro-ignore-direct-called)))"#,
    )
    .expect("parse");
    let results = eval.eval_forms(&forms);
    assert_eq!(results.len(), 1);
    assert_eq!(format_eval_result(&results[0]), "OK (1 nil)");
}

#[test]
fn test_execute_kbd_macro_named_symbol_uses_function_indirection_chain() {
    let mut eval = Context::new();
    let forms = parse_forms(
        r#"(progn
             (setq kmacro-named-symbol-count 0)
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g "a"
                 (lambda ()
                   (interactive)
                   (setq kmacro-named-symbol-count (1+ kmacro-named-symbol-count)))))
             (fset 'kmacro-target "a")
             (fset 'kmacro-alias 'kmacro-target)
             (execute-kbd-macro 'kmacro-alias)
             kmacro-named-symbol-count)"#,
    )
    .expect("parse");
    let results = eval.eval_forms(&forms);
    assert_eq!(results.len(), 1);
    assert_eq!(format_eval_result(&results[0]), "OK 1");
}

#[test]
fn test_call_last_kbd_macro_raw_prefix_repeats_real_key_macro() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let setup = parse_forms(
        r#"(progn
             (setq kmacro-call-last-count 0)
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g "a"
                 (lambda ()
                   (interactive)
                   (setq kmacro-call-last-count (1+ kmacro-call-last-count))))))"#,
    )
    .expect("parse setup");
    let _ = eval.eval_forms(&setup);

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::char('a')]).expect("store a");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end");

    builtin_call_last_kbd_macro(&mut eval, vec![Value::list(vec![Value::fixnum(4)])])
        .expect("call-last with raw prefix");

    assert_eq!(
        eval.eval_symbol("kmacro-call-last-count")
            .expect("kmacro-call-last-count"),
        Value::fixnum(4)
    );
}

#[test]
fn test_call_last_kbd_macro_symbol_events_use_command_loop_dispatch() {
    let mut eval = Context::new();
    let setup = parse_forms(
        r#"(progn
             (setq kmacro-call-last-symbol-count 0)
             (setq kmacro-call-last-ignore-direct-called nil)
             (fset 'ignore
                   (lambda ()
                     (setq kmacro-call-last-ignore-direct-called t)))
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g [ignore]
                 (lambda ()
                   (interactive)
                   (setq kmacro-call-last-symbol-count
                         (1+ kmacro-call-last-symbol-count))))))"#,
    )
    .expect("parse setup");
    let _ = eval.eval_forms(&setup);

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::symbol("ignore")]).expect("store ignore");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end");
    builtin_call_last_kbd_macro(&mut eval, vec![]).expect("call-last");

    assert_eq!(
        eval.eval_symbol("kmacro-call-last-symbol-count")
            .expect("kmacro-call-last-symbol-count"),
        Value::fixnum(1)
    );
    assert_eq!(
        eval.eval_symbol("kmacro-call-last-ignore-direct-called")
            .expect("kmacro-call-last-ignore-direct-called"),
        Value::NIL
    );
}

#[test]
fn test_execute_kbd_macro_zero_count_uses_loopfunc_for_real_key_macro() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let setup = parse_forms(
        r#"(progn
             (setq kmacro-loop-count 0)
             (setq kmacro-loopfunc-count 0)
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (fset 'kmacro-loopfunc
               (lambda ()
                 (setq kmacro-loopfunc-count (1+ kmacro-loopfunc-count))
                 (< kmacro-loopfunc-count 3)))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g "a"
                 (lambda ()
                   (interactive)
                   (setq kmacro-loop-count (1+ kmacro-loop-count))))))"#,
    )
    .expect("parse setup");
    let _ = eval.eval_forms(&setup);

    builtin_execute_kbd_macro(
        &mut eval,
        vec![
            Value::string("a"),
            Value::fixnum(0),
            Value::symbol("kmacro-loopfunc"),
        ],
    )
    .expect("execute with loopfunc");

    assert_eq!(
        eval.eval_symbol("kmacro-loop-count")
            .expect("kmacro-loop-count"),
        Value::fixnum(2)
    );
    assert_eq!(
        eval.eval_symbol("kmacro-loopfunc-count")
            .expect("kmacro-loopfunc-count"),
        Value::fixnum(3)
    );
}

#[test]
fn test_end_kbd_macro_repeat_executes_remaining_iterations() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let setup = parse_forms(
        r#"(progn
             (setq kmacro-end-repeat-count 0)
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g "a"
                 (lambda ()
                   (interactive)
                   (setq kmacro-end-repeat-count (1+ kmacro-end-repeat-count))))))"#,
    )
    .expect("parse setup");
    let _ = eval.eval_forms(&setup);

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::char('a')]).expect("store a");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![Value::fixnum(3)]).expect("end with repeat");

    assert_eq!(
        eval.eval_symbol("kmacro-end-repeat-count")
            .expect("kmacro-end-repeat-count"),
        Value::fixnum(2)
    );
    assert_eq!(
        eval.command_loop.last_kbd_macro(),
        Some([Value::char('a')].as_slice())
    );
}

#[test]
fn test_execute_kbd_macro_runs_termination_hook_after_restoring_runtime_state() {
    let mut eval = Context::new();
    let forms = parse_forms(
        r#"(progn
             (setq kmacro-term-ok nil)
             (setq real-this-command 'outer-real)
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (fset 'kmacro-term-hook
                   (lambda ()
                     (setq kmacro-term-ok
                           (and (null executing-kbd-macro)
                                (= executing-kbd-macro-index 0)
                                (eq real-this-command 'outer-real)))))
             (setq kbd-macro-termination-hook '(kmacro-term-hook))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g "a" (lambda () (interactive) 'ok)))
             (execute-kbd-macro "a"))"#,
    )
    .expect("parse");
    let _ = eval.eval_forms(&forms);

    assert_eq!(
        eval.eval_symbol("kmacro-term-ok").expect("kmacro-term-ok"),
        Value::T
    );
    assert_eq!(
        eval.eval_symbol("real-this-command")
            .expect("real-this-command"),
        Value::symbol("outer-real")
    );
}

#[test]
fn test_execute_kbd_macro_runs_termination_hook_after_error() {
    let mut eval = Context::new();
    let forms = parse_forms(
        r#"(progn
             (setq kmacro-error-term-ok nil)
             (setq real-this-command 'outer-real)
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (fset 'kmacro-error-term-hook
                   (lambda ()
                     (setq kmacro-error-term-ok
                           (and (null executing-kbd-macro)
                                (= executing-kbd-macro-index 0)
                                (eq real-this-command 'outer-real)))))
             (setq kbd-macro-termination-hook '(kmacro-error-term-hook))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g "a" (lambda () (interactive) (error "boom"))))
             (condition-case nil
                 (execute-kbd-macro "a")
               (error nil)))"#,
    )
    .expect("parse");
    let _ = eval.eval_forms(&forms);

    assert_eq!(
        eval.eval_symbol("kmacro-error-term-ok")
            .expect("kmacro-error-term-ok"),
        Value::T
    );
    assert_eq!(
        eval.eval_symbol("real-this-command")
            .expect("real-this-command"),
        Value::symbol("outer-real")
    );
}

#[test]
fn test_call_last_kbd_macro_preserves_gnu_real_this_command_shape() {
    let mut eval = Context::new();
    let setup = parse_forms(
        r#"(progn
             (setq kmacro-call-last-term-ok nil)
             (fset 'command-execute (lambda (cmd &optional _record _keys _special) (funcall cmd)))
             (let ((g (make-sparse-keymap)))
               (use-global-map g)
               (define-key g "a" (lambda () (interactive) 'ok)))
             (start-kbd-macro nil nil)
             (store-kbd-macro-event ?a)
             (end-kbd-macro)
             (setq real-this-command 'outer-real)
             (fset 'kmacro-call-last-term-hook
                   (lambda ()
                     (setq kmacro-call-last-term-ok
                           (and (null executing-kbd-macro)
                                (= executing-kbd-macro-index 0)
                                (equal real-this-command last-kbd-macro)))))
             (setq kbd-macro-termination-hook '(kmacro-call-last-term-hook))
             (call-last-kbd-macro))"#,
    )
    .expect("parse setup");
    let _ = eval.eval_forms(&setup);

    assert_eq!(
        eval.eval_symbol("kmacro-call-last-term-ok")
            .expect("kmacro-call-last-term-ok"),
        Value::T
    );
    assert_eq!(
        eval.eval_symbol("real-this-command")
            .expect("real-this-command"),
        eval.eval_symbol("last-kbd-macro").expect("last-kbd-macro")
    );
}

#[test]
fn test_last_kbd_macro_builtin() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    assert_eq!(
        builtin_last_kbd_macro(&mut eval, vec![]).unwrap(),
        Value::NIL
    );

    eval.command_loop.keyboard.kboard.last_kbd_macro =
        Some(vec![Value::char('x'), Value::char('y')]);
    let value = builtin_last_kbd_macro(&mut eval, vec![]).unwrap();
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = value.as_vector_data().unwrap().clone();
            assert_eq!(*items, vec![Value::char('x'), Value::char('y')]);
        }
        other => panic!("expected vector, got {other:?}"),
    }

    assert!(builtin_last_kbd_macro(&mut eval, vec![Value::NIL]).is_err());
}

#[test]
fn test_kmacro_p_builtin_subset() {
    assert_eq!(builtin_kmacro_p(vec![Value::NIL]).unwrap(), Value::NIL);
    assert_eq!(
        builtin_kmacro_p(vec![Value::vector(vec![])]).unwrap(),
        Value::T
    );
    assert_eq!(
        builtin_kmacro_p(vec![Value::string("abc")]).unwrap(),
        Value::T
    );
    assert_eq!(
        builtin_kmacro_p(vec![Value::fixnum(1)]).unwrap(),
        Value::NIL
    );
    assert!(builtin_kmacro_p(vec![]).is_err());
    assert!(builtin_kmacro_p(vec![Value::NIL, Value::NIL]).is_err());
}

#[test]
fn test_kmacro_set_counter_builtin() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    assert_eq!(
        builtin_kmacro_set_counter(&mut eval, vec![Value::fixnum(42)]).unwrap(),
        Value::NIL
    );
    assert_eq!(eval.kmacro.counter, 42);

    assert_eq!(
        builtin_kmacro_set_counter(&mut eval, vec![Value::fixnum(-3), Value::NIL]).unwrap(),
        Value::NIL
    );
    assert_eq!(eval.kmacro.counter, -3);

    assert!(builtin_kmacro_set_counter(&mut eval, vec![]).is_err());
    assert!(builtin_kmacro_set_counter(&mut eval, vec![Value::NIL]).is_err());
    assert!(
        builtin_kmacro_set_counter(&mut eval, vec![Value::fixnum(1), Value::NIL, Value::NIL])
            .is_err()
    );
}

#[test]
fn test_kmacro_add_counter_builtin() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    eval.kmacro.counter = 10;
    assert_eq!(
        builtin_kmacro_add_counter(&mut eval, vec![Value::fixnum(5)]).unwrap(),
        Value::NIL
    );
    assert_eq!(eval.kmacro.counter, 15);

    assert_eq!(
        builtin_kmacro_add_counter(&mut eval, vec![Value::fixnum(-2)]).unwrap(),
        Value::NIL
    );
    assert_eq!(eval.kmacro.counter, 13);

    assert!(builtin_kmacro_add_counter(&mut eval, vec![]).is_err());
    assert!(builtin_kmacro_add_counter(&mut eval, vec![Value::NIL]).is_err());
    assert!(builtin_kmacro_add_counter(&mut eval, vec![Value::fixnum(1), Value::NIL]).is_err());
}

#[test]
fn test_kmacro_set_format_builtin() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    assert_eq!(eval.kmacro.counter_format, "%d");

    assert_eq!(
        builtin_kmacro_set_format(&mut eval, vec![Value::string("item-%d")]).unwrap(),
        Value::NIL
    );
    assert_eq!(eval.kmacro.counter_format, "item-%d");

    assert_eq!(
        builtin_kmacro_set_format(&mut eval, vec![Value::string("")]).unwrap(),
        Value::NIL
    );
    assert_eq!(eval.kmacro.counter_format, "%d");

    assert!(builtin_kmacro_set_format(&mut eval, vec![]).is_err());
    assert!(builtin_kmacro_set_format(&mut eval, vec![Value::NIL]).is_err());
    assert!(builtin_kmacro_set_format(&mut eval, vec![Value::string("%d"), Value::NIL]).is_err());
}

#[test]
fn test_kmacro_builtin_arity_contracts() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    assert_eq!(
        builtin_start_kbd_macro(&mut eval, vec![Value::NIL, Value::NIL]).unwrap(),
        Value::NIL
    );
    assert!(builtin_start_kbd_macro(&mut eval, vec![Value::NIL, Value::NIL]).is_err());
    assert_eq!(
        builtin_end_kbd_macro(&mut eval, vec![]).unwrap(),
        Value::NIL
    );
    assert!(builtin_start_kbd_macro(&mut eval, vec![Value::NIL, Value::NIL, Value::NIL]).is_err());
    assert!(builtin_end_kbd_macro(&mut eval, vec![Value::NIL, Value::NIL, Value::NIL]).is_err());
    assert!(
        builtin_call_last_kbd_macro(&mut eval, vec![Value::NIL, Value::NIL, Value::NIL]).is_err()
    );
    assert!(builtin_execute_kbd_macro(&mut eval, vec![]).is_err());
    assert!(
        builtin_execute_kbd_macro(
            &mut eval,
            vec![Value::NIL, Value::NIL, Value::NIL, Value::NIL]
        )
        .is_err()
    );
}

#[test]
fn test_name_last_kbd_macro() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // No macro -- should error
    let result = builtin_name_last_kbd_macro(&mut eval, vec![Value::symbol("my-macro")]);
    assert!(result.is_err());

    // Record a macro
    builtin_start_kbd_macro(&mut eval, vec![]).expect("start");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::symbol(intern("forward-char"))])
        .expect("store");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end");

    // Name it
    let result = builtin_name_last_kbd_macro(&mut eval, vec![Value::symbol("my-macro")]);
    assert!(result.is_ok());

    // Check that the symbol has a function binding
    let func = eval.obarray.symbol_function("my-macro");
    assert!(func.is_some());
    match func.unwrap().kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = func.unwrap().as_vector_data().unwrap().clone();
            assert_eq!(items.len(), 1);
        }
        other => panic!("Expected Vector, got {:?}", func.unwrap()),
    }
}

#[test]
fn test_name_last_kbd_macro_wrong_type() {
    use super::super::eval::Context;
    use crate::emacs_core::value::{ValueKind, VecLikeType};

    let mut eval = Context::new();

    let result = builtin_name_last_kbd_macro(&mut eval, vec![Value::fixnum(42)]);
    assert!(result.is_err());
}

#[test]
fn test_kbd_macro_query_startup_is_autoloaded() {
    let eval = eval_with_ldefs_boot_autoloads(&["kbd-macro-query"]);
    let function = eval
        .obarray
        .symbol_function("kbd-macro-query")
        .expect("missing kbd-macro-query startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn test_kbd_macro_query_loads_from_gnu_macros_el() {
    let result = bootstrap_eval_all(
        r#"(list (condition-case err
                     (kbd-macro-query nil)
                   (error (list 'err (car err) (car (cdr err)))))
                 (subrp (symbol-function 'kbd-macro-query)))"#,
    );
    assert_eq!(
        result[0],
        r#"OK ((err user-error "Not defining or executing kbd macro") nil)"#
    );
}

#[test]
fn test_kbd_macro_query_loaded_arity_matches_gnu() {
    let result = bootstrap_eval_all(
        r#"(condition-case err
               (kbd-macro-query)
             (error (list 'err (car err))))"#,
    );
    assert_eq!(result[0], r#"OK (err wrong-number-of-arguments)"#);
}

#[test]
fn test_resolve_macro_events_vector() {
    let eval = Context::new();
    let v = Value::vector(vec![Value::char('a'), Value::char('b')]);
    let events = resolve_macro_events(&eval, &v).unwrap();
    assert_eq!(events.len(), 2);
}

#[test]
fn test_resolve_macro_events_string() {
    let eval = Context::new();
    let s = Value::string("hello");
    let events = resolve_macro_events(&eval, &s).unwrap();
    assert_eq!(events.len(), 5);
    match events[0].kind() {
        ValueKind::Char('h') => {}
        other => panic!("Expected Char('h'), got {:?}", events[0]),
    }
}

#[test]
fn test_resolve_macro_events_symbol_function_chain() {
    let mut eval = Context::new();
    eval.obarray_mut().set_symbol_function(
        "kmacro-target",
        Value::vector(vec![Value::char('x'), Value::char('y')]),
    );
    eval.obarray_mut()
        .set_symbol_function("kmacro-alias", Value::symbol("kmacro-target"));

    let events = resolve_macro_events(&eval, &Value::symbol("kmacro-alias")).unwrap();
    assert_eq!(events.len(), 2);
}

#[test]
fn test_resolve_macro_events_list_errors_like_gnu() {
    let eval = Context::new();
    let list = Value::list(vec![Value::char('x'), Value::char('y')]);
    let result = resolve_macro_events(&eval, &list);
    let Err(Flow::Signal(sig)) = result else {
        panic!("expected signal for list macro");
    };
    assert_eq!(sig.symbol_name(), "error");
    assert_eq!(
        sig.data,
        vec![Value::string("Keyboard macros must be strings or vectors")]
    );
}

#[test]
fn test_resolve_macro_events_wrong_type() {
    let eval = Context::new();
    let result = resolve_macro_events(&eval, &Value::fixnum(42));
    let Err(Flow::Signal(sig)) = result else {
        panic!("expected signal for non-macro value");
    };
    assert_eq!(sig.symbol_name(), "error");
    assert_eq!(
        sig.data,
        vec![Value::string("Keyboard macros must be strings or vectors")]
    );
}

#[test]
fn test_insert_kbd_macro_startup_is_autoloaded() {
    let eval = eval_with_ldefs_boot_autoloads(&["insert-kbd-macro"]);
    let function = eval
        .obarray
        .symbol_function("insert-kbd-macro")
        .expect("missing insert-kbd-macro startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn test_insert_kbd_macro_loads_from_gnu_macros_el() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (fset 'test-macro [97 98])
             (insert-kbd-macro 'test-macro)
             (list (and (string-match-p "defalias" (buffer-string)) t)
                   (and (string-match-p "test-macro" (buffer-string)) t)
                   (subrp (symbol-function 'insert-kbd-macro))))"#,
    );
    assert_eq!(result[0], r#"OK (t t nil)"#);
}

#[test]
fn test_insert_kbd_macro_loaded_arity_matches_gnu() {
    let result = bootstrap_eval_all(
        r#"(condition-case err
               (insert-kbd-macro)
             (error (list 'err (car err))))"#,
    );
    assert_eq!(result[0], r#"OK (err wrong-number-of-arguments)"#);
}
