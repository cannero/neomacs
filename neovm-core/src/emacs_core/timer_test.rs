use super::super::value::next_float_id;
use super::*;
use std::time::{Duration, Instant};

#[test]
fn timer_creation_and_list() {
    let mut mgr = TimerManager::new();
    let id1 = mgr.add_timer(1.0, 0.0, Value::symbol("my-callback"), vec![], false);
    let id2 = mgr.add_timer(
        2.0,
        0.0,
        Value::symbol("other-callback"),
        vec![Value::Int(42)],
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
    let mut mgr = TimerManager::new();
    // Create a timer with 0 delay (fires immediately)
    let id = mgr.add_timer(
        0.0,
        0.0,
        Value::symbol("immediate"),
        vec![Value::Int(1)],
        false,
    );

    // Fire it
    let now = Instant::now();
    let fired = mgr.fire_pending_timers(now);

    assert_eq!(fired.len(), 1);
    // Check callback is the symbol we set
    match &fired[0].0 {
        Value::Symbol(id) => assert_eq!(crate::emacs_core::intern::resolve_sym(*id), "immediate"),
        other => panic!("Expected Symbol, got {:?}", other),
    }
    assert_eq!(fired[0].1.len(), 1);

    // Timer should be inactive after one-shot fire
    assert!(!mgr.timer_active_p(id));

    // Fire again: nothing should fire
    let fired2 = mgr.fire_pending_timers(Instant::now());
    assert!(fired2.is_empty());
}

#[test]
fn fire_pending_timers_repeat() {
    let mut mgr = TimerManager::new();
    // Create a repeating timer with 0 delay and 1-second repeat
    let id = mgr.add_timer(0.0, 1.0, Value::symbol("repeater"), vec![], false);

    // Fire it once
    let now = Instant::now();
    let fired = mgr.fire_pending_timers(now);
    assert_eq!(fired.len(), 1);

    // Timer should still be active (it repeats)
    assert!(mgr.timer_active_p(id));

    // Immediately firing again should NOT fire (needs 1 second)
    let fired2 = mgr.fire_pending_timers(Instant::now());
    assert!(fired2.is_empty());

    // Advance time by simulating future instant
    let future = Instant::now() + Duration::from_secs(2);
    let fired3 = mgr.fire_pending_timers(future);
    assert_eq!(fired3.len(), 1);
    assert!(mgr.timer_active_p(id));
}

#[test]
fn timer_not_yet_due() {
    let mut mgr = TimerManager::new();
    // Timer fires in 10 seconds
    let id = mgr.add_timer(10.0, 0.0, Value::symbol("future"), vec![], false);

    let fired = mgr.fire_pending_timers(Instant::now());
    assert!(fired.is_empty());
    assert!(mgr.timer_active_p(id));
}

#[test]
fn next_fire_time_works() {
    let mut mgr = TimerManager::new();

    // No timers => None
    assert!(mgr.next_fire_time().is_none());

    // Add a timer in the future
    let _id = mgr.add_timer(5.0, 0.0, Value::symbol("cb"), vec![], false);
    let next = mgr.next_fire_time();
    assert!(next.is_some());
    // Should be roughly 5 seconds (with some tolerance for test execution time)
    let dur = next.unwrap();
    assert!(dur.as_secs_f64() > 4.0);
    assert!(dur.as_secs_f64() < 6.0);
}

#[test]
fn next_fire_time_overdue() {
    let mut mgr = TimerManager::new();
    // Timer with 0 delay => immediately overdue
    let _id = mgr.add_timer(0.0, 0.0, Value::symbol("cb"), vec![], false);
    let next = mgr.next_fire_time();
    assert!(next.is_some());
    assert!(next.unwrap() <= Duration::from_millis(10));
}

#[test]
fn idle_timer_flag() {
    let mut mgr = TimerManager::new();
    let id = mgr.add_timer(1.0, 0.0, Value::symbol("idle-cb"), vec![], true);

    // The timer is stored with idle=true
    let timer = mgr.timers.iter().find(|t| t.id == id).unwrap();
    assert!(timer.idle);
}

#[test]
fn timer_set_time_reschedules() {
    let mut mgr = TimerManager::new();
    let id = mgr.add_timer(100.0, 0.0, Value::symbol("cb"), vec![], false);

    // Originally 100 seconds away — won't fire now
    let fired = mgr.fire_pending_timers(Instant::now());
    assert!(fired.is_empty());

    // Reschedule to 0 seconds
    mgr.timer_set_time(id, 0.0);
    let fired = mgr.fire_pending_timers(Instant::now());
    assert_eq!(fired.len(), 1);
}

#[test]
fn timer_activate_reactivates() {
    let mut mgr = TimerManager::new();
    let id = mgr.add_timer(0.0, 0.0, Value::symbol("cb"), vec![], false);

    // Fire and deactivate
    mgr.fire_pending_timers(Instant::now());
    assert!(!mgr.timer_active_p(id));

    // Reactivate
    assert!(mgr.timer_activate(id));
    assert!(mgr.timer_active_p(id));

    // Fire again
    let fired = mgr.fire_pending_timers(Instant::now());
    assert_eq!(fired.len(), 1);
}

#[test]
fn timer_activate_nonexistent() {
    let mut mgr = TimerManager::new();
    assert!(!mgr.timer_activate(999));
}

#[test]
fn list_active_timers() {
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
// Builtin-level tests (via Evaluator)
// -----------------------------------------------------------------------

#[test]
fn test_builtin_timerp() {
    // Timer value
    let result = builtin_timerp(vec![Value::Timer(1)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());

    // Non-timer value
    let result = builtin_timerp(vec![Value::Int(42)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());

    // Nil
    let result = builtin_timerp(vec![Value::Nil]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn test_builtin_sit_for() {
    use super::super::eval::Evaluator;

    let mut eval = Evaluator::new();

    let result = builtin_sit_for(&mut eval, vec![Value::Float(0.1, next_float_id())]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());

    // Wrong type
    let result = builtin_sit_for(&mut eval, vec![Value::string("bad")]);
    assert!(result.is_err());

    // Wrong arity
    let result = builtin_sit_for(&mut eval, vec![Value::Int(0), Value::Nil, Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn test_builtin_sleep_for() {
    let result = builtin_sleep_for(vec![Value::Int(0)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());

    let result = builtin_sleep_for(vec![Value::Int(0), Value::Int(0)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());

    let result = builtin_sleep_for(vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let result = builtin_sleep_for(vec![Value::Int(0), Value::Int(0), Value::Int(0)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let result = builtin_sleep_for(vec![Value::string("1")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("numberp"), Value::string("1")]
    ));

    let result = builtin_sleep_for(vec![Value::Int(0), Value::Float(0.5, next_float_id())]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("fixnump"), Value::Float(0.5, next_float_id())]
    ));
}

#[test]
fn test_eval_run_at_time_and_cancel() {
    use super::super::eval::Evaluator;

    let mut eval = Evaluator::new();

    // run-at-time with 0 delay
    let result = builtin_run_at_time(
        &mut eval,
        vec![
            Value::Float(0.0, next_float_id()),
            Value::Nil,
            Value::symbol("my-func"),
            Value::Int(1),
            Value::Int(2),
        ],
    );
    assert!(result.is_ok());
    let timer_val = result.unwrap();
    assert!(matches!(timer_val, Value::Timer(_)));

    // cancel-timer
    let result = builtin_cancel_timer(&mut eval, vec![timer_val]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());

    // Verify it's cancelled
    if let Value::Timer(id) = timer_val {
        assert!(!eval.timers.timer_active_p(id));
    }
}

#[test]
fn test_eval_run_with_idle_timer() {
    use super::super::eval::Evaluator;

    let mut eval = Evaluator::new();

    let result = builtin_run_with_idle_timer(
        &mut eval,
        vec![Value::Int(5), Value::Nil, Value::symbol("idle-func")],
    );
    assert!(result.is_ok());
    let timer_val = result.unwrap();

    // Should be a timer
    assert!(matches!(timer_val, Value::Timer(_)));

    // The timer should be idle
    if let Value::Timer(id) = timer_val {
        let timer = eval.timers.timers.iter().find(|t| t.id == id).unwrap();
        assert!(timer.idle);
    }
}

#[test]
fn test_eval_run_at_time_accepts_nil_and_string_specs() {
    use super::super::eval::Evaluator;

    let mut eval = Evaluator::new();

    let from_nil = builtin_run_at_time(
        &mut eval,
        vec![Value::Nil, Value::Nil, Value::symbol("cb-from-nil")],
    )
    .expect("nil time spec should be accepted");
    assert!(matches!(from_nil, Value::Timer(_)));

    let from_string = builtin_run_at_time(
        &mut eval,
        vec![
            Value::string("0 sec"),
            Value::Nil,
            Value::symbol("cb-from-string"),
        ],
    )
    .expect("string time spec should be accepted");
    assert!(matches!(from_string, Value::Timer(_)));
}

#[test]
fn test_parse_run_at_time_delay_units() {
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
    use super::super::eval::Evaluator;

    let mut eval = Evaluator::new();

    let invalid_string = builtin_run_at_time(
        &mut eval,
        vec![Value::string("abc"), Value::Nil, Value::symbol("cb")],
    );
    assert!(matches!(
        invalid_string,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "error"
    ));

    let invalid_type = builtin_run_at_time(
        &mut eval,
        vec![Value::True, Value::Nil, Value::symbol("cb")],
    );
    assert!(matches!(
        invalid_type,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "error"
    ));
}

#[test]
fn test_eval_run_with_idle_timer_nil_ok_string_error() {
    use super::super::eval::Evaluator;

    let mut eval = Evaluator::new();

    let from_nil =
        builtin_run_with_idle_timer(&mut eval, vec![Value::Nil, Value::Nil, Value::symbol("cb")])
            .expect("nil idle delay should be accepted");
    assert!(matches!(from_nil, Value::Timer(_)));

    let from_string = builtin_run_with_idle_timer(
        &mut eval,
        vec![Value::string("0 sec"), Value::Nil, Value::symbol("cb")],
    );
    assert!(matches!(
        from_string,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "error"
    ));
}

#[test]
fn test_eval_timer_activate() {
    use super::super::eval::Evaluator;

    let mut eval = Evaluator::new();

    // Create and cancel a timer
    let result = builtin_run_at_time(
        &mut eval,
        vec![
            Value::Float(1.0, next_float_id()),
            Value::Nil,
            Value::symbol("cb"),
        ],
    );
    let timer_val = result.unwrap();
    builtin_cancel_timer(&mut eval, vec![timer_val]).unwrap();

    if let Value::Timer(id) = &timer_val {
        assert!(!eval.timers.timer_active_p(*id));
    }

    // Reactivate
    let result = builtin_timer_activate(&mut eval, vec![timer_val]);
    assert!(result.is_ok());

    if let Value::Timer(id) = &timer_val {
        assert!(eval.timers.timer_active_p(*id));
    }

    // Active timers cannot be activated again.
    let second = builtin_timer_activate(&mut eval, vec![timer_val]);
    assert!(matches!(second, Err(Flow::Signal(sig)) if sig.symbol_name() == "error"));

    // Cancel again and verify optional args are accepted.
    builtin_cancel_timer(&mut eval, vec![timer_val]).unwrap();
    let with_restart = builtin_timer_activate(&mut eval, vec![timer_val, Value::True]);
    assert!(with_restart.is_ok());

    builtin_cancel_timer(&mut eval, vec![timer_val]).unwrap();
    let with_restart_and_delta = builtin_timer_activate(
        &mut eval,
        vec![
            timer_val,
            Value::Nil,
            Value::cons(Value::Int(1), Value::Int(2)),
        ],
    );
    assert!(with_restart_and_delta.is_ok());
}

#[test]
fn test_eval_timer_activate_rejects_non_timer_with_error() {
    use super::super::eval::Evaluator;

    let mut eval = Evaluator::new();
    let result = builtin_timer_activate(&mut eval, vec![Value::Nil]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "error"));
}

#[test]
fn test_eval_timer_activate_optional_delta_must_be_cons_or_nil() {
    use super::super::eval::Evaluator;

    let mut eval = Evaluator::new();
    let timer_val = builtin_run_at_time(
        &mut eval,
        vec![
            Value::Float(1.0, next_float_id()),
            Value::Nil,
            Value::symbol("cb"),
        ],
    )
    .unwrap();
    builtin_cancel_timer(&mut eval, vec![timer_val]).unwrap();

    let result = builtin_timer_activate(&mut eval, vec![timer_val, Value::Nil, Value::Int(2)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data == vec![Value::symbol("consp"), Value::Int(2)]
    ));
}
