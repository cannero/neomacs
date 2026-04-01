use super::super::value::next_float_id;
use super::*;
use crate::emacs_core::eval::Context;
use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn eval_first_form_after_marker(eval: &mut Context, source: &str, marker: &str) {
    let start = source
        .find(marker)
        .unwrap_or_else(|| panic!("missing GNU subr.el marker: {marker}"));
    let forms = super::super::parser::parse_forms(&source[start..])
        .unwrap_or_else(|err| panic!("parse GNU subr.el from {marker} failed: {:?}", err));
    let form = forms
        .first()
        .unwrap_or_else(|| panic!("no GNU subr.el form found after marker: {marker}"));
    eval.eval_expr(form)
        .unwrap_or_else(|err| panic!("evaluate GNU subr.el form {marker} failed: {:?}", err));
}

/// Install minimal `defun`/`defmacro`/`when`/`unless` shims so a bare
/// evaluator can evaluate forms extracted from GNU `.el` source files.
fn install_bare_elisp_shims(ev: &mut Context) {
    let shims = r#"
(defalias 'defun (cons 'macro #'(lambda (name arglist &rest body)
  (list 'defalias (list 'quote name) (cons 'function (list (cons 'lambda (cons arglist body))))))))
(defalias 'defmacro (cons 'macro #'(lambda (name arglist &rest body)
  (list 'defalias (list 'quote name)
        (list 'cons ''macro (cons 'function (list (cons 'lambda (cons arglist body)))))))))
(defalias 'when (cons 'macro #'(lambda (cond &rest body)
  (list 'if cond (cons 'progn body)))))
(defalias 'unless (cons 'macro #'(lambda (cond &rest body)
  (cons 'if (cons cond (cons nil body))))))
"#;
    let forms = super::super::parser::parse_forms(shims).expect("parse bare elisp shims");
    for form in &forms {
        ev.eval_expr(form).expect("install bare elisp shim");
    }
}

fn gnu_subr_sit_for_eval() -> Context {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let subr_path = project_root.join("lisp/subr.el");
    let subr_source = fs::read_to_string(&subr_path).expect("read GNU subr.el");

    let mut ev = Context::new();
    install_bare_elisp_shims(&mut ev);
    ev.set_lexical_binding(true);
    eval_first_form_after_marker(
        &mut ev,
        &subr_source,
        "(defun sit-for (seconds &optional nodisp)",
    );
    ev
}

fn install_minimal_special_event_command_runtime(ev: &mut Context) {
    let setup = super::super::parser::parse_forms(
        r#"
(fset 'command-execute
      (lambda (cmd &optional _record keys _special)
        (funcall cmd (aref keys 0))))
(fset 'handle-delete-frame
      (lambda (event)
        (setq neo-last-delete-frame-event event)
        nil))
"#,
    )
    .expect("parse special-event command runtime");
    for form in &setup {
        ev.eval_expr(form)
            .expect("install special-event command runtime");
    }
}

fn gnu_timer_before(delay: Duration, callback: &str) -> Value {
    let when = SystemTime::now()
        .checked_sub(delay)
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .expect("timer deadline should not precede unix epoch");
    let secs = when.as_secs() as i64;

    Value::vector(vec![
        Value::NIL,
        Value::fixnum(secs >> 16),
        Value::fixnum(secs & 0xFFFF),
        Value::fixnum(when.subsec_micros() as i64),
        Value::NIL,
        Value::symbol(callback),
        Value::NIL,
        Value::NIL,
        Value::fixnum(0),
        Value::NIL,
    ])
}

#[test]
fn timer_creation_and_list() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    let id1 = mgr.add_timer(1.0, 0.0, Value::symbol("my-callback"), vec![], false);
    let id2 = mgr.add_timer(
        2.0,
        0.0,
        Value::symbol("other-callback"),
        vec![Value::fixnum(42)],
        false,
    );

    assert_ne!(id1, id2);
    assert!(mgr.is_timer(id1));
    assert!(mgr.is_timer(id2));
    assert!(!mgr.is_timer(999));

    let all = mgr.list_timers();
    assert_eq!(all.len(), 2);
    assert!(all.contains(&id1));
    assert!(all.contains(&id2));
}

#[test]
fn timer_cancellation() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    let id = mgr.add_timer(1.0, 0.0, Value::symbol("cb"), vec![], false);

    assert!(mgr.timer_active_p(id));
    assert!(mgr.cancel_timer(id));
    assert!(!mgr.timer_active_p(id));

    // Cancelling again still returns true (timer exists, just already inactive)
    assert!(mgr.cancel_timer(id));

    // Cancelling non-existent timer returns false
    assert!(!mgr.cancel_timer(999));
}

#[test]
fn fire_pending_timers_one_shot() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    // Create a timer with 0 delay (fires immediately)
    let id = mgr.add_timer(
        0.0,
        0.0,
        Value::symbol("immediate"),
        vec![Value::fixnum(1)],
        false,
    );

    // Fire it
    let now = Instant::now();
    let fired = mgr.fire_pending_timers(now, None);

    assert_eq!(fired.len(), 1);
    // Check callback is the symbol we set
    match fired[0].0.kind() {
        ValueKind::Symbol(id) => {
            assert_eq!(crate::emacs_core::intern::resolve_sym(id), "immediate")
        }
        other => panic!("Expected Symbol, got {:?}", fired[0].0),
    }
    assert_eq!(fired[0].1.len(), 1);

    // Timer should be inactive after one-shot fire
    assert!(!mgr.timer_active_p(id));

    // Fire again: nothing should fire
    let fired2 = mgr.fire_pending_timers(Instant::now(), None);
    assert!(fired2.is_empty());
}

#[test]
fn fire_pending_timers_repeat() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    // Create a repeating timer with 0 delay and 1-second repeat
    let id = mgr.add_timer(0.0, 1.0, Value::symbol("repeater"), vec![], false);

    // Fire it once
    let now = Instant::now();
    let fired = mgr.fire_pending_timers(now, None);
    assert_eq!(fired.len(), 1);

    // Timer should still be active (it repeats)
    assert!(mgr.timer_active_p(id));

    // Immediately firing again should NOT fire (needs 1 second)
    let fired2 = mgr.fire_pending_timers(Instant::now(), None);
    assert!(fired2.is_empty());

    // Advance time by simulating future instant
    let future = Instant::now() + Duration::from_secs(2);
    let fired3 = mgr.fire_pending_timers(future, None);
    assert_eq!(fired3.len(), 1);
    assert!(mgr.timer_active_p(id));
}

#[test]
fn timer_not_yet_due() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    // Timer fires in 10 seconds
    let id = mgr.add_timer(10.0, 0.0, Value::symbol("future"), vec![], false);

    let fired = mgr.fire_pending_timers(Instant::now(), None);
    assert!(fired.is_empty());
    assert!(mgr.timer_active_p(id));
}

#[test]
fn next_fire_time_works() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();

    // No timers => None
    assert!(mgr.next_fire_time(None).is_none());

    // Add a timer in the future
    let _id = mgr.add_timer(5.0, 0.0, Value::symbol("cb"), vec![], false);
    let next = mgr.next_fire_time(None);
    assert!(next.is_some());
    // Should be roughly 5 seconds (with some tolerance for test execution time)
    let dur = next.unwrap();
    assert!(dur.as_secs_f64() > 4.0);
    assert!(dur.as_secs_f64() < 6.0);
}

#[test]
fn next_fire_time_overdue() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    // Timer with 0 delay => immediately overdue
    let _id = mgr.add_timer(0.0, 0.0, Value::symbol("cb"), vec![], false);
    let next = mgr.next_fire_time(None);
    assert!(next.is_some());
    assert!(next.unwrap() <= Duration::from_millis(10));
}

#[test]
fn idle_timer_flag() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    let id = mgr.add_timer(1.0, 0.0, Value::symbol("idle-cb"), vec![], true);

    // The timer is stored with idle=true
    let timer = mgr.timers.iter().find(|t| t.id == id).unwrap();
    assert!(timer.idle);
}

#[test]
fn timer_set_time_reschedules() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    let id = mgr.add_timer(100.0, 0.0, Value::symbol("cb"), vec![], false);

    // Originally 100 seconds away — won't fire now
    let fired = mgr.fire_pending_timers(Instant::now(), None);
    assert!(fired.is_empty());

    // Reschedule to 0 seconds
    mgr.timer_set_time(id, 0.0);
    let fired = mgr.fire_pending_timers(Instant::now(), None);
    assert_eq!(fired.len(), 1);
}

#[test]
fn timer_activate_reactivates() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    let id = mgr.add_timer(0.0, 0.0, Value::symbol("cb"), vec![], false);

    // Fire and deactivate
    mgr.fire_pending_timers(Instant::now(), None);
    assert!(!mgr.timer_active_p(id));

    // Reactivate
    assert!(mgr.timer_activate(id));
    assert!(mgr.timer_active_p(id));

    // Fire again
    let fired = mgr.fire_pending_timers(Instant::now(), None);
    assert_eq!(fired.len(), 1);
}

#[test]
fn timer_activate_nonexistent() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    assert!(!mgr.timer_activate(999));
}

#[test]
fn list_active_timers() {
    crate::test_utils::init_test_tracing();
    let mut mgr = TimerManager::new();
    let id1 = mgr.add_timer(1.0, 0.0, Value::symbol("a"), vec![], false);
    let id2 = mgr.add_timer(2.0, 0.0, Value::symbol("b"), vec![], false);

    let active = mgr.list_active_timers();
    assert_eq!(active.len(), 2);

    mgr.cancel_timer(id1);
    let active = mgr.list_active_timers();
    assert_eq!(active.len(), 1);
    assert!(active.contains(&id2));
}

// -----------------------------------------------------------------------
// Builtin-level tests (via Context)
// -----------------------------------------------------------------------

#[test]
fn test_builtin_timerp() {
    crate::test_utils::init_test_tracing();
    // Timer value
    let result = builtin_timerp(vec![Value::make_timer(1)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());

    // Non-timer value
    let result = builtin_timerp(vec![Value::fixnum(42)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());

    // Nil
    let result = builtin_timerp(vec![Value::NIL]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn gnu_sit_for_matches_subr_el() {
    crate::test_utils::init_test_tracing();
    let mut ev = gnu_subr_sit_for_eval();
    let forms = super::super::parser::parse_forms(
        r#"
        (let ((noninteractive t))
          (sit-for 0.0))
        (let ((noninteractive t))
          (sit-for 0.01 t))
        "#,
    )
    .expect("parse sit-for forms");

    let first = ev.eval(&forms[0]).expect("eval sit-for");
    assert!(first.is_truthy());

    let second = ev.eval(&forms[1]).expect("eval sit-for nodisp");
    assert!(second.is_truthy());
}

#[test]
fn gnu_sit_for_interactive_timeout_returns_t() {
    crate::test_utils::init_test_tracing();
    let mut ev = gnu_subr_sit_for_eval();
    ev.set_variable("noninteractive", Value::NIL);
    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    let forms = super::super::parser::parse_forms("(sit-for 0.01 t)").expect("parse sit-for");

    let start = Instant::now();
    let result = ev.eval(&forms[0]).expect("eval interactive sit-for");
    drop(tx);

    assert!(result.is_truthy());
    assert!(start.elapsed() < Duration::from_millis(250));
}

#[test]
fn gnu_sit_for_with_pending_input_does_not_run_timers_first() {
    crate::test_utils::init_test_tracing();
    let mut ev = gnu_subr_sit_for_eval();
    ev.set_variable("noninteractive", Value::NIL);
    let setup = super::super::parser::parse_forms(
        r#"(progn
             (setq sit-for-pending-input-timer-fired nil)
             (fset 'sit-for-pending-input-timer-callback
                   (lambda ()
                     (setq sit-for-pending-input-timer-fired 'done))))"#,
    )
    .expect("parse sit-for pending-input timer setup");
    ev.eval_expr(&setup[0])
        .expect("install sit-for pending-input timer setup");
    ev.timers.add_timer(
        0.0,
        0.0,
        Value::symbol("sit-for-pending-input-timer-callback"),
        vec![],
        false,
    );

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue keypress");
    ev.input_rx = Some(rx);
    let forms = super::super::parser::parse_forms("(sit-for 0.5 t)").expect("parse sit-for");

    let result = ev.eval(&forms[0]).expect("eval interactive sit-for");

    assert!(result.is_nil());
    assert!(
        ev.eval_symbol("sit-for-pending-input-timer-fired")
            .expect("timer callback flag")
            .is_nil()
    );
    let event = ev.read_char().expect("keypress should remain available");
    assert_eq!(event, Value::fixnum('a' as i64));
}

#[test]
fn gnu_sit_for_pending_input_returns_nil_without_redisplay() {
    crate::test_utils::init_test_tracing();
    let mut ev = gnu_subr_sit_for_eval();
    ev.set_variable("noninteractive", Value::NIL);
    let redisplays = Rc::new(RefCell::new(0usize));
    let redisplays_in_cb = Rc::clone(&redisplays);
    ev.redisplay_fn = Some(Box::new(move |_ev: &mut Context| {
        *redisplays_in_cb.borrow_mut() += 1;
    }));

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue keypress");
    ev.input_rx = Some(rx);
    let forms = super::super::parser::parse_forms("(sit-for 0.5)").expect("parse sit-for");

    let result = ev.eval(&forms[0]).expect("eval interactive sit-for");

    assert!(result.is_nil());
    assert_eq!(*redisplays.borrow(), 0);
    let event = ev.read_char().expect("keypress should remain available");
    assert_eq!(event, Value::fixnum('a' as i64));
}

#[test]
fn gnu_sit_for_zero_without_nodisp_redisplays_once() {
    crate::test_utils::init_test_tracing();
    let mut ev = gnu_subr_sit_for_eval();
    ev.set_variable("noninteractive", Value::NIL);
    let redisplays = Rc::new(RefCell::new(0usize));
    let redisplays_in_cb = Rc::clone(&redisplays);
    ev.redisplay_fn = Some(Box::new(move |_ev: &mut Context| {
        *redisplays_in_cb.borrow_mut() += 1;
    }));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    let forms = super::super::parser::parse_forms("(sit-for 0)").expect("parse sit-for");

    let result = ev.eval(&forms[0]).expect("eval zero-second sit-for");
    drop(tx);

    assert!(result.is_truthy());
    assert_eq!(*redisplays.borrow(), 1);
}

#[test]
fn gnu_sit_for_zero_nodisp_runs_due_gnu_timer_without_redisplay() {
    crate::test_utils::init_test_tracing();
    let mut ev = gnu_subr_sit_for_eval();
    ev.set_variable("noninteractive", Value::NIL);
    let setup = super::super::parser::parse_forms(
        r#"(progn
             (setq sit-for-zero-timer-fired nil)
             (fset 'sit-for-zero-timer-callback
                   (lambda ()
                     (setq sit-for-zero-timer-fired 'done)))
             (fset 'timer-event-handler
                   (lambda (timer)
                     (setq timer-list (delq timer timer-list))
                     (funcall (aref timer 5)))))"#,
    )
    .expect("parse zero-second sit-for timer setup");
    ev.eval_expr(&setup[0])
        .expect("install zero-second sit-for timer setup");
    ev.set_variable(
        "timer-list",
        Value::list(vec![gnu_timer_before(
            Duration::from_millis(1),
            "sit-for-zero-timer-callback",
        )]),
    );

    let redisplays = Rc::new(RefCell::new(0usize));
    let redisplays_in_cb = Rc::clone(&redisplays);
    ev.redisplay_fn = Some(Box::new(move |_ev: &mut Context| {
        *redisplays_in_cb.borrow_mut() += 1;
    }));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    let forms = super::super::parser::parse_forms("(sit-for 0 t)").expect("parse sit-for");

    let result = ev.eval(&forms[0]).expect("eval zero-second sit-for");
    drop(tx);

    assert!(result.is_truthy());
    assert_eq!(*redisplays.borrow(), 0);
    assert_eq!(
        ev.eval_symbol("sit-for-zero-timer-fired")
            .expect("zero-second sit-for timer flag"),
        Value::symbol("done")
    );
}

#[test]
fn test_builtin_sleep_for() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();

    let result = builtin_sleep_for(&mut eval, vec![Value::fixnum(0)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());

    let result = builtin_sleep_for(&mut eval, vec![Value::fixnum(0), Value::fixnum(0)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());

    let result = builtin_sleep_for(&mut eval, vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let result = builtin_sleep_for(
        &mut eval,
        vec![Value::fixnum(0), Value::fixnum(0), Value::fixnum(0)],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let result = builtin_sleep_for(&mut eval, vec![Value::string("1")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("numberp"), Value::string("1")]
    ));

    let result = builtin_sleep_for(&mut eval, vec![Value::fixnum(0), Value::make_float(0.5)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("fixnump"), Value::make_float(0.5)]
    ));
}

#[test]
fn sleep_for_window_close_uses_special_event_map_handler_when_loaded() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let scratch = ev.buffers.create_buffer("*scratch*");
    ev.buffers.set_current(scratch);
    let frame = ev.frames.create_frame("F1", 80, 24, scratch);
    install_minimal_special_event_command_runtime(&mut ev);

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::WindowClose {
        emacs_frame_id: frame.0,
    })
    .expect("queue window close");
    ev.input_rx = Some(rx);
    ev.command_loop.running = true;

    let result = builtin_sleep_for(&mut ev, vec![Value::make_float(0.01)])
        .expect("sleep-for should consume handled window close");
    drop(tx);

    assert_eq!(result, Value::NIL);
    let logged = ev
        .eval_symbol("neo-last-delete-frame-event")
        .expect("delete-frame event should be logged");
    assert_eq!(
        logged,
        Value::list(vec![
            Value::symbol("delete-frame"),
            Value::list(vec![Value::make_frame(frame.0)]),
        ]),
    );
}

#[test]
fn sleep_for_window_close_honors_throw_on_input_before_handler() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let scratch = ev.buffers.create_buffer("*scratch*");
    ev.buffers.set_current(scratch);
    let frame = ev.frames.create_frame("F1", 80, 24, scratch);
    install_minimal_special_event_command_runtime(&mut ev);

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::WindowClose {
        emacs_frame_id: frame.0,
    })
    .expect("queue window close");
    ev.input_rx = Some(rx);
    ev.command_loop.running = true;
    ev.obarray
        .set_symbol_value("throw-on-input", Value::symbol("tag"));

    let flow = builtin_sleep_for(&mut ev, vec![Value::make_float(0.01)])
        .expect_err("throw-on-input should interrupt sleep-for");
    assert!(matches!(
        flow,
        Flow::Throw { tag, value } if tag == Value::symbol("tag") && value == Value::T
    ));

    ev.obarray.set_symbol_value("throw-on-input", Value::NIL);
    let result = builtin_sleep_for(&mut ev, vec![Value::make_float(0.01)])
        .expect("sleep-for should consume handled window close afterwards");
    drop(tx);

    assert_eq!(result, Value::NIL);
    let logged = ev
        .eval_symbol("neo-last-delete-frame-event")
        .expect("delete-frame event should be logged");
    assert_eq!(
        logged,
        Value::list(vec![
            Value::symbol("delete-frame"),
            Value::list(vec![Value::make_frame(frame.0)]),
        ]),
    );
}

#[test]
fn test_eval_run_at_time_and_cancel() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();

    // run-at-time with 0 delay
    let result = builtin_run_at_time(
        &mut eval,
        vec![
            Value::make_float(0.0),
            Value::NIL,
            Value::symbol("my-func"),
            Value::fixnum(1),
            Value::fixnum(2),
        ],
    );
    assert!(result.is_ok());
    let timer_val = result.unwrap();
    assert!(timer_val.is_timer());

    // cancel-timer
    let result = builtin_cancel_timer(&mut eval, vec![timer_val]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());

    // Verify it's cancelled
    if let Some(timer_id) = timer_val.as_timer_id() {
        assert!(!eval.timers.timer_active_p(timer_id));
    }
}

#[test]
fn test_eval_run_with_idle_timer() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();

    let result = builtin_run_with_idle_timer(
        &mut eval,
        vec![Value::fixnum(5), Value::NIL, Value::symbol("idle-func")],
    );
    assert!(result.is_ok());
    let timer_val = result.unwrap();

    // Should be a timer
    assert!(timer_val.is_timer());

    // The timer should be idle
    if let Some(timer_id) = timer_val.as_timer_id() {
        let timer = eval
            .timers
            .timers
            .iter()
            .find(|t| t.id == timer_id)
            .unwrap();
        assert!(timer.idle);
    }
}

#[test]
fn test_eval_run_at_time_accepts_nil_and_string_specs() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();

    let from_nil = builtin_run_at_time(
        &mut eval,
        vec![Value::NIL, Value::NIL, Value::symbol("cb-from-nil")],
    )
    .expect("nil time spec should be accepted");
    assert!(from_nil.is_timer());

    let from_string = builtin_run_at_time(
        &mut eval,
        vec![
            Value::string("0 sec"),
            Value::NIL,
            Value::symbol("cb-from-string"),
        ],
    )
    .expect("string time spec should be accepted");
    assert!(from_string.is_timer());
}

#[test]
fn test_parse_run_at_time_delay_units() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        parse_run_at_time_delay(&Value::string("2 min")).expect("2 min should parse"),
        120.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1.5 hour")).expect("1.5 hour should parse"),
        5400.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("3 day")).expect("3 day should parse"),
        259_200.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 week")).expect("1 week should parse"),
        604_800.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("4 fortnights")).expect("4 fortnights should parse"),
        4_838_400.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("4fortnight")).expect("4fortnight should parse"),
        4_838_400.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("2.5day")).expect("2.5day should parse"),
        216_000.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("2.5 day")).expect("2.5 day should parse"),
        216_000.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("+2day")).expect("+2day should parse"),
        172_800.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("+2 day")).expect("+2 day should parse"),
        172_800.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("+ 2 day")).expect("+ 2 day should parse"),
        172_800.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("- 2 day")).expect("- 2 day should parse"),
        -172_800.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("+ .5day")).expect("+ .5day should parse"),
        43_200.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("- .5 day")).expect("- .5 day should parse"),
        -43_200.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("+ .5 day")).expect("+ .5 day should parse"),
        43_200.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 2 min")).expect("1 2 min should parse"),
        720.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("+ 1 5 sec")).expect("+ 1 5 sec should parse"),
        15.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 +2 sec")).expect("1 +2 sec should parse"),
        2.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 +2e3 sec")).expect("1 +2e3 sec should parse"),
        2_000.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 -2 sec")).expect("1 -2 sec should parse"),
        -2.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 -2e2 sec")).expect("1 -2e2 sec should parse"),
        -200.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 - +2 sec")).expect("1 - +2 sec should parse"),
        2.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 + 2 sec")).expect("1 + 2 sec should parse"),
        2.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 - 2 sec")).expect("1 - 2 sec should parse"),
        2.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 + 2e 3 sec")).expect("1 + 2e 3 sec should parse"),
        3.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 2 3 min")).expect("1 2 3 min should parse"),
        7_380.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 2 e3 sec")).expect("1 2 e3 sec should parse"),
        12_000.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1e3sec")).expect("1e3sec should parse"),
        1_000.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1e3 sec")).expect("1e3 sec should parse"),
        1_000.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("2e1 min")).expect("2e1 min should parse"),
        1_200.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("2e1min")).expect("2e1min should parse"),
        1_200.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 .5sec")).expect("1 .5sec should parse"),
        1.5
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1 .5 sec")).expect("1 .5 sec should parse"),
        1.5
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1e-1 sec")).expect("1e-1 sec should parse"),
        0.1
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string(".5e2sec")).expect(".5e2sec should parse"),
        50.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1e+1 sec")).expect("1e+1 sec should parse"),
        10.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("1e+ 1 sec")).expect("1e+ 1 sec should parse"),
        10.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string(" \t+ 2 day \t"))
            .expect("whitespace + 2 day should parse"),
        172_800.0
    );
    assert_eq!(
        parse_run_at_time_delay(&Value::string("\t+ .5day\n"))
            .expect("whitespace + .5day should parse"),
        43_200.0
    );
    assert!(parse_run_at_time_delay(&Value::string("4 foo")).is_err());
    assert!(parse_run_at_time_delay(&Value::string("2 s")).is_err());
    assert!(parse_run_at_time_delay(&Value::string("2h")).is_err());
    assert!(parse_run_at_time_delay(&Value::string("2 hr")).is_err());
    assert!(parse_run_at_time_delay(&Value::string("+")).is_err());
    assert!(parse_run_at_time_delay(&Value::string("-")).is_err());
    assert!(parse_run_at_time_delay(&Value::string("+ 2")).is_err());
    assert!(parse_run_at_time_delay(&Value::string("- 2")).is_err());
    assert!(parse_run_at_time_delay(&Value::string("1 + foo sec")).is_err());
    assert!(parse_run_at_time_delay(&Value::string("1e+ sec")).is_err());
    assert!(parse_run_at_time_delay(&Value::string("+ 1 5")).is_err());
}

#[test]
fn test_eval_run_at_time_invalid_spec_signals_error() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();

    let invalid_string = builtin_run_at_time(
        &mut eval,
        vec![Value::string("abc"), Value::NIL, Value::symbol("cb")],
    );
    assert!(matches!(
        invalid_string,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "error"
    ));

    let invalid_type =
        builtin_run_at_time(&mut eval, vec![Value::T, Value::NIL, Value::symbol("cb")]);
    assert!(matches!(
        invalid_type,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "error"
    ));
}

#[test]
fn test_eval_run_with_idle_timer_nil_ok_string_error() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();

    let from_nil =
        builtin_run_with_idle_timer(&mut eval, vec![Value::NIL, Value::NIL, Value::symbol("cb")])
            .expect("nil idle delay should be accepted");
    assert!(from_nil.is_timer());

    let from_string = builtin_run_with_idle_timer(
        &mut eval,
        vec![Value::string("0 sec"), Value::NIL, Value::symbol("cb")],
    );
    assert!(matches!(
        from_string,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "error"
    ));
}

#[test]
fn test_eval_timer_activate() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();

    // Create and cancel a timer
    let result = builtin_run_at_time(
        &mut eval,
        vec![Value::make_float(1.0), Value::NIL, Value::symbol("cb")],
    );
    let timer_val = result.unwrap();
    builtin_cancel_timer(&mut eval, vec![timer_val]).unwrap();

    if let Some(timer_id) = timer_val.as_timer_id() {
        assert!(!eval.timers.timer_active_p(timer_id));
    }

    // Reactivate
    let result = builtin_timer_activate(&mut eval, vec![timer_val]);
    assert!(result.is_ok());

    if let Some(timer_id) = timer_val.as_timer_id() {
        assert!(eval.timers.timer_active_p(timer_id));
    }

    // Active timers cannot be activated again.
    let second = builtin_timer_activate(&mut eval, vec![timer_val]);
    assert!(matches!(second, Err(Flow::Signal(sig)) if sig.symbol_name() == "error"));

    // Cancel again and verify optional args are accepted.
    builtin_cancel_timer(&mut eval, vec![timer_val]).unwrap();
    let with_restart = builtin_timer_activate(&mut eval, vec![timer_val, Value::T]);
    assert!(with_restart.is_ok());

    builtin_cancel_timer(&mut eval, vec![timer_val]).unwrap();
    let with_restart_and_delta = builtin_timer_activate(
        &mut eval,
        vec![
            timer_val,
            Value::NIL,
            Value::cons(Value::fixnum(1), Value::fixnum(2)),
        ],
    );
    assert!(with_restart_and_delta.is_ok());
}

#[test]
fn test_eval_timer_activate_rejects_non_timer_with_error() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();
    let result = builtin_timer_activate(&mut eval, vec![Value::NIL]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "error"));
}

#[test]
fn test_eval_timer_activate_optional_delta_must_be_cons_or_nil() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    use crate::emacs_core::value::ValueKind;

    let mut eval = Context::new();
    let timer_val = builtin_run_at_time(
        &mut eval,
        vec![Value::make_float(1.0), Value::NIL, Value::symbol("cb")],
    )
    .unwrap();
    builtin_cancel_timer(&mut eval, vec![timer_val]).unwrap();

    let result = builtin_timer_activate(&mut eval, vec![timer_val, Value::NIL, Value::fixnum(2)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("consp"), Value::fixnum(2)]
    ));
}
