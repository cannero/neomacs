use super::*;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};
use crate::emacs_core::{format_eval_result, parse_forms};
use std::cell::RefCell;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn eval_one(src: &str) -> String {
    let mut ev = Evaluator::new();
    let forms = parse_forms(src).expect("parse");
    let result = ev.eval_expr(&forms[0]);
    format_eval_result(&result)
}

fn eval_all(src: &str) -> Vec<String> {
    let mut ev = Evaluator::new();
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn eval_all_with_subr(src: &str) -> Vec<String> {
    let mut ev = Evaluator::new();
    load_minimal_backquote_runtime(&mut ev);
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn bootstrap_eval_one(src: &str) -> String {
    bootstrap_eval_all(src).into_iter().next().expect("result")
}

fn gnu_timer_after(delay: Duration, callback: &str) -> Value {
    let when = SystemTime::now()
        .checked_add(delay)
        .expect("timer deadline should fit in system time")
        .duration_since(UNIX_EPOCH)
        .expect("timer deadline should be after unix epoch");
    let secs = when.as_secs() as i64;

    Value::vector(vec![
        Value::Nil,
        Value::Int(secs >> 16),
        Value::Int(secs & 0xFFFF),
        Value::Int(when.subsec_micros() as i64),
        Value::Nil,
        Value::symbol(callback),
        Value::Nil,
        Value::Nil,
        Value::Int(0),
        Value::Nil,
    ])
}

#[test]
fn eval_with_explicit_lexenv_restores_outer_lexenv() {
    assert_eq!(
        eval_one("(let ((x 41)) (list (eval 'x '((x . 7))) x))"),
        "OK (7 41)"
    );
}

fn load_minimal_backquote_runtime(eval: &mut Evaluator) {
    use crate::emacs_core::load::{find_file_in_load_path, get_load_path, load_file};

    eval.set_lexical_binding(true);
    eval.set_variable(
        "load-path",
        Value::list(vec![
            Value::string(concat!(env!("CARGO_MANIFEST_DIR"), "/../lisp/emacs-lisp")),
            Value::string(concat!(env!("CARGO_MANIFEST_DIR"), "/../lisp")),
        ]),
    );
    let load_path = get_load_path(&eval.obarray());
    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
    ] {
        let path = find_file_in_load_path(name, &load_path)
            .unwrap_or_else(|| panic!("cannot find {name}"));
        load_file(eval, &path).unwrap_or_else(|e| panic!("load {name}: {e:?}"));
    }
}

#[test]
fn evaluator_drop_clears_owned_thread_locals() {
    {
        let mut ev = Evaluator::new_vm_harness();
        assert!(std::ptr::eq(
            crate::emacs_core::intern::current_interner_ptr(),
            &mut *ev.interner,
        ));
        assert!(crate::emacs_core::value::has_current_heap());
        assert!(std::ptr::eq(
            crate::emacs_core::value::current_heap_ptr(),
            &mut *ev.heap,
        ));
    }

    assert!(crate::emacs_core::intern::current_interner_ptr().is_null());
    assert!(!crate::emacs_core::value::has_current_heap());
}

#[test]
fn read_char_applies_resize_event_before_returning_next_keypress() {
    let mut ev = Evaluator::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);

    tx.send(crate::keyboard::InputEvent::Resize {
        width: 700,
        height: 800,
        emacs_frame_id: 0,
    })
    .unwrap();
    tx.send(crate::keyboard::InputEvent::KeyPress(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .unwrap();

    let event = ev.read_char().expect("read_char should return a keypress");
    assert_eq!(event, Value::Int('a' as i64));

    let frame = ev.frames.get(fid).expect("frame should still be live");
    assert_eq!(frame.width, 700);
    assert_eq!(frame.height, 800);
}

#[test]
fn read_char_triggers_redisplay_after_resize_event() {
    let mut ev = Evaluator::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let redisplay_calls_in_cb = redisplay_calls.clone();
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Evaluator| {
        let frame = ev
            .frames
            .selected_frame()
            .expect("selected frame during redisplay");
        redisplay_calls_in_cb
            .borrow_mut()
            .push((frame.width, frame.height));
    }));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);

    tx.send(crate::keyboard::InputEvent::Resize {
        width: 700,
        height: 800,
        emacs_frame_id: 0,
    })
    .unwrap();
    tx.send(crate::keyboard::InputEvent::KeyPress(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .unwrap();

    let event = ev.read_char().expect("read_char should return a keypress");
    assert_eq!(event, Value::Int('a' as i64));
    assert_eq!(*redisplay_calls.borrow(), vec![(700, 800)]);
}

#[test]
fn redisplay_applies_pending_resize_before_callback() {
    let mut ev = Evaluator::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let redisplay_calls_in_cb = redisplay_calls.clone();
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Evaluator| {
        let frame = ev
            .frames
            .selected_frame()
            .expect("selected frame during redisplay");
        redisplay_calls_in_cb
            .borrow_mut()
            .push((frame.width, frame.height));
    }));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    tx.send(crate::keyboard::InputEvent::Resize {
        width: 700,
        height: 800,
        emacs_frame_id: 0,
    })
    .unwrap();

    ev.redisplay();

    assert_eq!(*redisplay_calls.borrow(), vec![(700, 800)]);
}

#[test]
fn recursive_edit_runs_top_level_before_outer_command_loop_reads_input() {
    let mut ev = Evaluator::new();
    let setup = parse_forms("(setq top-level '(setq neo-top-level-hit t))").expect("parse");
    let _ = ev.eval_forms(&setup);

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::CloseRequested)
        .expect("queue close request");
    drop(tx);

    ev.input_rx = Some(rx);
    ev.command_loop.running = true;

    let result = ev
        .recursive_edit_inner()
        .expect("outer command loop should exit cleanly");
    assert_eq!(result, Value::Nil);
    assert!(
        ev.eval_symbol("neo-top-level-hit")
            .expect("top-level probe should be bound")
            .is_truthy(),
        "expected recursive_edit to evaluate `top-level' before waiting for input"
    );
}

#[test]
fn frame_native_width_syncs_pending_resize_without_read_char() {
    let mut ev = Evaluator::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    ev.frames
        .get_mut(fid)
        .expect("frame should exist")
        .parameters
        .insert("window-system".to_string(), Value::symbol("x"));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    tx.send(crate::keyboard::InputEvent::Resize {
        width: 700,
        height: 800,
        emacs_frame_id: 0,
    })
    .unwrap();

    let width = crate::emacs_core::window_cmds::builtin_frame_native_width(&mut ev, vec![])
        .expect("frame-native-width should succeed");
    let height = crate::emacs_core::window_cmds::builtin_frame_native_height(&mut ev, vec![])
        .expect("frame-native-height should succeed");

    assert_eq!(width, Value::Int(700));
    assert_eq!(height, Value::Int(800));
}

#[test]
fn frame_native_width_syncs_pending_resize_behind_focus_event() {
    let mut ev = Evaluator::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    ev.frames
        .get_mut(fid)
        .expect("frame should exist")
        .parameters
        .insert("window-system".to_string(), Value::symbol("x"));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    tx.send(crate::keyboard::InputEvent::Focus(true)).unwrap();
    tx.send(crate::keyboard::InputEvent::Resize {
        width: 700,
        height: 800,
        emacs_frame_id: 0,
    })
    .unwrap();

    let width = crate::emacs_core::window_cmds::builtin_frame_native_width(&mut ev, vec![])
        .expect("frame-native-width should succeed");
    let height = crate::emacs_core::window_cmds::builtin_frame_native_height(&mut ev, vec![])
        .expect("frame-native-height should succeed");

    assert_eq!(width, Value::Int(700));
    assert_eq!(height, Value::Int(800));
}

#[test]
fn redisplay_preserves_non_resize_input_for_read_char() {
    let mut ev = Evaluator::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    tx.send(crate::keyboard::InputEvent::KeyPress(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .unwrap();

    ev.redisplay();

    let event = ev
        .read_char()
        .expect("read_char should return queued keypress");
    assert_eq!(event, Value::Int('a' as i64));
}

#[test]
fn fire_pending_timers_executes_lisp_callbacks() {
    let mut ev = Evaluator::new();
    ev.set_variable("vm-timer-fired", Value::Nil);
    let forms = parse_forms(
        "(progn
           (fset 'vm-test-timer-callback
                 (lambda () (setq vm-timer-fired 'done)))
           (fset 'timer-event-handler
                 (lambda (timer)
                   (setq timer-list nil)
                   (funcall (aref timer 5)))))",
    )
    .expect("parse timer test setup");
    ev.eval_expr(&forms[0]).expect("install timer handlers");

    let timer = Value::vector(vec![
        Value::Nil,
        Value::Int(0),
        Value::Int(0),
        Value::Int(0),
        Value::Nil,
        Value::symbol("vm-test-timer-callback"),
        Value::Nil,
        Value::Nil,
        Value::Int(0),
        Value::Nil,
    ]);
    ev.set_variable("timer-list", Value::list(vec![timer]));

    ev.fire_pending_timers();

    assert_eq!(
        ev.eval_symbol("vm-timer-fired")
            .expect("timer flag should be bound"),
        Value::symbol("done")
    );
}

#[test]
fn fire_pending_timers_requests_redisplay_after_callbacks() {
    let mut ev = Evaluator::new();
    ev.set_variable("vm-timer-fired", Value::Nil);

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let redisplay_calls_in_cb = redisplay_calls.clone();
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Evaluator| {
        redisplay_calls_in_cb.borrow_mut().push(
            ev.eval_symbol("vm-timer-fired")
                .expect("timer flag during redisplay"),
        );
    }));

    let forms = parse_forms(
        "(progn
           (fset 'vm-test-timer-callback
                 (lambda () (setq vm-timer-fired 'done)))
           (fset 'timer-event-handler
                 (lambda (timer)
                   (setq timer-list nil)
                   (funcall (aref timer 5)))))",
    )
    .expect("parse timer test setup");
    ev.eval_expr(&forms[0]).expect("install timer handlers");

    let timer = Value::vector(vec![
        Value::Nil,
        Value::Int(0),
        Value::Int(0),
        Value::Int(0),
        Value::Nil,
        Value::symbol("vm-test-timer-callback"),
        Value::Nil,
        Value::Nil,
        Value::Int(0),
        Value::Nil,
    ]);
    ev.set_variable("timer-list", Value::list(vec![timer]));

    ev.fire_pending_timers();

    assert_eq!(*redisplay_calls.borrow(), vec![Value::symbol("done")]);
}

#[test]
fn next_input_wait_timeout_accounts_for_gnu_timer_list() {
    let mut ev = Evaluator::new();
    ev.set_variable(
        "timer-list",
        Value::list(vec![gnu_timer_after(Duration::from_millis(200), "ignore")]),
    );

    let timeout = ev
        .next_input_wait_timeout()
        .expect("gnu timer should bound read_char wait");

    assert!(timeout > Duration::ZERO);
    assert!(timeout <= Duration::from_millis(200));
}

#[test]
fn next_input_wait_timeout_chooses_earliest_timer_source() {
    let mut ev = Evaluator::new();
    ev.set_variable(
        "timer-list",
        Value::list(vec![gnu_timer_after(Duration::from_millis(250), "ignore")]),
    );
    ev.timers
        .add_timer(0.05, 0.0, Value::symbol("ignore-rust"), vec![], false);

    let timeout = ev
        .next_input_wait_timeout()
        .expect("timers should bound read_char wait");

    assert!(timeout <= Duration::from_millis(100));
}

#[test]
fn read_char_fires_bootstrapped_gnu_run_with_timer_while_waiting_for_input() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");

    let forms = parse_forms(
        "(progn
           (setq vm-timer-fired nil)
           (run-with-timer
            0.01 nil
            (lambda () (setq vm-timer-fired 'done))))",
    )
    .expect("parse timer program");
    ev.eval_expr(&forms[0]).expect("schedule GNU Lisp timer");

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        tx.send(crate::keyboard::InputEvent::KeyPress(
            crate::keyboard::KeyEvent::char('a'),
        ))
        .expect("send keypress");
    });

    let event = ev
        .read_char()
        .expect("read_char should return queued keypress");
    assert_eq!(event, Value::Int('a' as i64));
    assert_eq!(
        ev.eval_symbol("vm-timer-fired")
            .expect("timer flag should be bound"),
        Value::symbol("done")
    );
}

#[test]
fn callable_print_targets_stream_gnu_char_callbacks() {
    assert_eq!(
        eval_one(
            r#"(progn
                 (setq vm-print-calls nil)
                 (fset 'vm-print-target
                       (lambda (ch)
                         (setq vm-print-calls (cons ch vm-print-calls))))
                 (list
                  (progn
                    (setq vm-print-calls nil)
                    (princ "ab" 'vm-print-target)
                    vm-print-calls)
                  (progn
                    (setq vm-print-calls nil)
                    (prin1 '(1 . 2) 'vm-print-target)
                    vm-print-calls)
                  (progn
                    (setq vm-print-calls nil)
                    (print 'foo 'vm-print-target)
                    vm-print-calls)))"#
        ),
        "OK ((98 97) (41 50 32 46 32 49 40) (10 111 111 102 10))"
    );
}

#[test]
fn marker_print_targets_insert_and_restore_like_gnu() {
    assert_eq!(
        eval_one(
            r#"(let* ((orig (current-buffer))
                      (obuf (get-buffer-create "*vm-marker-print*")))
                 (with-current-buffer obuf
                   (erase-buffer)
                   (insert "xy")
                   (goto-char 2))
                 (let ((m (with-current-buffer obuf (point-marker))))
                   (list
                    (progn
                      (princ "ab" m)
                      (with-current-buffer obuf
                        (list (buffer-string) (point) (marker-position m))))
                    (progn
                      (write-char 67 m)
                      (with-current-buffer obuf
                        (list (buffer-string) (point) (marker-position m))))
                    (progn
                      (terpri m)
                      (with-current-buffer obuf
                        (list (buffer-string) (point) (marker-position m))))
                    (eq (current-buffer) orig)
                    (point))))"#
        ),
        "OK ((\"xaby\" 4 4) (\"xabCy\" 5 5) (\"xabC\ny\" 6 6) t 1)"
    );
}

#[test]
fn basic_arithmetic() {
    assert_eq!(eval_one("(+ 1 2)"), "OK 3");
    assert_eq!(eval_one("(- 10 3)"), "OK 7");
    assert_eq!(eval_one("(* 4 5)"), "OK 20");
    assert_eq!(eval_one("(/ 10 3)"), "OK 3");
    assert_eq!(eval_one("(% 10 3)"), "OK 1");
    assert_eq!(eval_one("(1+ 5)"), "OK 6");
    assert_eq!(eval_one("(1- 5)"), "OK 4");
}

#[test]
fn substring_accepts_vectors_like_gnu_emacs() {
    assert_eq!(
        eval_one("(substring [10 20 30 40 50] 1 4)"),
        "OK [20 30 40]"
    );
    assert_eq!(eval_one("(substring [10 20 30 40 50] -3 -1)"), "OK [30 40]");
    assert_eq!(eval_one("(substring [10 20 30] 0)"), "OK [10 20 30]");
}

#[test]
fn substring_then_string_match_mirrors_gnu_bracket_class_closing() {
    assert_eq!(
        bootstrap_eval_one(
            r#"(let* ((code "x = 42;")
                      (rest (substring code 2)))
                 (list rest
                       (string-match "\\`[-+*/=<>!&|(){}\\[\\];,.]" rest)))"#
        ),
        r#"OK ("= 42;" nil)"#
    );
}

#[test]
fn bootstrap_string_match_posix_upper_class_folds_to_alpha_under_case_fold() {
    assert_eq!(
        bootstrap_eval_one(
            r#"(list
                 (string-match "[[:upper:]]+" "helloWORLDfoo")
                 (match-string 0 "helloWORLDfoo"))"#
        ),
        r#"OK (0 "helloWORLDfoo")"#
    );
}

#[test]
fn bootstrap_string_match_explicit_numbered_group_preserves_group_slot() {
    assert_eq!(
        bootstrap_eval_one(
            r#"(let ((case-fold-search nil))
                 (list
                  (string-match "\\(?9:[A-Z]+\\)" "xxABCyy")
                  (match-string 9 "xxABCyy")))"#
        ),
        r#"OK (2 "ABC")"#
    );
}

#[test]
fn bootstrap_string_match_open_interval_quantifier_matches_gnu_semantics() {
    assert_eq!(
        bootstrap_eval_one(
            r#"(list
                 (string-match "a\\{,2\\}b" "aab")
                 (match-string 0 "aab"))"#
        ),
        r#"OK (0 "aab")"#
    );
}

#[test]
fn bootstrap_string_match_posix_char_class_sequence_matches_gnu_order() {
    assert_eq!(
        bootstrap_eval_one(
            r#"(list
                 (string-match "[[:alpha:]]+" "hello123")
                 (match-string 0 "hello123")
                 (string-match "[[:digit:]]+" "hello123")
                 (match-string 0 "hello123")
                 (string-match "[[:alnum:]]+" "  abc123  ")
                 (match-string 0 "  abc123  ")
                 (string-match "[[:space:]]+" "hello   world")
                 (match-string 0 "hello   world")
                 (string-match "[[:upper:]]+" "helloWORLDfoo")
                 (match-string 0 "helloWORLDfoo")
                 (string-match "[[:lower:]]+" "HELLOworldFOO")
                 (match-string 0 "HELLOworldFOO")
                 (string-match "[[:punct:]]+" "hello!@#world")
                 (match-string 0 "hello!@#world")
                 (string-match "[^[:digit:]]+" "123abc456")
                 (match-string 0 "123abc456")
                 (string-match "[[:alpha:][:digit:]]+" "---abc123---")
                 (match-string 0 "---abc123---")
                 (progn (string-match "[[:blank:]]+" "a \t b")
                        (match-string 0 "a \t b")))"#
        ),
        r#"OK (0 "hello" 5 "123" 2 "abc123" 5 "   " 0 "helloWORLDfoo" 0 "HELLOworldFOO" 5 "!@#" 3 "abc" 3 "abc123" " 	 ")"#
    );
}

#[test]
fn void_function_symbol_signals_before_evaluating_arguments_like_gnu_emacs() {
    assert_eq!(
        eval_one(
            r#"
(let ((vm-side nil))
  (condition-case err
      (vm-undefined-function
       (progn
         (setq vm-side t)
         1))
    (error (list err vm-side))))
"#
        ),
        "OK ((void-function vm-undefined-function) nil)"
    );
}

#[test]
fn eval_of_generated_lambda_preserves_uninterned_symbol_identity() {
    assert_eq!(
        eval_one(
            r#"
(let* ((exp (make-symbol "exp"))
       (form (list 'let
                   '((lexical-binding t))
                   (list 'lambda
                         '(new)
                         (list 'let*
                               (list (list exp 'new)
                                     (list 'x exp))
                               'x))))
       (f (eval form t)))
  (funcall f 42))
"#
        ),
        "OK 42"
    );
}

#[test]
fn save_restriction_restores_labeled_restrictions_and_widen_semantics() {
    let mut eval = Evaluator::new();
    let buffer_id = eval.buffers.create_buffer("eval-labeled-restriction");
    eval.buffers.set_current(buffer_id);
    let _ = eval.buffers.insert_into_buffer(buffer_id, "abcdef");
    let forms = parse_forms(
        r#"(progn
             (internal--labeled-narrow-to-region 2 5 'tag)
             (list (point-min) (point-max)
                   (save-restriction
                     (internal--labeled-widen 'tag)
                     (list (point-min) (point-max)))
                   (point-min) (point-max)
                   (progn (widen) (list (point-min) (point-max)))
                   (progn (internal--labeled-widen 'tag)
                          (list (point-min) (point-max)))))"#,
    )
    .expect("parse");
    let result = eval.eval_expr(&forms[0]);
    assert_eq!(
        format_eval_result(&result),
        "OK (2 5 (1 7) 2 5 (2 5) (1 7))"
    );
}

#[test]
fn redisplay_restores_current_innermost_labeled_restriction_after_callback_mutation() {
    let mut eval = Evaluator::new();
    let buffer_id = eval.buffers.create_buffer("redisplay-labeled");
    eval.buffers.set_current(buffer_id);
    let _ = eval.buffers.insert_into_buffer(buffer_id, "abcdef");
    let _ = eval
        .buffers
        .internal_labeled_narrow_to_region(buffer_id, 1, 5, Value::symbol("outer"));
    let _ = eval
        .buffers
        .internal_labeled_narrow_to_region(buffer_id, 2, 4, Value::symbol("inner"));

    let observed = Rc::new(RefCell::new(Vec::new()));
    let observed_in_callback = observed.clone();
    eval.redisplay_fn = Some(Box::new(move |ev: &mut Evaluator| {
        let buf = ev
            .buffers
            .get(buffer_id)
            .expect("buffer visible during redisplay");
        observed_in_callback
            .borrow_mut()
            .push((buf.point_min(), buf.point_max()));
        let _ = ev
            .buffers
            .internal_labeled_widen(buffer_id, &Value::symbol("inner"));
        let buf = ev
            .buffers
            .get(buffer_id)
            .expect("buffer after labeled widen");
        observed_in_callback
            .borrow_mut()
            .push((buf.point_min(), buf.point_max()));
    }));

    eval.redisplay();

    assert_eq!(*observed.borrow(), vec![(0, 6), (1, 5)]);
    let buf = eval.buffers.get(buffer_id).expect("buffer after redisplay");
    assert_eq!((buf.point_min(), buf.point_max()), (1, 5));
}

#[test]
fn simple_defvar_declares_local_dynamic_scope_in_lexical_environment() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    ev.lexenv = Value::list(vec![Value::True]);

    let forms = parse_forms(
        r#"
        (progn
          (defvar vm-local-special)
          (let ((vm-local-special 10))
            (let ((f (lambda () vm-local-special)))
              (let ((vm-local-special 20))
                (funcall f)))))
    "#,
    )
    .expect("parse");

    let result = ev.eval_expr(&forms[0]);
    assert_eq!(format_eval_result(&result), "OK 20");
}

#[test]
fn put_get_preserves_closure_captured_uninterned_symbol_identity() {
    assert_eq!(
        eval_one(
            r#"
(let* ((exp (make-symbol "exp"))
       (form (list 'let
                   '((lexical-binding t))
                   (list 'lambda
                         '(new)
                         (list 'let*
                               (list (list exp 'new)
                                     (list 'x exp))
                               'x))))
       (f (eval form t)))
  (put 'vm-closure-prop 'vm-test-prop f)
  (garbage-collect)
  (funcall (get 'vm-closure-prop 'vm-test-prop) 42))
"#
        ),
        "OK 42"
    );
}

#[test]
fn recent_input_events_are_bounded() {
    let mut ev = Evaluator::new();
    for i in 0..(RECENT_INPUT_EVENT_LIMIT + 1) {
        ev.record_input_event(Value::Int(i as i64));
    }
    let recent = ev.recent_input_events();
    assert_eq!(recent.len(), RECENT_INPUT_EVENT_LIMIT);
    assert_eq!(recent[0], Value::Int(1));
    assert_eq!(
        recent.last(),
        Some(&Value::Int(RECENT_INPUT_EVENT_LIMIT as i64))
    );
}

#[test]
fn eval_and_compile_defines_function() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        r#"
        (defmacro eval-and-compile (&rest body)
          (list 'quote (eval (cons 'progn body))))
        (eval-and-compile
          (defun my-test-fn (x) (+ x 1)))
        (my-test-fn 41)
    "#,
    )
    .expect("parse");
    let results: Vec<String> = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect();
    tracing::debug!("eval-and-compile results: {:?}", results);
    // The function should be defined by eval-and-compile
    assert!(
        ev.obarray().symbol_function("my-test-fn").is_some(),
        "my-test-fn should be defined after eval-and-compile"
    );
    assert_eq!(results[2], "OK 42");
}

#[test]
fn eval_and_compile_with_backtick_name() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        r#"
        (defmacro eval-and-compile (&rest body)
          (list 'quote (eval (cons 'progn body))))
        (let ((fsym (intern (format "%s--pcase-macroexpander" '\`))))
          (eval (list 'eval-and-compile
                      (list 'defun fsym '(x) '(+ x 1)))))
    "#,
    )
    .expect("parse");
    let results: Vec<String> = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect();
    tracing::debug!("backtick-name results: {:?}", results);
    let has_fn = ev
        .obarray()
        .symbol_function("`--pcase-macroexpander")
        .is_some();
    tracing::debug!("`--pcase-macroexpander defined: {}", has_fn);
    // Check what format produces for the backtick symbol
    let fmt_forms = parse_forms(r#"(format "%s--pcase-macroexpander" '\`)"#).expect("parse");
    let fmt_result = ev.eval_expr(&fmt_forms[0]);
    tracing::debug!("format result: {:?}", format_eval_result(&fmt_result));
}

#[test]
fn float_arithmetic() {
    assert_eq!(eval_one("(+ 1.0 2.0)"), "OK 3.0");
    assert_eq!(eval_one("(+ 1 2.0)"), "OK 3.0"); // int promoted to float
    assert_eq!(eval_one("(/ 10.0 3.0)"), "OK 3.3333333333333335");
}

#[test]
fn eq_float_corner_cases_match_oracle_shape() {
    assert_eq!(
        eval_one("(list (eq 1.0 1.0) (let ((x 1.0)) (eq x x)) (eq 0.0 -0.0) (eql 0.0 -0.0))"),
        "OK (nil t nil nil)"
    );
}

#[test]
fn intern_keyword_matches_reader_keyword_for_eq_and_memq() {
    assert_eq!(
        eval_one(
            r#"(let* ((k (intern ":beginning"))
                      (keys (list k (intern ":end") (intern ":value"))))
                 (list (keywordp k)
                       (eq k :beginning)
                       (if (memq :beginning keys) t nil)
                       (eq (intern-soft ":beginning") :beginning)))"#
        ),
        "OK (t t t t)"
    );
}

#[test]
fn setq_keeps_canonical_symbols_in_obarray() {
    assert_eq!(
        eval_one(
            r#"(let ((s 'vm-ghost))
                 (setq vm-ghost 1)
                 (list (if (intern-soft "vm-ghost") t nil)
                       (let (seen)
                         (mapatoms (lambda (x) (when (eq x s) (setq seen t))))
                         seen)
                       (symbol-value s)))"#
        ),
        "OK (t t 1)"
    );
}

#[test]
fn uninterned_nil_function_is_not_treated_as_canonical_nil() {
    assert_eq!(
        bootstrap_eval_one(
            r#"(let ((s (make-symbol "nil")))
                 (fset s (lambda () 'ok))
                 (list (special-form-p s) (funcall s)))"#
        ),
        "OK (nil ok)"
    );
}

#[test]
fn comparisons() {
    assert_eq!(eval_one("(< 1 2)"), "OK t");
    assert_eq!(eval_one("(> 1 2)"), "OK nil");
    assert_eq!(eval_one("(= 3 3)"), "OK t");
    assert_eq!(eval_one("(<= 3 3)"), "OK t");
    assert_eq!(eval_one("(>= 5 3)"), "OK t");
    assert_eq!(eval_one("(/= 1 2)"), "OK t");
}

#[test]
fn type_predicates() {
    assert_eq!(eval_one("(integerp 42)"), "OK t");
    assert_eq!(eval_one("(floatp 3.14)"), "OK t");
    assert_eq!(eval_one("(stringp \"hello\")"), "OK t");
    assert_eq!(eval_one("(symbolp 'foo)"), "OK t");
    assert_eq!(eval_one("(consp '(1 2))"), "OK t");
    assert_eq!(eval_one("(null nil)"), "OK t");
    assert_eq!(eval_one("(null t)"), "OK nil");
    assert_eq!(eval_one("(listp nil)"), "OK t");
}

#[test]
fn string_operations() {
    assert_eq!(
        eval_one(r#"(concat "hello" " " "world")"#),
        r#"OK "hello world""#
    );
    assert_eq!(eval_one(r#"(substring "hello" 1 3)"#), r#"OK "el""#);
    assert_eq!(eval_one(r#"(length "hello")"#), "OK 5");
    assert_eq!(eval_one(r#"(upcase "hello")"#), r#"OK "HELLO""#);
    assert_eq!(eval_one(r#"(string-equal "abc" "abc")"#), "OK t");
}

#[test]
fn and_or_cond() {
    assert_eq!(eval_one("(and 1 2 3)"), "OK 3");
    assert_eq!(eval_one("(and 1 nil 3)"), "OK nil");
    assert_eq!(eval_one("(or nil nil 3)"), "OK 3");
    assert_eq!(eval_one("(or nil nil nil)"), "OK nil");
    assert_eq!(eval_one("(cond (nil 1) (t 2))"), "OK 2");
}

#[test]
fn while_loop() {
    assert_eq!(
        eval_one("(let ((x 0)) (while (< x 5) (setq x (1+ x))) x)"),
        "OK 5"
    );
}

#[test]
fn defvar_only_sets_if_unbound() {
    let results = eval_all("(defvar x 42) x (defvar x 99) x");
    assert_eq!(results, vec!["OK x", "OK 42", "OK x", "OK 42"]);
}

#[test]
fn defvar_and_defconst_error_payloads_match_oracle_edges() {
    let results = eval_all(
        "(condition-case err (defvar) (error err))
         (condition-case err (defvar 1) (error err))
         (condition-case err (defvar 'vm-dv) (error err))
         (condition-case err (defvar vm-dv 1 \"doc\" t) (error err))
         (condition-case err (defconst) (error err))
         (condition-case err (defconst vm-dc) (error err))
         (condition-case err (defconst 1 2) (error err))
         (condition-case err (defconst 'vm-dc 1) (error err))
         (condition-case err (defconst vm-dc 1 \"doc\" t) (error err))",
    );
    assert_eq!(results[0], "OK (wrong-number-of-arguments defvar 0)");
    assert_eq!(results[1], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[2], "OK (wrong-type-argument symbolp 'vm-dv)");
    assert_eq!(results[3], "OK (error \"Too many arguments\")");
    assert_eq!(results[4], "OK (wrong-number-of-arguments defconst 0)");
    assert_eq!(results[5], "OK (wrong-number-of-arguments defconst 1)");
    assert_eq!(results[6], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[7], "OK (wrong-type-argument symbolp 'vm-dc)");
    assert_eq!(results[8], "OK (error \"Too many arguments\")");
}

#[test]
fn setq_local_makes_binding_buffer_local() {
    let result = eval_one("(with-temp-buffer (setq-local vm-x 7) vm-x)");
    assert_eq!(result, "OK 7");
}

#[test]
fn setq_local_constant_and_type_payloads_match_oracle() {
    let results = eval_all(
        "(list
            (condition-case err (setq-local :foo 1) (error err))
            (condition-case err (setq-local nil 1) (error err))
            (condition-case err (setq-local t 1) (error err))
            (condition-case err (setq-local 1 2) (error err)))
         (let ((x 0))
           (condition-case err
               (setq-local nil (setq x 1))
             (error (list err x))))
         (let ((x 0))
           (condition-case err
               (setq-local :foo (setq x 2))
             (error (list err x))))",
    );
    assert_eq!(
        results[0],
        "OK ((setting-constant :foo) (setting-constant nil) (setting-constant t) (error \"Attempting to set a non-symbol: 1\"))"
    );
    assert_eq!(results[1], "OK ((setting-constant nil) 0)");
    assert_eq!(results[2], "OK ((setting-constant :foo) 0)");
}

#[test]
fn setq_local_follows_variable_alias_resolution() {
    let result = bootstrap_eval_one(
        "(progn
           (defvaralias 'vm-setq-local-alias 'vm-setq-local-base)
           (with-temp-buffer
             (setq-local vm-setq-local-alias 5)
             (list
               (symbol-value 'vm-setq-local-alias)
               (symbol-value 'vm-setq-local-base)
               (local-variable-p 'vm-setq-local-alias)
               (local-variable-p 'vm-setq-local-base)
               (buffer-local-boundp 'vm-setq-local-alias (current-buffer))
               (buffer-local-boundp 'vm-setq-local-base (current-buffer)))))",
    );
    assert_eq!(result, "OK (5 5 t t t t)");
}

#[test]
fn setq_local_alias_to_constant_preserves_error_payload_and_rhs_skip() {
    let results = eval_all(
        "(progn
           (defvaralias 'vm-setq-local-const 'nil)
           (let ((x 0))
             (condition-case err
                 (setq-local vm-setq-local-const (setq x 1))
               (error (list err x)))))
         (progn
           (defvaralias 'vm-setq-local-const-k ':vm-setq-local-k)
           (let ((x 0))
             (condition-case err
                 (setq-local vm-setq-local-const-k (setq x 2))
               (error (list err x)))))",
    );
    assert_eq!(results[0], "OK ((setting-constant vm-setq-local-const) 0)");
    assert_eq!(
        results[1],
        "OK ((setting-constant vm-setq-local-const-k) 0)"
    );
}

#[test]
fn setq_local_alias_triggers_single_watcher_callback_on_resolved_target() {
    let result = eval_one(
        "(progn
           (setq vm-setq-local-watch-events nil)
           (fset 'vm-setq-local-watch-rec
                 (lambda (symbol newval operation where)
                   (setq vm-setq-local-watch-events
                         (cons (list symbol newval operation where)
                               vm-setq-local-watch-events))))
           (defvaralias 'vm-setq-local-watch 'vm-setq-local-watch-base)
           (add-variable-watcher 'vm-setq-local-watch-base 'vm-setq-local-watch-rec)
           (with-temp-buffer
             (setq-local vm-setq-local-watch 7))
           (let ((where (nth 3 (car vm-setq-local-watch-events))))
             (list (length vm-setq-local-watch-events)
                   (car (car vm-setq-local-watch-events))
                   (nth 1 (car vm-setq-local-watch-events))
                   (nth 2 (car vm-setq-local-watch-events))
                   (bufferp where)
                   (buffer-live-p where))))",
    );
    assert_eq!(result, "OK (1 vm-setq-local-watch-base 7 set t nil)");
}

#[test]
fn defmacro_works() {
    let result = eval_all(
        "(defmacro my-when (cond &rest body)
           (list 'if cond (cons 'progn body)))
         (my-when t 1 2 3)",
    );
    assert_eq!(result[1], "OK 3");
}

#[test]
fn defun_and_defmacro_allow_empty_body() {
    let results = eval_all(
        "(defun vm-empty-f nil)
         (vm-empty-f)
         (defmacro vm-empty-m nil)
         (vm-empty-m)",
    );
    assert_eq!(results[0], "OK vm-empty-f");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK vm-empty-m");
    assert_eq!(results[3], "OK nil");
}

#[test]
fn defun_and_defmacro_error_payloads_match_oracle_edges() {
    let results = eval_all(
        "(condition-case err (defun) (error err))
         (condition-case err (defun 1 nil) (error err))
         (condition-case err (defun 'vm-df nil 1) (error err))
         (condition-case err (defmacro) (error err))
         (condition-case err (defmacro 1 nil) (error err))
         (condition-case err (defmacro 'vm-dm nil 1) (error err))",
    );
    assert_eq!(results[0], "OK (wrong-number-of-arguments (2 . 2) 0)");
    assert_eq!(results[1], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[2], "OK (wrong-type-argument symbolp 'vm-df)");
    assert_eq!(results[3], "OK (wrong-number-of-arguments (2 . 2) 0)");
    assert_eq!(results[4], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[5], "OK (wrong-type-argument symbolp 'vm-dm)");
}

#[test]
fn optional_and_rest_params() {
    let results = eval_all(
        "(defun f (a &optional b &rest c) (list a b c))
         (f 1)
         (f 1 2)
         (f 1 2 3 4)",
    );
    assert_eq!(results[1], "OK (1 nil nil)");
    assert_eq!(results[2], "OK (1 2 nil)");
    assert_eq!(results[3], "OK (1 2 (3 4))");
}

#[test]
fn when_unless() {
    assert_eq!(eval_one("(when t 1 2 3)"), "OK 3");
    assert_eq!(eval_one("(when nil 1 2 3)"), "OK nil");
    assert_eq!(eval_one("(unless nil 1 2 3)"), "OK 3");
    assert_eq!(eval_one("(unless t 1 2 3)"), "OK nil");
}

#[test]
fn bound_and_true_p_runtime_semantics() {
    assert_eq!(bootstrap_eval_one("(fboundp 'bound-and-true-p)"), "OK t");
    assert_eq!(bootstrap_eval_one("(macrop 'bound-and-true-p)"), "OK t");
    assert_eq!(
        bootstrap_eval_one("(let ((vm-batp t)) (bound-and-true-p vm-batp))"),
        "OK t"
    );
    assert_eq!(
        bootstrap_eval_one("(let ((vm-batp nil)) (bound-and-true-p vm-batp))"),
        "OK nil"
    );
    assert_eq!(
        bootstrap_eval_one("(bound-and-true-p vm-batp-unbound)"),
        "OK nil"
    );
    assert_eq!(
        bootstrap_eval_one("(condition-case err (bound-and-true-p) (error err))"),
        "OK (wrong-number-of-arguments (1 . 1) 0)"
    );
    assert_eq!(
        bootstrap_eval_one("(condition-case err (bound-and-true-p a b) (error err))"),
        "OK (wrong-number-of-arguments (1 . 1) 2)"
    );
    assert_eq!(
        bootstrap_eval_one("(condition-case err (bound-and-true-p 1) (error err))"),
        "OK (wrong-type-argument symbolp 1)"
    );
}

#[test]
fn hash_table_ops() {
    let results = eval_all(
        "(let ((ht (make-hash-table :test 'equal)))
           (puthash \"key\" 42 ht)
           (gethash \"key\" ht))",
    );
    assert_eq!(results[0], "OK 42");
}

#[test]
fn vector_ops() {
    assert_eq!(eval_one("(aref [10 20 30] 1)"), "OK 20");
    assert_eq!(eval_one("(length [1 2 3])"), "OK 3");
}

#[test]
fn vector_literals_are_self_evaluating_constants() {
    assert_eq!(eval_one("(aref [f1] 0)"), "OK f1");
    assert_eq!(eval_one("(let ((f1 'shadowed)) (aref [f1] 0))"), "OK f1");
    assert_eq!(eval_one("(aref [(+ 1 2)] 0)"), "OK (+ 1 2)");
    assert_eq!(eval_one("(let ((x 1)) (aref [x] 0))"), "OK x");
}

#[test]
fn sort_keyword_form_returns_stable_copy_by_default() {
    assert_eq!(
        eval_one(
            "(let* ((xs '((1 . a) (1 . b) (0 . c)))
                    (ys (sort xs :key #'car)))
               (list xs ys (eq xs ys)))"
        ),
        "OK (((1 . a) (1 . b) (0 . c)) ((0 . c) (1 . a) (1 . b)) nil)"
    );
}

#[test]
fn sort_legacy_form_remains_in_place() {
    assert_eq!(
        eval_one(
            "(let* ((xs '((1 . a) (0 . b)))
                    (ys (sort xs (lambda (a b) (< (car a) (car b))))))
               (list xs ys (eq xs ys)))"
        ),
        "OK (((0 . b) (1 . a)) ((0 . b) (1 . a)) t)"
    );
}

#[test]
fn format_function() {
    assert_eq!(
        eval_one(r#"(format "hello %s, %d" "world" 42)"#),
        r#"OK "hello world, 42""#
    );
}

#[test]
fn prog1() {
    assert_eq!(eval_one("(prog1 1 2 3)"), "OK 1");
}

#[test]
fn function_special_form() {
    let results = eval_all(
        "(defun add1 (x) (+ x 1))
         (funcall #'add1 5)",
    );
    assert_eq!(results[1], "OK 6");
}

#[test]
fn function_special_form_symbol_and_literal_payloads() {
    assert_eq!(eval_one("#'car"), "OK car");
    assert_eq!(eval_one("#'definitely-missing"), "OK definitely-missing");
    assert_eq!(
        eval_one("(condition-case err #'1 (error (car err)))"),
        "OK 1"
    );
    assert_eq!(eval_one("(equal #''(lambda) ''(lambda))"), "OK t");
}

#[test]
fn lambda_captures_docstring_metadata() {
    let mut ev = Evaluator::new();
    let forms = parse_forms("(lambda nil \"lambda-doc\" nil)").expect("parse");
    let value = ev.eval_expr(&forms[0]).expect("eval");
    let docstring = value
        .get_lambda_data()
        .expect("expected lambda value")
        .docstring
        .clone();
    assert_eq!(docstring.as_deref(), Some("lambda-doc"));
}

#[test]
fn lambda_single_string_body_is_a_return_value_not_a_docstring() {
    let mut ev = Evaluator::new();
    let forms = parse_forms("(lambda nil \"ok-1\")").expect("parse");
    let value = ev.eval_expr(&forms[0]).expect("eval");
    let lambda = value.get_lambda_data().expect("expected lambda value");
    assert_eq!(lambda.docstring, None);
    assert_eq!(lambda.body.as_ref(), &vec![Expr::Str("ok-1".to_string())]);
    assert_eq!(eval_one("(funcall (lambda nil \"ok-1\"))"), "OK \"ok-1\"");
}

#[test]
fn defmacro_captures_docstring_metadata() {
    let mut ev = Evaluator::new();
    let forms = parse_forms("(defmacro vm-doc-macro (x) \"macro-doc\" x)").expect("parse");
    ev.eval_expr(&forms[0]).expect("eval defmacro");
    let macro_val = ev
        .obarray
        .symbol_function("vm-doc-macro")
        .cloned()
        .expect("macro function cell");
    let docstring = macro_val
        .get_lambda_data()
        .expect("expected macro value")
        .docstring
        .clone();
    assert_eq!(docstring.as_deref(), Some("macro-doc"));
}

#[test]
fn function_special_form_wrong_arity_signals() {
    assert_eq!(
        eval_one("(condition-case err (function) (error (car err)))"),
        "OK wrong-number-of-arguments"
    );
    assert_eq!(
        eval_one("(condition-case err (function 1 2) (error (car err)))"),
        "OK wrong-number-of-arguments"
    );
}

#[test]
fn special_form_arity_payloads_match_oracle_edges() {
    let results = eval_all(
        "(condition-case err (if) (error err))
         (condition-case err (if t) (error err))
         (condition-case err (when) (error err))
         (condition-case err (unless) (error err))
         (condition-case err (quote) (error err))
         (condition-case err (quote 1 2) (error err))
         (condition-case err (function) (error err))
         (condition-case err (function 1 2) (error err))
         (condition-case err (prog1) (error err))
         (condition-case err (catch) (error err))
         (condition-case err (throw) (error err))
         (condition-case err (condition-case) (error err))
         (condition-case err (let) (error err))
         (condition-case err (let*) (error err))
         (condition-case err (while) (error err))
         (condition-case err (unwind-protect) (error err))",
    );
    assert_eq!(results[0], "OK (wrong-number-of-arguments if 0)");
    assert_eq!(results[1], "OK (wrong-number-of-arguments if 1)");
    assert_eq!(results[2], "OK (wrong-number-of-arguments (1 . 1) 0)");
    assert_eq!(results[3], "OK (wrong-number-of-arguments (1 . 1) 0)");
    assert_eq!(results[4], "OK (wrong-number-of-arguments quote 0)");
    assert_eq!(results[5], "OK (wrong-number-of-arguments quote 2)");
    assert_eq!(results[6], "OK (wrong-number-of-arguments function 0)");
    assert_eq!(results[7], "OK (wrong-number-of-arguments function 2)");
    assert_eq!(results[8], "OK (wrong-number-of-arguments prog1 0)");
    assert_eq!(results[9], "OK (wrong-number-of-arguments catch 0)");
    assert_eq!(results[10], "OK (wrong-number-of-arguments throw 0)");
    assert_eq!(
        results[11],
        "OK (wrong-number-of-arguments condition-case 0)"
    );
    assert_eq!(results[12], "OK (wrong-number-of-arguments let 0)");
    assert_eq!(results[13], "OK (wrong-number-of-arguments let* 0)");
    assert_eq!(results[14], "OK (wrong-number-of-arguments while 0)");
    assert_eq!(
        results[15],
        "OK (wrong-number-of-arguments unwind-protect 0)"
    );
}

#[test]
fn let_dotted_binding_list_reports_listp_tail_payload() {
    assert_eq!(
        eval_one("(condition-case err (let ((x 1) . 2) x) (error err))"),
        "OK (wrong-type-argument listp 2)"
    );
    assert_eq!(
        eval_one("(condition-case err (let* ((x 1) . 2) x) (error err))"),
        "OK (wrong-type-argument listp 2)"
    );
}

#[test]
fn let_and_let_star_binding_constants_signal_setting_constant() {
    let results = eval_all(
        "(setq vm-let-a 0 vm-let-b 0)
         (condition-case err
             (let ((t (setq vm-let-a 1))
                   (x (setq vm-let-b 1)))
               x)
           (error (list :error (car err) (cdr err))))
         (list vm-let-a vm-let-b)
         (setq vm-let-a 0 vm-let-b 0)
         (condition-case err
             (let* ((t (setq vm-let-a 1))
                    (x (setq vm-let-b 1)))
               x)
           (error (list :error (car err) (cdr err))))
         (list vm-let-a vm-let-b)
         (condition-case err (let ((nil 1)) nil) (error (list :error (car err) (cdr err))))
         (condition-case err (let* ((nil 1)) nil) (error (list :error (car err) (cdr err))))
         (condition-case err (let (t) t) (error (list :error (car err) (cdr err))))
         (condition-case err (let* (t) t) (error (list :error (car err) (cdr err))))",
    );
    assert_eq!(results[1], "OK (:error setting-constant (t))");
    assert_eq!(results[2], "OK (1 1)");
    assert_eq!(results[4], "OK (:error setting-constant (t))");
    assert_eq!(results[5], "OK (1 0)");
    assert_eq!(results[6], "OK (:error setting-constant (nil))");
    assert_eq!(results[7], "OK (:error setting-constant (nil))");
    assert_eq!(results[8], "OK (:error setting-constant (t))");
    assert_eq!(results[9], "OK (:error setting-constant (t))");
}

#[test]
fn lambda_parameters_can_shadow_nil_and_t_like_gnu_emacs() {
    let results = eval_all(
        "(list
            (funcall (lambda (t) t) 7)
            (funcall (lambda (nil) nil) 9)
            (mapcar (lambda (t) t) '(1 2 3))
            (mapcar (lambda (nil) nil) '(4 5 6)))",
    );
    assert_eq!(results[0], "OK (7 9 (1 2 3) (4 5 6))");
}

#[test]
fn setq_can_assign_shadowing_nil_and_t_parameters_like_gnu_emacs() {
    let results = eval_all(
        "(list
            (funcall (lambda (t) (setq t 9) t) 7)
            (funcall (lambda (nil) (setq nil 11) nil) 8))",
    );
    assert_eq!(results[0], "OK (9 11)");
}

#[test]
fn random_accepts_string_seed_and_repeats_sequences_like_gnu_emacs() {
    let results = eval_all(
        "(let ((seq1 (progn (random \"vm-random-seed\") (list (random 1000) (random 1000) (random 1000))))
               (seq2 (progn (random \"vm-random-seed\") (list (random 1000) (random 1000) (random 1000)))))
           (list (integerp (random \"vm-random-seed\"))
                 (equal seq1 seq2)
                 (random 1)))",
    );
    assert_eq!(results[0], "OK (t t 0)");
}

#[test]
fn setq_constants_signal_setting_constant_after_rhs_evaluation() {
    let results = eval_all(
        "(setq vm-setq-side 0)
         (condition-case err
             (setq nil (setq vm-setq-side 1))
           (error (list (car err) (cdr err) vm-setq-side)))
         (setq vm-setq-side 0)
         (condition-case err
             (setq t (setq vm-setq-side 2))
           (error (list (car err) (cdr err) vm-setq-side)))
         (setq vm-setq-side 0)
         (condition-case err
             (setq :vm-key (setq vm-setq-side 3))
           (error (list (car err) (cdr err) vm-setq-side)))
         (condition-case err (setq 1 2) (error err))",
    );
    assert_eq!(results[1], "OK (setting-constant (nil) 1)");
    assert_eq!(results[3], "OK (setting-constant (t) 2)");
    assert_eq!(results[5], "OK (setting-constant (:vm-key) 3)");
    assert_eq!(results[6], "OK (wrong-type-argument symbolp 1)");
}

#[test]
fn set_ignores_lexical_bindings_and_updates_dynamic_cell() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    let forms = parse_forms(
        "(makunbound 'vm-lex-set)
         (let ((vm-lex-set 10))
           (list (set 'vm-lex-set 20) vm-lex-set (symbol-value 'vm-lex-set)))
         (makunbound 'vm-lex-set)",
    )
    .expect("parse");
    let results = ev.eval_forms(&forms);
    assert_eq!(format_eval_result(&results[1]), "OK (20 10 20)");
}

#[test]
fn setq_follows_variable_alias_resolution() {
    let results = eval_all(
        "(defvaralias 'vm-setq-alias 'vm-setq-base)
         (setq vm-setq-alias 3)
         (list (symbol-value 'vm-setq-base) (symbol-value 'vm-setq-alias))",
    );
    assert_eq!(results[2], "OK (3 3)");
}

#[test]
fn makunbound_ignores_lexical_bindings_and_unbinds_runtime_cell() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    let forms = parse_forms(
        "(setq vm-lex-makunbound 30)
         (let ((vm-lex-makunbound 10))
           (list (makunbound 'vm-lex-makunbound)
                 vm-lex-makunbound
                 (condition-case err
                     (symbol-value 'vm-lex-makunbound)
                   (error (car err)))))
         (condition-case err
             (symbol-value 'vm-lex-makunbound)
           (error (car err)))",
    )
    .expect("parse");
    let results = ev.eval_forms(&forms);
    assert_eq!(
        format_eval_result(&results[1]),
        "OK (vm-lex-makunbound 10 void-variable)"
    );
    assert_eq!(format_eval_result(&results[2]), "OK void-variable");
}

#[test]
fn makunbound_marks_dynamic_binding_void_without_falling_back_to_global() {
    let results = eval_all(
        "(defvar vm-mku-dyn 'global)
         (let ((vm-mku-dyn 'dyn))
           (list (makunbound 'vm-mku-dyn)
                 (condition-case err vm-mku-dyn (error (car err)))
                 (condition-case err (default-value 'vm-mku-dyn) (error (car err)))
                 (boundp 'vm-mku-dyn)))
         vm-mku-dyn
         (default-value 'vm-mku-dyn)",
    );
    assert_eq!(
        results[1],
        "OK (vm-mku-dyn void-variable void-variable nil)"
    );
    assert_eq!(results[2], "OK global");
    assert_eq!(results[3], "OK global");
}

#[test]
fn setq_alias_triggers_single_watcher_callback_on_resolved_target() {
    let results = eval_all(
        "(setq vm-setq-watch-events nil)
         (defun vm-setq-watch-rec (symbol newval operation where)
           (setq vm-setq-watch-events
                 (cons (list symbol newval operation where)
                       vm-setq-watch-events)))
         (defvaralias 'vm-setq-watch 'vm-setq-watch-base)
         (add-variable-watcher 'vm-setq-watch-base 'vm-setq-watch-rec)
         (setq vm-setq-watch 9)
         (length vm-setq-watch-events)",
    );
    assert_eq!(results[5], "OK 1");
}

#[test]
fn buffer_local_value_follows_alias_and_keyword_semantics() {
    let results = eval_all(
        "(progn
           (defvaralias 'vm-blv-alias 'vm-blv-base)
           (with-temp-buffer
             (setq-local vm-blv-alias 3)
             (list (buffer-local-value 'vm-blv-alias (current-buffer))
                   (buffer-local-value 'vm-blv-base (current-buffer))
                   (local-variable-p 'vm-blv-alias)
                   (local-variable-p 'vm-blv-base))))
         (progn
           (defvaralias 'vm-blv-alias2 'vm-blv-base2)
           (with-temp-buffer
             (condition-case err
                 (buffer-local-value 'vm-blv-alias2 (current-buffer))
               (error err))))
         (list
           (with-temp-buffer (buffer-local-value nil (current-buffer)))
           (with-temp-buffer (buffer-local-value t (current-buffer)))
           (with-temp-buffer (buffer-local-value :vm-blv-k (current-buffer)))
           (condition-case err
               (with-temp-buffer (buffer-local-value 'vm-blv-miss (current-buffer)))
             (error err))
           (condition-case err
               (with-temp-buffer (buffer-local-value 1 (current-buffer)))
             (error err)))",
    );
    assert_eq!(results[0], "OK (3 3 t t)");
    assert_eq!(results[1], "OK (void-variable vm-blv-alias2)");
    assert_eq!(
        results[2],
        "OK (nil t :vm-blv-k (void-variable vm-blv-miss) (wrong-type-argument symbolp 1))"
    );
}

#[test]
fn local_variable_if_set_p_follows_alias_and_contract_semantics() {
    let results = eval_all(
        "(progn
           (defvaralias 'vm-lvis-alias 'vm-lvis-base)
           (make-variable-buffer-local 'vm-lvis-base)
           (list (local-variable-if-set-p 'vm-lvis-alias)
                 (local-variable-if-set-p 'vm-lvis-base)))
         (list
           (condition-case err (local-variable-if-set-p nil) (error err))
           (condition-case err (local-variable-if-set-p t) (error err))
           (condition-case err (local-variable-if-set-p :vm-k) (error err))
           (condition-case err (local-variable-if-set-p 1) (error err))
           (condition-case err (local-variable-if-set-p 'x nil) (error err))
           (condition-case err (local-variable-if-set-p 'x (current-buffer)) (error err))
           (condition-case err (local-variable-if-set-p 'x 1) (error err))
           (condition-case err (local-variable-if-set-p 'x (current-buffer) nil)
             (error err)))",
    );
    assert_eq!(results[0], "OK (t t)");
    assert_eq!(
        results[1],
        "OK (nil nil nil (wrong-type-argument symbolp 1) nil nil nil (wrong-number-of-arguments local-variable-if-set-p 3))"
    );
}

#[test]
fn variable_binding_locus_follows_buffer_local_and_alias_semantics() {
    let results = eval_all(
        "(let ((locus (condition-case err
                          (progn (with-temp-buffer (setq-local x 2) (variable-binding-locus 'x)))
                        (error err))))
           (list (condition-case err (variable-binding-locus 'x) (error err))
                 (condition-case err (progn (setq x 1) (variable-binding-locus 'x)) (error err))
                 (bufferp locus)
                 (buffer-live-p locus)
                 (condition-case err (variable-binding-locus nil) (error err))
                 (condition-case err (variable-binding-locus t) (error err))
                 (condition-case err (variable-binding-locus :vm-k) (error err))
                 (condition-case err (variable-binding-locus 1) (error err))
                 (condition-case err (variable-binding-locus 'x nil) (error err))))
         (progn
           (defvaralias 'vm-vbl-alias 'vm-vbl-base)
           (with-temp-buffer
             (setq-local vm-vbl-alias 9)
             (list (bufferp (variable-binding-locus 'vm-vbl-alias))
                   (buffer-live-p (variable-binding-locus 'vm-vbl-alias))
                   (bufferp (variable-binding-locus 'vm-vbl-base))
                   (buffer-live-p (variable-binding-locus 'vm-vbl-base)))))",
    );
    assert_eq!(
        results[0],
        "OK (nil nil t nil nil nil nil (wrong-type-argument symbolp 1) (wrong-number-of-arguments variable-binding-locus 2))"
    );
    assert_eq!(results[1], "OK (t t t t)");
}

#[test]
fn value_lt_matches_oracle_type_and_ordering_semantics() {
    let results = eval_all(
        "(list
           (value< 1 2)
           (value< 2 1)
           (value< 1 1)
           (value< 'a 'b)
           (value< 'b 'a)
           (value< \"a\" \"b\")
           (condition-case err (value< 1 \"a\") (error err))
           (value< 1.0 2)
           (value< :a :b)
           (value< '(1 2) '(1 3))
           (value< '(1 2) '(1 2 0))
           (value< [1 2] [1 3])
           (condition-case err (value< [1] '(1)) (error err))
           (condition-case err (value< '(1 . 2) '(1 2)) (error err))
           (condition-case err (value< '(1 2) '(1 . 2)) (error err)))",
    );
    assert_eq!(
        results[0],
        "OK (t nil nil t nil t (type-mismatch 1 \"a\") t t t t t (type-mismatch [1] (1)) (type-mismatch 2 (2)) (type-mismatch (2) 2))"
    );
}

#[test]
fn variable_watchers_report_let_and_unlet_runtime_transitions() {
    let results = eval_all(
        "(setq vm-watch-events nil)
         (setq vm-watch-target 9)
         (defun vm-watch-rec (sym new op where)
           (setq vm-watch-events (cons (list op new) vm-watch-events)))
         (add-variable-watcher 'vm-watch-target 'vm-watch-rec)
         (let ((vm-watch-target 1)) 'done)
         vm-watch-events
         (setq vm-watch-events nil)
         (let* ((vm-watch-target 2)) 'done)
         vm-watch-events",
    );
    assert_eq!(results[5], "OK ((unlet 9) (let 1))");
    assert_eq!(results[8], "OK ((unlet 9) (let 2))");
}

#[test]
fn special_form_type_payloads_match_oracle_edges() {
    let results = eval_all(
        "(condition-case err (setq x) (error err))
         (condition-case err (setq 1 2) (error err))
         (condition-case err (let ((1 2)) nil) (error err))
         (condition-case err (let* ((1 2)) nil) (error err))
         (condition-case err (cond 1) (error err))
         (condition-case err (condition-case 1 2 (error 3)) (error err))
         (condition-case err (condition-case err 2 3) (error err))
         (condition-case err (condition-case err 2 ()) (error err))",
    );
    assert_eq!(results[0], "OK (wrong-number-of-arguments setq 1)");
    assert_eq!(results[1], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[2], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[3], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[4], "OK (wrong-type-argument listp 1)");
    assert_eq!(results[5], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[6], "OK (error \"Invalid condition handler: 3\")");
    assert_eq!(results[7], "OK 2");
}

#[test]
fn mapcar_works() {
    assert_eq!(eval_one("(mapcar #'1+ '(1 2 3))"), "OK (2 3 4)");
}

#[test]
fn apply_works() {
    assert_eq!(eval_one("(apply #'+ '(1 2 3))"), "OK 6");
    assert_eq!(eval_one("(apply #'+ 1 2 '(3))"), "OK 6");
}

#[test]
fn apply_improper_tail_signals_wrong_type_argument() {
    assert_eq!(
        eval_one(
            "(condition-case err
                 (apply 'list '(1 . 2))
               (error (list (car err) (nth 2 err))))"
        ),
        "OK (wrong-type-argument 2)"
    );
}

#[test]
fn funcall_and_apply_nil_signal_void_function() {
    let funcall_result = eval_one(
        "(condition-case err
             (funcall nil)
           (void-function (car err)))",
    );
    assert_eq!(funcall_result, "OK void-function");

    let apply_result = eval_one(
        "(condition-case err
             (apply nil nil)
           (void-function (car err)))",
    );
    assert_eq!(apply_result, "OK void-function");
}

#[test]
fn funcall_and_apply_non_callable_symbol_edges() {
    assert_eq!(
        eval_one("(condition-case err (funcall t) (error (car err)))"),
        "OK void-function"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall :vm-matrix-keyword) (error (car err)))"),
        "OK void-function"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall 'if) (error (car err)))"),
        "OK invalid-function"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall (symbol-function 'if) t 1 2) (error (car err)))"),
        "OK invalid-function"
    );
    assert_eq!(
        eval_one("(condition-case err (apply t nil) (error (car err)))"),
        "OK void-function"
    );
    assert_eq!(
        eval_one("(condition-case err (apply :vm-matrix-keyword nil) (error (car err)))"),
        "OK void-function"
    );
    assert_eq!(
        eval_one("(condition-case err (apply 'if '(t 1 2)) (error (car err)))"),
        "OK invalid-function"
    );
}

#[test]
fn funcall_throw_is_callable_and_preserves_throw_semantics() {
    assert_eq!(eval_one("(catch 'tag (funcall 'throw 'tag 42))"), "OK 42");
    assert_eq!(
        eval_one("(condition-case err (funcall 'throw 'tag 42) (error err))"),
        "OK (no-catch tag 42)"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall 'throw) (error err))"),
        "OK (wrong-number-of-arguments #<subr throw> 0)"
    );
}

#[test]
fn funcall_named_symbol_propagates_inner_invalid_function_payload() {
    assert_eq!(
        eval_one(
            "(progn
               (fset 'vm-invalid-wrap
                     (lambda ()
                       (funcall '(1 2 3))))
               (unwind-protect
                   (condition-case err
                       (funcall 'vm-invalid-wrap)
                     (invalid-function (nth 1 err)))
                 (fmakunbound 'vm-invalid-wrap)))"
        ),
        "OK (1 2 3)"
    );
}

#[test]
fn fmakunbound_masks_builtin_special_and_evaluator_callable_fallbacks() {
    let results = eval_all(
        "(fmakunbound 'car)
         (fboundp 'car)
         (symbol-function 'car)
         (condition-case err (car '(1 2)) (void-function 'void-function))
         (fmakunbound 'if)
         (fboundp 'if)
         (symbol-function 'if)
         (condition-case err (if t 1 2) (void-function 'void-function))
         (fmakunbound 'throw)
         (fboundp 'throw)
         (symbol-function 'throw)
         (condition-case err (throw 'tag 1) (void-function 'void-function))",
    );
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[3], "OK void-function");
    assert_eq!(results[5], "OK nil");
    assert_eq!(results[6], "OK nil");
    assert_eq!(results[7], "OK void-function");
    assert_eq!(results[9], "OK nil");
    assert_eq!(results[10], "OK nil");
    assert_eq!(results[11], "OK void-function");
}

#[test]
fn fset_can_override_special_form_name_for_direct_calls() {
    let result = eval_one(
        "(let ((orig (symbol-function 'if)))
           (unwind-protect
               (progn
                 (fset 'if (lambda (&rest _args) 'ov))
                 (if t 1 2))
             (fset 'if orig)))",
    );
    assert_eq!(result, "OK ov");
}

#[test]
fn fset_restoring_subr_object_keeps_callability() {
    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function 'car)))
               (fset 'car orig)
               (car '(1 2)))"
        ),
        "OK 1"
    );

    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function 'if)))
               (fset 'if orig)
               (if t 1 2))"
        ),
        "OK 1"
    );
}

#[test]
fn funcall_subr_object_ignores_symbol_function_rebinding() {
    // GNU Emacs byte-compiled code uses Bcar opcode which bypasses the
    // function cell entirely.  NeoVM now matches this: overriding `car`'s
    // function cell via `fset` does NOT affect direct `(car ...)` calls.
    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function 'car))
                   (snap (symbol-function 'car)))
               (unwind-protect
                   (progn
                     (fset 'car (lambda (&rest _) 'shadow))
                     (list (funcall snap '(1 2)) (car '(1 2))))
                 (fset 'car orig)))"
        ),
        "OK (1 1)"
    );
}

#[test]
fn funcall_autoload_object_signals_wrong_type_argument_symbolp() {
    assert_eq!(
        eval_one(
            "(condition-case err
                 (funcall '(autoload \"x\" nil nil nil) 3)
               (wrong-type-argument
                (list (car err)
                      (nth 1 err)
                      (and (consp (nth 2 err))
                           (eq (car (nth 2 err)) 'autoload)))))"
        ),
        "OK (wrong-type-argument symbolp t)"
    );
}

#[test]
fn apply_autoload_object_signals_wrong_type_argument_symbolp() {
    assert_eq!(
        eval_one(
            "(condition-case err
                 (apply '(autoload \"x\" nil nil nil) '(3))
               (wrong-type-argument
                (list (car err)
                      (nth 1 err)
                      (and (consp (nth 2 err))
                           (eq (car (nth 2 err)) 'autoload)))))"
        ),
        "OK (wrong-type-argument symbolp t)"
    );
}

#[test]
fn fset_nil_reports_symbol_payload_for_void_function_calls() {
    let results = eval_all(
        "(fset 'vm-fsetnil nil)
         (fboundp 'vm-fsetnil)
         (condition-case err (vm-fsetnil) (error err))
         (condition-case err (funcall 'vm-fsetnil) (error err))
         (condition-case err (apply 'vm-fsetnil nil) (error err))
         (fset 'length nil)
         (fboundp 'length)
         (condition-case err (length '(1 2)) (error err))",
    );

    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK (void-function vm-fsetnil)");
    assert_eq!(results[3], "OK (void-function vm-fsetnil)");
    assert_eq!(results[4], "OK (void-function vm-fsetnil)");
    assert_eq!(results[5], "OK nil");
    assert_eq!(results[6], "OK nil");
    assert_eq!(results[7], "OK (void-function length)");
}

#[test]
fn fset_noncallable_reports_symbol_payload_for_invalid_function_calls() {
    let results = eval_all(
        "(fset 'vm-fsetint 1)
         (fboundp 'vm-fsetint)
         (symbol-function 'vm-fsetint)
         (condition-case err (vm-fsetint) (error err))
         (condition-case err (funcall 'vm-fsetint) (error err))
         (condition-case err (apply 'vm-fsetint nil) (error err))",
    );

    assert_eq!(results[0], "OK 1");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK 1");
    assert_eq!(results[3], "OK (invalid-function vm-fsetint)");
    assert_eq!(results[4], "OK (invalid-function vm-fsetint)");
    assert_eq!(results[5], "OK (invalid-function vm-fsetint)");
}

#[test]
fn fset_t_function_cell_controls_funcall_and_apply_behavior() {
    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function 't)))
               (unwind-protect
                   (progn
                     (fset 't 'car)
                     (funcall t '(1 2)))
                 (fset 't orig)))"
        ),
        "OK 1"
    );

    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function 't)))
               (unwind-protect
                   (progn
                     (fset 't 1)
                     (condition-case err (funcall t) (error err)))
                 (fset 't orig)))"
        ),
        "OK (invalid-function t)"
    );
}

#[test]
fn fset_keyword_function_cell_controls_funcall_and_apply_behavior() {
    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function :k)))
               (unwind-protect
                   (progn
                     (fset :k 'car)
                     (funcall :k '(1 2)))
                 (fset :k orig)))"
        ),
        "OK 1"
    );

    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function :k)))
               (unwind-protect
                   (progn
                     (fset :k 'car)
                     (apply :k '((1 2))))
                 (fset :k orig)))"
        ),
        "OK 1"
    );

    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function :k)))
               (unwind-protect
                   (progn
                     (fset :k 1)
                     (condition-case err (funcall :k) (error err)))
                 (fset :k orig)))"
        ),
        "OK (invalid-function :k)"
    );
}

#[test]
fn named_call_cache_invalidates_on_function_cell_mutation() {
    let results = eval_all(
        "(condition-case err
             (funcall 'vm-cache-target)
           (error (car err)))
         (fset 'vm-cache-target (lambda () 9))
         (funcall 'vm-cache-target)
         (fset 'vm-cache-target (lambda () 11))
         (funcall 'vm-cache-target)",
    );
    assert_eq!(results[0], "OK void-function");
    assert_eq!(results[2], "OK 9");
    assert_eq!(results[4], "OK 11");
}

#[test]
fn funcall_builtin_wrong_arity_uses_subr_object_payload() {
    assert_eq!(
        eval_one("(condition-case err (car) (error (subrp (nth 1 err))))"),
        "OK nil"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall 'car) (error (subrp (nth 1 err))))"),
        "OK t"
    );
}

#[test]
fn condition_case_catches_uncaught_throw_as_no_catch() {
    assert_eq!(
        eval_one("(condition-case err (throw 'tag 42) (error (car err)))"),
        "OK no-catch"
    );
    // Test uncaught throw from a function call (not just special form).
    // Use a lambda that throws instead of exit-minibuffer (which is Elisp).
    assert_eq!(
        eval_one("(condition-case err (funcall (lambda () (throw 'exit nil))) (error (car err)))"),
        "OK no-catch"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall (lambda () (throw 'exit nil))) (no-catch err))"),
        "OK (no-catch exit nil)"
    );
}

#[test]
fn backward_compat_core_forms() {
    // Same tests as original elisp.rs
    let source = r#"
    (+ 1 2)
    (let ((x 1)) (setq x (+ x 2)) x)
    (let ((lst '(1 2))) (setcar lst 9) lst)
    (catch 'tag (throw 'tag 42))
    (condition-case e (/ 1 0) (arith-error 'div-zero))
    (let ((x 1))
      (let ((f (lambda () x)))
        (let ((x 2))
          (funcall f))))
    "#;

    let mut ev = Evaluator::new();
    let forms = parse_forms(source).expect("parse");
    let rendered: Vec<String> = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect();

    assert_eq!(
        rendered,
        vec!["OK 3", "OK 3", "OK (9 2)", "OK 42", "OK div-zero", "OK 2"]
    );
}

#[test]
fn excessive_recursion_detected() {
    let results = eval_all("(defun inf () (inf))\n(inf)");
    // Second form should trigger excessive nesting
    assert!(results[1].contains("excessive-lisp-nesting"));
}

#[test]
fn excessive_recursion_reports_overflow_depth_like_gnu_emacs() {
    let results = eval_all("(defun inf () (inf))\n(inf)");
    assert_eq!(results[1], "ERR (excessive-lisp-nesting (1601))");
}

#[test]
fn lambda_can_call_symbol_function_subr_as_first_class_value() {
    assert_eq!(
        eval_one("((lambda (orig x y) (funcall orig (+ x 1) y)) (symbol-function '+) 4 7)"),
        "OK 12"
    );
    assert_eq!(
        eval_one(
            "(apply (lambda (orig x y) (funcall orig (+ x 1) y)) (symbol-function '+) '(4 7))"
        ),
        "OK 12"
    );
}

#[test]
fn lexical_binding_closure() {
    // With lexical binding, closures capture the lexical environment
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        r#"
        (let ((x 1))
          (let ((f (lambda () x)))
            (let ((x 2))
              (funcall f))))
    "#,
    )
    .expect("parse");
    ev.set_lexical_binding(true);
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    // In lexical binding, the closure captures x=1, not x=2
    assert_eq!(result, "OK 1");
}

#[test]
fn dynamic_binding_closure() {
    // Without lexical binding (default), closures see dynamic scope
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        r#"
        (let ((x 1))
          (let ((f (lambda () x)))
            (let ((x 2))
              (funcall f))))
    "#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    // In dynamic binding, the lambda sees x=2 (innermost dynamic binding)
    assert_eq!(result, "OK 2");
}

#[test]
fn lexical_binding_special_var_stays_dynamic() {
    // defvar makes a variable special — it stays dynamically scoped
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        r#"
        (defvar my-special 10)
        (let ((my-special 20))
          (let ((f (lambda () my-special)))
            (let ((my-special 30))
              (funcall f))))
    "#,
    )
    .expect("parse");
    ev.set_lexical_binding(true);
    let results: Vec<String> = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect();
    // my-special is declared special, so even in lexical mode it's dynamic
    assert_eq!(results[1], "OK 30");
}

#[test]
fn defalias_works() {
    let results = eval_all(
        "(defun my-add (a b) (+ a b))
         (defalias 'my-plus 'my-add)
         (my-plus 3 4)",
    );
    assert_eq!(results[2], "OK 7");
}

#[test]
fn defalias_rejects_self_alias_cycle() {
    let result = eval_one(
        "(condition-case err
             (defalias 'vm-da-self 'vm-da-self)
           (error err))",
    );
    assert_eq!(result, "OK (cyclic-function-indirection vm-da-self)");
}

#[test]
fn defalias_rejects_two_node_alias_cycle() {
    let results = eval_all(
        "(defalias 'vm-da-a 'vm-da-b)
         (condition-case err
             (defalias 'vm-da-b 'vm-da-a)
           (error err))",
    );
    assert_eq!(results[0], "OK vm-da-a");
    assert_eq!(results[1], "OK (cyclic-function-indirection vm-da-b)");
}

#[test]
fn defalias_nil_signals_setting_constant() {
    let result = eval_one(
        "(condition-case err
             (defalias nil 'car)
           (error err))",
    );
    assert_eq!(result, "OK (setting-constant nil)");
}

#[test]
fn defalias_t_accepts_symbol_cell_updates() {
    let results = eval_all(
        "(defalias t 'car)
         (symbol-function t)",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK car");
}

#[test]
fn defalias_enforces_argument_count() {
    let results = eval_all(
        "(condition-case err (defalias) (error err))
         (condition-case err (defalias 'vm-da-too-few) (error err))
         (condition-case err (defalias 'vm-da-too-many 'car \"doc\" t) (error err))",
    );
    assert_eq!(results[0], "OK (wrong-number-of-arguments defalias 0)");
    assert_eq!(results[1], "OK (wrong-number-of-arguments defalias 1)");
    assert_eq!(results[2], "OK (wrong-number-of-arguments defalias 4)");
}

#[test]
fn defalias_honors_defalias_fset_function_hook() {
    let results = eval_all(
        "(setq vm-da-hook-log nil)
         (put 'vm-da-hooked 'defalias-fset-function
              (lambda (sym def)
                (setq vm-da-hook-log (list sym def))
                (fset sym def)))
         (defalias 'vm-da-hooked 'car)
         vm-da-hook-log
         (symbol-function 'vm-da-hooked)",
    );
    assert_eq!(results[2], "OK vm-da-hooked");
    assert_eq!(results[3], "OK (vm-da-hooked car)");
    assert_eq!(results[4], "OK car");
}

#[test]
fn defalias_stores_function_documentation_property() {
    let results = eval_all(
        "(defalias 'vm-da-doc (lambda () 'ok) \"vm doc\")
         (get 'vm-da-doc 'function-documentation)",
    );
    assert_eq!(results[0], "OK vm-da-doc");
    assert_eq!(results[1], "OK \"vm doc\"");
}

#[test]
fn fset_inside_lambda_uses_argument_definition() {
    assert_eq!(
        eval_one(
            "((lambda (sym def)
                (fset sym def)
                (list sym def (symbol-function sym)))
              'vm-eval-hook-lambda
              'car)"
        ),
        "OK (vm-eval-hook-lambda car car)"
    );
}

#[test]
fn compiled_literal_reader_form_is_not_callable() {
    let result = eval_one(
        "(condition-case err
             (funcall (car (read-from-string \"#[nil \\\"\\\\300\\\\207\\\" [42] 1]\")))
           (error (car err)))",
    );
    assert_eq!(result, "OK invalid-function");
}

#[test]
fn provide_require() {
    let mut ev = Evaluator::new();
    let forms = parse_forms("(provide 'my-feature) (featurep 'my-feature)").expect("parse");
    let results: Vec<String> = ev
        .eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect();
    assert_eq!(results[0], "OK my-feature");
    assert_eq!(results[1], "OK t");
}

#[test]
fn default_directory_is_bound_to_directory_path() {
    let results = eval_all(
        "(stringp default-directory)
         (file-directory-p default-directory)
         (let ((len (length default-directory)))
           (and (> len 0)
                (eq (aref default-directory (1- len)) ?/)))",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK t");
}

#[test]
fn unread_command_events_is_bound_to_nil_at_startup() {
    let results = eval_all(
        "unread-command-events
         (boundp 'unread-command-events)
         (let ((unread-command-events '(97))) unread-command-events)
         unread-command-events",
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK (97)");
    assert_eq!(results[3], "OK nil");
}

#[test]
fn startup_string_variable_docs_are_seeded_at_startup() {
    let results = eval_all(
        "(stringp (get 'kill-ring 'variable-documentation))
         (integerp (get 'kill-ring 'variable-documentation))
         (stringp (get 'kill-ring-yank-pointer 'variable-documentation))
         (integerp (get 'kill-ring-yank-pointer 'variable-documentation))
         (stringp (get 'after-init-hook 'variable-documentation))
         (integerp (get 'after-init-hook 'variable-documentation))
         (stringp (get 'Buffer-menu-buffer-list 'variable-documentation))
         (integerp (get 'Buffer-menu-buffer-list 'variable-documentation))
         (stringp (get 'Info-default-directory-list 'variable-documentation))
         (integerp (get 'Info-default-directory-list 'variable-documentation))
         (stringp (get 'auto-coding-alist 'variable-documentation))
         (integerp (get 'auto-coding-alist 'variable-documentation))
         (stringp (get 'auto-save--timer 'variable-documentation))
         (integerp (get 'auto-save--timer 'variable-documentation))
         (stringp (get 'backup-directory-alist 'variable-documentation))
         (integerp (get 'backup-directory-alist 'variable-documentation))
         (stringp (get 'before-init-hook 'variable-documentation))
         (integerp (get 'before-init-hook 'variable-documentation))
         (stringp (get 'blink-cursor-mode 'variable-documentation))
         (integerp (get 'blink-cursor-mode 'variable-documentation))
         (stringp (get 'buffer-offer-save 'variable-documentation))
         (integerp (get 'buffer-offer-save 'variable-documentation))
         (stringp (get 'buffer-quit-function 'variable-documentation))
         (integerp (get 'buffer-quit-function 'variable-documentation))
         (stringp (get 'command-line-functions 'variable-documentation))
         (integerp (get 'command-line-functions 'variable-documentation))
         (stringp (get 'comment-start 'variable-documentation))
         (integerp (get 'comment-start 'variable-documentation))
         (stringp (get 'completion-styles 'variable-documentation))
         (integerp (get 'completion-styles 'variable-documentation))
         (stringp (get 'context-menu-mode 'variable-documentation))
         (integerp (get 'context-menu-mode 'variable-documentation))
         (stringp (get 'current-input-method 'variable-documentation))
         (integerp (get 'current-input-method 'variable-documentation))
         (stringp (get 'custom-enabled-themes 'variable-documentation))
         (integerp (get 'custom-enabled-themes 'variable-documentation))
         (stringp (get 'default-input-method 'variable-documentation))
         (integerp (get 'default-input-method 'variable-documentation))
         (stringp (get 'default-korean-keyboard 'variable-documentation))
         (integerp (get 'default-korean-keyboard 'variable-documentation))
         (stringp (get 'delete-selection-mode 'variable-documentation))
         (integerp (get 'delete-selection-mode 'variable-documentation))
         (stringp (get 'display-buffer-alist 'variable-documentation))
         (integerp (get 'display-buffer-alist 'variable-documentation))
         (stringp (get 'eldoc-mode 'variable-documentation))
         (integerp (get 'eldoc-mode 'variable-documentation))
         (stringp (get 'emacs-major-version 'variable-documentation))
         (integerp (get 'emacs-major-version 'variable-documentation))
         (stringp (get 'file-name-shadow-mode 'variable-documentation))
         (integerp (get 'file-name-shadow-mode 'variable-documentation))
         (stringp (get 'fill-prefix 'variable-documentation))
         (integerp (get 'fill-prefix 'variable-documentation))
         (stringp (get 'font-lock-comment-start-skip 'variable-documentation))
         (integerp (get 'font-lock-comment-start-skip 'variable-documentation))
         (stringp (get 'font-lock-mode 'variable-documentation))
         (integerp (get 'font-lock-mode 'variable-documentation))
         (stringp (get 'global-font-lock-mode 'variable-documentation))
         (integerp (get 'global-font-lock-mode 'variable-documentation))
         (stringp (get 'grep-command 'variable-documentation))
         (integerp (get 'grep-command 'variable-documentation))
         (stringp (get 'help-window-select 'variable-documentation))
         (integerp (get 'help-window-select 'variable-documentation))
         (stringp (get 'icomplete-mode 'variable-documentation))
         (integerp (get 'icomplete-mode 'variable-documentation))
         (stringp (get 'indent-line-function 'variable-documentation))
         (integerp (get 'indent-line-function 'variable-documentation))
         (stringp (get 'input-method-history 'variable-documentation))
         (integerp (get 'input-method-history 'variable-documentation))
         (stringp (get 'isearch-mode-hook 'variable-documentation))
         (integerp (get 'isearch-mode-hook 'variable-documentation))
         (stringp (get 'jit-lock-mode 'variable-documentation))
         (integerp (get 'jit-lock-mode 'variable-documentation))
         (stringp (get 'jka-compr-load-suffixes 'variable-documentation))
         (integerp (get 'jka-compr-load-suffixes 'variable-documentation))
         (stringp (get 'keyboard-coding-system 'variable-documentation))
         (integerp (get 'keyboard-coding-system 'variable-documentation))
         (stringp (get 'kill-ring-max 'variable-documentation))
         (integerp (get 'kill-ring-max 'variable-documentation))
         (stringp (get 'line-number-mode 'variable-documentation))
         (integerp (get 'line-number-mode 'variable-documentation))
         (stringp (get 'list-buffers-directory 'variable-documentation))
         (integerp (get 'list-buffers-directory 'variable-documentation))
         (stringp (get 'lock-file-mode 'variable-documentation))
         (integerp (get 'lock-file-mode 'variable-documentation))
         (stringp (get 'mail-user-agent 'variable-documentation))
         (integerp (get 'mail-user-agent 'variable-documentation))
         (stringp (get 'menu-bar-mode-hook 'variable-documentation))
         (integerp (get 'menu-bar-mode-hook 'variable-documentation))
         (stringp (get 'minibuffer-local-completion-map 'variable-documentation))
         (integerp (get 'minibuffer-local-completion-map 'variable-documentation))
         (stringp (get 'mouse-wheel-mode 'variable-documentation))
         (integerp (get 'mouse-wheel-mode 'variable-documentation))
         (stringp (get 'next-error-function 'variable-documentation))
         (integerp (get 'next-error-function 'variable-documentation))
         (stringp (get 'package-user-dir 'variable-documentation))
         (integerp (get 'package-user-dir 'variable-documentation))
         (stringp (get 'prettify-symbols-mode 'variable-documentation))
         (integerp (get 'prettify-symbols-mode 'variable-documentation))
         (stringp (get 'previous-transient-input-method 'variable-documentation))
         (integerp (get 'previous-transient-input-method 'variable-documentation))
         (stringp (get 'process-file-side-effects 'variable-documentation))
         (integerp (get 'process-file-side-effects 'variable-documentation))
         (stringp (get 'process-menu-mode-map 'variable-documentation))
         (integerp (get 'process-menu-mode-map 'variable-documentation))
         (stringp (get 'prog-mode-map 'variable-documentation))
         (integerp (get 'prog-mode-map 'variable-documentation))
         (stringp (get 'query-about-changed-file 'variable-documentation))
         (integerp (get 'query-about-changed-file 'variable-documentation))
         (stringp (get 'read-extended-command-predicate 'variable-documentation))
         (integerp (get 'read-extended-command-predicate 'variable-documentation))
         (stringp (get 'regexp-search-ring-max 'variable-documentation))
         (integerp (get 'regexp-search-ring-max 'variable-documentation))
         (stringp (get 'safe-local-variable-values 'variable-documentation))
         (integerp (get 'safe-local-variable-values 'variable-documentation))
         (stringp (get 'selection-coding-system 'variable-documentation))
         (integerp (get 'selection-coding-system 'variable-documentation))
         (stringp (get 'show-paren-mode 'variable-documentation))
         (integerp (get 'show-paren-mode 'variable-documentation))
         (stringp (get 'tab-bar-format 'variable-documentation))
         (integerp (get 'tab-bar-format 'variable-documentation))
         (stringp (get 'tool-bar-map 'variable-documentation))
         (integerp (get 'tool-bar-map 'variable-documentation))
         (stringp (get 'transient-mark-mode-hook 'variable-documentation))
         (integerp (get 'transient-mark-mode-hook 'variable-documentation))
         (stringp (get 'user-emacs-directory 'variable-documentation))
         (integerp (get 'user-emacs-directory 'variable-documentation))
         (stringp (get 'window-size-fixed 'variable-documentation))
         (integerp (get 'window-size-fixed 'variable-documentation))
         (stringp (get 'yank-transform-functions 'variable-documentation))
         (integerp (get 'yank-transform-functions 'variable-documentation))",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK t");
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK t");
    assert_eq!(results[5], "OK nil");
    assert_eq!(results[6], "OK t");
    assert_eq!(results[7], "OK nil");
    assert_eq!(results[8], "OK t");
    assert_eq!(results[9], "OK nil");
    assert_eq!(results[10], "OK t");
    assert_eq!(results[11], "OK nil");
    assert_eq!(results[12], "OK t");
    assert_eq!(results[13], "OK nil");
    assert_eq!(results[14], "OK t");
    assert_eq!(results[15], "OK nil");
    assert_eq!(results[16], "OK t");
    assert_eq!(results[17], "OK nil");
    assert_eq!(results[18], "OK t");
    assert_eq!(results[19], "OK nil");
    assert_eq!(results[20], "OK t");
    assert_eq!(results[21], "OK nil");
    assert_eq!(results[22], "OK t");
    assert_eq!(results[23], "OK nil");
    assert_eq!(results[24], "OK t");
    assert_eq!(results[25], "OK nil");
    assert_eq!(results[26], "OK t");
    assert_eq!(results[27], "OK nil");
    assert_eq!(results[28], "OK t");
    assert_eq!(results[29], "OK nil");
    assert_eq!(results[30], "OK t");
    assert_eq!(results[31], "OK nil");
    assert_eq!(results[32], "OK t");
    assert_eq!(results[33], "OK nil");
    assert_eq!(results[34], "OK t");
    assert_eq!(results[35], "OK nil");
    assert_eq!(results[36], "OK t");
    assert_eq!(results[37], "OK nil");
    assert_eq!(results[38], "OK t");
    assert_eq!(results[39], "OK nil");
    assert_eq!(results[40], "OK t");
    assert_eq!(results[41], "OK nil");
    assert_eq!(results[42], "OK t");
    assert_eq!(results[43], "OK nil");
    assert_eq!(results[44], "OK t");
    assert_eq!(results[45], "OK nil");
    assert_eq!(results[46], "OK t");
    assert_eq!(results[47], "OK nil");
    assert_eq!(results[48], "OK t");
    assert_eq!(results[49], "OK nil");
    assert_eq!(results[50], "OK t");
    assert_eq!(results[51], "OK nil");
    assert_eq!(results[52], "OK t");
    assert_eq!(results[53], "OK nil");
    assert_eq!(results[54], "OK t");
    assert_eq!(results[55], "OK nil");
    assert_eq!(results[56], "OK t");
    assert_eq!(results[57], "OK nil");
    assert_eq!(results[58], "OK t");
    assert_eq!(results[59], "OK nil");
    assert_eq!(results[60], "OK t");
    assert_eq!(results[61], "OK nil");
    assert_eq!(results[62], "OK t");
    assert_eq!(results[63], "OK nil");
    assert_eq!(results[64], "OK t");
    assert_eq!(results[65], "OK nil");
    assert_eq!(results[66], "OK t");
    assert_eq!(results[67], "OK nil");
    assert_eq!(results[68], "OK t");
    assert_eq!(results[69], "OK nil");
    assert_eq!(results[70], "OK t");
    assert_eq!(results[71], "OK nil");
    assert_eq!(results[72], "OK t");
    assert_eq!(results[73], "OK nil");
    assert_eq!(results[74], "OK t");
    assert_eq!(results[75], "OK nil");
    assert_eq!(results[76], "OK t");
    assert_eq!(results[77], "OK nil");
    assert_eq!(results[78], "OK t");
    assert_eq!(results[79], "OK nil");
    assert_eq!(results[80], "OK t");
    assert_eq!(results[81], "OK nil");
    assert_eq!(results[82], "OK t");
    assert_eq!(results[83], "OK nil");
    assert_eq!(results[84], "OK t");
    assert_eq!(results[85], "OK nil");
    assert_eq!(results[86], "OK t");
    assert_eq!(results[87], "OK nil");
    assert_eq!(results[88], "OK t");
    assert_eq!(results[89], "OK nil");
    assert_eq!(results[90], "OK t");
    assert_eq!(results[91], "OK nil");
    assert_eq!(results[92], "OK t");
    assert_eq!(results[93], "OK nil");
    assert_eq!(results[94], "OK t");
    assert_eq!(results[95], "OK nil");
    assert_eq!(results[96], "OK t");
    assert_eq!(results[97], "OK nil");
    assert_eq!(results[98], "OK t");
    assert_eq!(results[99], "OK nil");
    assert_eq!(results[100], "OK t");
    assert_eq!(results[101], "OK nil");
    assert_eq!(results[102], "OK t");
    assert_eq!(results[103], "OK nil");
    assert_eq!(results[104], "OK t");
    assert_eq!(results[105], "OK nil");
    assert_eq!(results[106], "OK t");
    assert_eq!(results[107], "OK nil");
    assert_eq!(results[108], "OK t");
    assert_eq!(results[109], "OK nil");
    assert_eq!(results[110], "OK t");
    assert_eq!(results[111], "OK nil");
    assert_eq!(results[112], "OK t");
    assert_eq!(results[113], "OK nil");
    assert_eq!(results[114], "OK t");
    assert_eq!(results[115], "OK nil");
    assert_eq!(results[116], "OK t");
    assert_eq!(results[117], "OK nil");
    assert_eq!(results[118], "OK t");
    assert_eq!(results[119], "OK nil");
    assert_eq!(results[120], "OK t");
    assert_eq!(results[121], "OK nil");
    assert_eq!(results[122], "OK t");
    assert_eq!(results[123], "OK nil");
    assert_eq!(results[124], "OK t");
    assert_eq!(results[125], "OK nil");
    assert_eq!(results[126], "OK t");
    assert_eq!(results[127], "OK nil");
    assert_eq!(results[128], "OK t");
    assert_eq!(results[129], "OK nil");
}

#[test]
fn startup_variable_documentation_property_counts_match_oracle_snapshot() {
    let results = eval_all(
        "(list
          (let ((n 0))
            (mapatoms
             (lambda (s)
               (let ((d (get s 'variable-documentation)))
                 (when (integerp d) (setq n (1+ n))))))
            n)
          (let ((n 0))
            (mapatoms
             (lambda (s)
               (let ((d (get s 'variable-documentation)))
                 (when (stringp d) (setq n (1+ n))))))
            n))",
    );
    assert_eq!(results[0], "OK (761 1902)");
}

#[test]
fn startup_variable_documentation_runtime_resolution_counts_match_oracle_snapshot() {
    let results = eval_all(
        "(list
          (let ((n 0))
            (mapatoms
             (lambda (s)
               (let ((d (get s 'variable-documentation)))
                 (when (and (integerp d)
                            (stringp (documentation-property s 'variable-documentation t)))
                   (setq n (1+ n))))))
            n)
          (let ((n 0))
            (mapatoms
             (lambda (s)
               (let ((d (get s 'variable-documentation)))
                 (when (and (stringp d)
                            (stringp (documentation-property s 'variable-documentation t)))
                   (setq n (1+ n))))))
            n))",
    );
    assert_eq!(results[0], "OK (761 1902)");
}

#[test]
fn features_variable_controls_featurep_and_require() {
    let results = eval_all(
        "(setq features '(vm-existing))
         (featurep 'vm-existing)
         (require 'vm-existing)",
    );
    assert_eq!(results[0], "OK (vm-existing)");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK vm-existing");
}

#[test]
fn require_accepts_nil_filename_as_feature_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("vm-require-nil.el"),
        "(provide 'vm-require-nil)\n",
    )
    .expect("write require fixture");

    let escaped = dir
        .path()
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let script = format!(
        "(progn (setq load-path (cons \"{}\" load-path)) 'ok)\n\
         (require 'vm-require-nil nil)\n\
         (featurep 'vm-require-nil)",
        escaped
    );
    let results = eval_all(&script);

    assert_eq!(results[1], "OK vm-require-nil");
    assert_eq!(results[2], "OK t");
}

#[test]
fn provide_preserves_features_variable_entries() {
    let results = eval_all(
        "(setq features '(vm-existing))
         (provide 'vm-new)
         features",
    );
    assert_eq!(results[0], "OK (vm-existing)");
    assert_eq!(results[1], "OK vm-new");
    assert_eq!(results[2], "OK (vm-new vm-existing)");
}

#[test]
fn require_recursive_cycle_returns_immediately() {
    // Official Emacs treats recursive require as a no-op, returning
    // the feature symbol immediately rather than signaling an error.
    // This supports circular dependencies like dired ↔ dired-aux.
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-require-recursive-{unique}"));
    fs::create_dir_all(&dir).expect("create fixture dir");
    fs::write(
        dir.join("vm-rec-a.el"),
        "(require 'vm-rec-b)\n(provide 'vm-rec-a)\n",
    )
    .expect("write vm-rec-a");
    fs::write(
        dir.join("vm-rec-b.el"),
        "(require 'vm-rec-a)\n(provide 'vm-rec-b)\n",
    )
    .expect("write vm-rec-b");

    let escaped = dir
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let script = format!(
        "(progn (setq load-path (cons \"{}\" load-path)) 'ok)\n\
         (require 'vm-rec-a)\n\
         (featurep 'vm-rec-a)\n\
         (featurep 'vm-rec-b)",
        escaped
    );
    let results = eval_all(&script);
    // Recursive require returns immediately; both features get provided
    assert_eq!(results[1], "OK vm-rec-a");
    assert_eq!(results[2], "OK t");
    assert_eq!(results[3], "OK t");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn dotimes_loop() {
    let result = eval_one("(let ((sum 0)) (dotimes (i 5) (setq sum (+ sum i))) sum)");
    assert_eq!(result, "OK 10"); // 0+1+2+3+4 = 10
}

#[test]
fn dolist_loop() {
    let result =
        eval_one("(let ((result nil)) (dolist (x '(a b c)) (setq result (cons x result))) result)");
    assert_eq!(result, "OK (c b a)");
}

#[test]
fn ignore_errors_catches_signal() {
    let result = bootstrap_eval_one("(ignore-errors (/ 1 0) 42)");
    assert_eq!(result, "OK nil"); // error caught, returns nil
}

#[test]
fn math_functions() {
    assert_eq!(eval_one("(expt 2 10)"), "OK 1024");
    assert_eq!(eval_one("(sqrt 4.0)"), "OK 2.0");
}

#[test]
fn hook_system() {
    let results = bootstrap_eval_all(
        "(defvar my-hook nil)
         (defun hook-fn () 42)
         (add-hook 'my-hook 'hook-fn)
         (list (run-hooks 'my-hook)
               my-hook
               (subrp (symbol-function 'add-hook))
               (subrp (symbol-function 'remove-hook))
               (subrp (symbol-function 'run-mode-hooks)))",
    );
    assert_eq!(results[3], "OK (nil (hook-fn) nil nil nil)");
}

#[test]
fn hook_system_runtime_value_shapes() {
    let results = eval_all(
        "(setq hook-count 0)
         (defun hook-inc () (setq hook-count (1+ hook-count)))
         (setq hook-probe-hook 'hook-inc)
         (condition-case err (run-hooks 'hook-probe-hook) (error err))
         hook-count
         (setq hook-count 0)
         (setq hook-probe-hook (cons 'hook-inc 1))
         (condition-case err (run-hooks 'hook-probe-hook) (error err))
         hook-count
         (setq hook-probe-hook t)
         (condition-case err (run-hooks 'hook-probe-hook) (error err))
         (setq hook-probe-hook 42)
         (condition-case err (run-hooks 'hook-probe-hook) (error err))
         (setq hook-probe-hook '(t hook-inc))
         (setq hook-count 0)
         (condition-case err (run-hooks 'hook-probe-hook) (error err))
         hook-count",
    );
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK 1");
    assert_eq!(results[7], "OK nil");
    assert_eq!(results[8], "OK 1");
    assert_eq!(results[10], "OK (void-function t)");
    assert_eq!(results[12], "OK (invalid-function 42)");
    assert_eq!(results[15], "OK nil");
    assert_eq!(results[16], "OK 2");
}

#[test]
fn run_hook_with_args_runtime_value_shapes() {
    let results = eval_all(
        "(setq hook-log nil)
         (defun hook-log-fn (&rest args) (setq hook-log (cons args hook-log)))
         (setq hook-probe-hook 'hook-log-fn)
         (condition-case err (run-hook-with-args 'hook-probe-hook 1 2) (error err))
         hook-log
         (setq hook-log nil)
         (setq hook-probe-hook (cons 'hook-log-fn 1))
         (condition-case err (run-hook-with-args 'hook-probe-hook 3) (error err))
         hook-log
         (setq hook-probe-hook t)
         (condition-case err (run-hook-with-args 'hook-probe-hook 4) (error err))
         (setq hook-probe-hook 42)
         (condition-case err (run-hook-with-args 'hook-probe-hook 5) (error err))
         (setq hook-log nil)
         (setq hook-probe-hook '(t hook-log-fn))
         (condition-case err (run-hook-with-args 'hook-probe-hook 6) (error err))
         hook-log",
    );
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK ((1 2))");
    assert_eq!(results[7], "OK nil");
    assert_eq!(results[8], "OK ((3))");
    assert_eq!(results[10], "OK (void-function t)");
    assert_eq!(results[12], "OK (invalid-function 42)");
    assert_eq!(results[15], "OK nil");
    assert_eq!(results[16], "OK ((6) (6))");
}

#[test]
fn symbol_operations() {
    let results = eval_all(
        "(defvar x 42)
         (boundp 'x)
         (symbol-value 'x)
         (put 'x 'doc \"A variable\")
         (get 'x 'doc)",
    );
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK 42");
    assert_eq!(results[4], r#"OK "A variable""#);
}

// -- Buffer operations -------------------------------------------------

#[test]
fn buffer_create_and_switch() {
    let results = eval_all(
        "(get-buffer-create \"test-buf\")
         (set-buffer \"test-buf\")
         (buffer-name)
         (bufferp (current-buffer))",
    );
    assert!(results[0].starts_with("OK #<buffer"));
    assert!(results[1].starts_with("OK #<buffer"));
    assert_eq!(results[2], r#"OK "test-buf""#);
    assert_eq!(results[3], "OK t");
}

#[test]
fn buffer_insert_and_point() {
    let results = eval_all(
        "(get-buffer-create \"ed\")
         (set-buffer \"ed\")
         (insert \"hello\")
         (point)
         (goto-char 1)
         (point)
         (buffer-string)
         (point-min)
         (point-max)",
    );
    assert_eq!(results[3], "OK 6"); // after inserting "hello", point is 6 (1-based)
    assert_eq!(results[5], "OK 1"); // after goto-char 1
    assert_eq!(results[6], r#"OK "hello""#);
    assert_eq!(results[7], "OK 1"); // point-min
    assert_eq!(results[8], "OK 6"); // point-max
}

#[test]
fn buffer_delete_region() {
    let results = eval_all(
        "(get-buffer-create \"del\")
         (set-buffer \"del\")
         (insert \"abcdef\")
         (delete-region 2 5)
         (buffer-string)",
    );
    assert_eq!(results[4], r#"OK "aef""#);
}

#[test]
fn buffer_delete_and_extract_region_accepts_live_markers_after_insertions() {
    let results = eval_all(
        "(with-temp-buffer
           (insert \"abcdef\")
           (let ((start (copy-marker 2))
                 (end (copy-marker 5 t)))
             (goto-char 1)
             (insert \"X\")
             (list (delete-and-extract-region start end)
                   (buffer-string))))",
    );
    assert_eq!(results[0], r#"OK ("bcd" "Xaef")"#);
}

#[test]
fn buffer_erase() {
    let results = eval_all(
        "(get-buffer-create \"era\")
        (set-buffer \"era\")
         (insert \"stuff\")
         (erase-buffer)
         (buffer-string)
         (buffer-size)",
    );
    assert_eq!(results[4], r#"OK """#);
    assert_eq!(results[5], "OK 0");
}

#[test]
fn buffer_mutation_read_only_shape_matches_gnu() {
    let results = eval_all(
        "(list
           (with-temp-buffer
             (insert \"abc\")
             (setq buffer-read-only t)
             (condition-case err
                 (delete-region 1 2)
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (insert \"abc\")
             (setq buffer-read-only t)
             (condition-case err
                 (delete-and-extract-region 1 2)
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (insert \"abc\")
             (setq buffer-read-only t)
             (condition-case err
                 (erase-buffer)
               (error (list (car err) (bufferp (car (cdr err))))))))",
    );
    assert_eq!(
        results[0],
        "OK ((buffer-read-only t) (buffer-read-only t) (buffer-read-only t))"
    );
}

#[test]
fn buffer_mutation_read_only_noop_cases_match_gnu() {
    let results = eval_all(
        "(list
           (with-temp-buffer
             (setq buffer-read-only t)
             (delete-region 1 1))
           (with-temp-buffer
             (setq buffer-read-only t)
             (delete-and-extract-region 1 1))
           (with-temp-buffer
             (narrow-to-region 1 1)
             (setq buffer-read-only t)
             (erase-buffer)
             (list (point-min) (point-max) (buffer-string))))",
    );
    assert_eq!(results[0], r#"OK (nil "" (1 1 ""))"#);
}

#[test]
fn buffer_narrowing() {
    let results = eval_all(
        "(get-buffer-create \"nar\")
         (set-buffer \"nar\")
         (insert \"hello world\")
         (narrow-to-region 7 12)
         (buffer-string)
         (widen)
         (buffer-string)",
    );
    assert_eq!(results[4], r#"OK "world""#);
    assert_eq!(results[6], r#"OK "hello world""#);
}

#[test]
fn buffer_narrowing_accepts_live_marker_bounds_after_insertions() {
    let results = eval_all(
        "(with-temp-buffer
           (insert \"abcdef\")
           (let ((start (copy-marker 2))
                 (end (copy-marker 5 t)))
             (goto-char 1)
             (insert \"X\")
             (narrow-to-region start end)
             (list (point-min) (point-max) (buffer-string))))",
    );
    assert_eq!(results[0], r#"OK (3 6 "bcd")"#);
}

#[test]
fn buffer_modified_p() {
    let results = eval_all(
        "(get-buffer-create \"mod\")
         (set-buffer \"mod\")
         (buffer-modified-p)
         (insert \"x\")
         (buffer-modified-p)
         (set-buffer-modified-p nil)
         (buffer-modified-p)",
    );
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[4], "OK t");
    assert_eq!(results[6], "OK nil");
}

#[test]
fn buffer_mark() {
    let results = bootstrap_eval_all(
        "(get-buffer-create \"mk\")
         (set-buffer \"mk\")
         (insert \"hello\")
         (set-mark 3)
         (mark)",
    );
    assert_eq!(results[4], "OK 3");
}

#[test]
fn buffer_with_current_buffer() {
    let results = eval_all(
        "(get-buffer-create \"a\")
         (get-buffer-create \"b\")
         (set-buffer \"a\")
         (insert \"in-a\")
         (with-current-buffer \"b\"
           (insert \"in-b\")
           (buffer-string))
         (buffer-name)
         (buffer-string)",
    );
    // with-current-buffer should switch to b, insert, get string, then restore a
    assert_eq!(results[4], r#"OK "in-b""#);
    assert_eq!(results[5], r#"OK "a""#); // current buffer restored
    assert_eq!(results[6], r#"OK "in-a""#); // a's content unchanged
}

#[test]
fn buffer_save_excursion() {
    let results = eval_all(
        "(get-buffer-create \"se\")
         (set-buffer \"se\")
         (insert \"abcdef\")
         (goto-char 3)
         (save-excursion
           (goto-char 1)
           (insert \"X\"))
         (point)",
    );
    // save-excursion restores point to the marker, which shifted from 3 to 4
    // because "X" was inserted before it at position 1.
    assert_eq!(results[5], "OK 4");
}

#[test]
fn buffer_save_excursion_tracks_marker_through_edits() {
    let results = eval_all(
        "(with-temp-buffer
           (insert \"0123456789\")
           (goto-char 6)
           (let ((before-point (point)))
             (save-excursion
               (goto-char 3)
               (insert \"XXX\")
               (goto-char 12)
               (delete-char 2))
             (list before-point (point) (buffer-string))))",
    );
    assert_eq!(results[0], "OK (6 9 \"01XXX234567\")");
}

#[test]
fn insert_before_markers_advances_before_markers_at_point() {
    let results = eval_all(
        "(with-temp-buffer
           (insert \"ab\")
           (goto-char 1)
           (let ((m (copy-marker (point))))
             (insert-before-markers \"X\")
             (list (buffer-string) (marker-position m))))",
    );
    assert_eq!(results[0], r#"OK ("Xab" 2)"#);
}

#[test]
fn insert_read_only_shape_and_noop_cases_match_gnu() {
    let results = eval_all(
        "(list
           (with-temp-buffer
             (setq buffer-read-only t)
             (condition-case err
                 (insert \"x\")
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (setq buffer-read-only t)
             (condition-case err
                 (insert-char ?x 1)
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (setq buffer-read-only t)
             (condition-case err
                 (insert-and-inherit \"x\")
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (setq buffer-read-only t)
             (condition-case err
                 (insert-before-markers-and-inherit \"x\")
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (setq buffer-read-only t)
             (condition-case err
                 (insert-byte 120 1)
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (setq buffer-read-only t)
             (list (insert)
                   (insert \"\")
                   (insert-char ?x 0)
                   (insert-byte 120 0)
                   (insert-and-inherit)
                   (insert-and-inherit \"\")
                   (insert-before-markers-and-inherit)
                   (insert-before-markers-and-inherit \"\")
                   (buffer-string))))",
    );
    assert_eq!(
        results[0],
        r#"OK ((buffer-read-only t) (buffer-read-only t) (buffer-read-only t) (buffer-read-only t) (buffer-read-only t) (nil nil nil nil nil nil nil nil ""))"#
    );
}

#[test]
fn lexical_inhibit_read_only_binding_overrides_buffer_read_only() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    let forms = parse_forms(
        "(with-temp-buffer
           (setq buffer-read-only t)
           (let ((inhibit-read-only t))
             (insert \"ok\")
             (buffer-string)))",
    )
    .expect("parse");
    let result = ev.eval_expr(&forms[0]);
    assert_eq!(format_eval_result(&result), r#"OK "ok""#);
}

#[test]
fn bootstrap_display_warning_does_not_signal_buffer_read_only() {
    let result = bootstrap_eval_one(
        "(condition-case err
             (progn
               (display-warning 'emacs \"hello from neomacs startup\")
               'ok)
           (error (list 'error (car err))))",
    );
    assert_eq!(result, "OK ok");
}

#[test]
fn insert_char_nil_count_defaults_to_one_with_inherit() {
    let results = eval_all(
        "(with-temp-buffer
           (insert \"ab\")
           (put-text-property 2 3 'face 'bold)
           (insert-char ?X nil t)
           (list (buffer-substring-no-properties (point-min) (point-max))
                 (get-text-property 3 'face)))",
    );
    assert_eq!(results[0], r#"OK ("abX" bold)"#);
}

#[test]
fn insert_inherit_variants_match_gnu_property_and_marker_semantics() {
    let results = eval_all(
        "(list
           (with-temp-buffer
             (insert \"a\")
             (put-text-property 1 2 'face 'bold)
             (insert-and-inherit (propertize \"X\" 'face 'italic 'mouse-face 'highlight))
             (list (buffer-substring-no-properties (point-min) (point-max))
                   (get-text-property 2 'face)
                   (get-text-property 2 'mouse-face)))
           (with-temp-buffer
             (insert \"ab\")
             (put-text-property 1 2 'face 'bold)
             (goto-char 2)
             (let ((m (copy-marker (point))))
               (insert-before-markers-and-inherit
                (propertize \"X\" 'mouse-face 'highlight))
               (list (buffer-substring-no-properties (point-min) (point-max))
                     (marker-position m)
                     (get-text-property 2 'face)
                     (get-text-property 2 'mouse-face)))))",
    );
    assert_eq!(
        results[0],
        r#"OK (("aX" bold highlight) ("aXb" 3 bold highlight))"#
    );
}

#[test]
fn insert_buffer_substring_preserves_source_text_properties() {
    assert_eq!(
        eval_one(
            r#"(let ((src (get-buffer-create "*eval-sub-src*"))
                     (dst (get-buffer-create "*eval-sub-dst*")))
                 (with-current-buffer src
                   (erase-buffer)
                   (insert "abcXYZ")
                   (put-text-property 2 5 'face 'bold))
                 (set-buffer dst)
                 (erase-buffer)
                 (insert-buffer-substring src 2 5)
                 (let ((sub (with-current-buffer src
                              (buffer-substring 2 5)))
                       (copied (buffer-string)))
                   (list sub
                         (get-text-property 1 'face sub)
                         copied
                         (get-text-property 1 'face copied))))"#,
        ),
        r#"OK (#("bcX" 0 3 (face bold)) bold #("bcX" 0 3 (face bold)) bold)"#
    );
}

#[test]
fn compare_buffer_substrings_respects_case_fold_search() {
    assert_eq!(
        eval_one(
            r#"(let ((left (get-buffer-create "*eval-cmp-left*"))
                     (right (get-buffer-create "*eval-cmp-right*")))
                 (with-current-buffer left
                   (erase-buffer)
                   (insert "Abc"))
                 (with-current-buffer right
                   (erase-buffer)
                   (insert "aBc"))
                 (list
                  (let ((case-fold-search nil))
                    (compare-buffer-substrings left nil nil right nil nil))
                  (let ((case-fold-search t))
                    (compare-buffer-substrings left nil nil right nil nil))
                  (let ((case-fold-search t))
                    (compare-buffer-substrings left 1 2 right 1 3))))"#,
        ),
        "OK (-1 0 -2)"
    );
}

#[test]
fn field_builtins_match_gnu_property_boundary_semantics() {
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (list
                  (progn
                    (insert "abcdefg")
                    (put-text-property 2 5 'field 'left)
                    (put-text-property 5 8 'field 'right)
                    (put-text-property 2 5 'face 'bold)
                    (let ((s (field-string 3)))
                      (list
                       (list (field-beginning 3)
                             (field-end 3)
                             (field-string-no-properties 3))
                       (get-text-property 1 'face s)
                       (list (field-beginning 5)
                             (field-beginning 5 t)
                             (field-end 5)
                             (field-end 5 t))
                       (progn
                         (delete-field 3)
                         (list
                          (buffer-substring-no-properties (point-min) (point-max))
                          (get-text-property 2 'field))))))
                  (progn
                    (erase-buffer)
                    (insert "abcdefg")
                    (put-text-property 2 4 'field 'left)
                    (put-text-property 4 5 'field 'boundary)
                    (put-text-property 5 8 'field 'right)
                    (list (field-beginning 4)
                          (field-beginning 4 t)
                          (field-end 4)
                          (field-end 4 t)
                          (field-beginning 5)
                          (field-beginning 5 t)
                          (field-end 5)
                          (field-end 5 t)))))"#,
        ),
        r#"OK (((2 5 "bcd") bold (2 2 5 8) ("aefg" right)) (2 2 4 8 4 2 5 8))"#
    );
}

#[test]
fn constrain_to_field_matches_gnu_boundary_and_capture_semantics() {
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (list
                  (progn
                    (insert "abcdefg")
                    (put-text-property 2 5 'field 'left)
                    (put-text-property 5 8 'field 'right)
                    (put-text-property 3 4 'capture t)
                    (goto-char 7)
                    (list
                     (constrain-to-field 7 3)
                     (constrain-to-field 7 5)
                     (constrain-to-field 7 5 t)
                     (progn
                       (goto-char 7)
                       (list (constrain-to-field nil 3) (point)))
                     (constrain-to-field 7 3 nil nil 'capture)
                     (constrain-to-field 7 2 nil nil 'capture)))
                  (progn
                    (erase-buffer)
                    (insert "ab\ncd\nef")
                    (put-text-property 1 4 'field 'top)
                    (put-text-property 4 9 'field 'bottom)
                    (list
                     (constrain-to-field 6 2 nil t)
                     (constrain-to-field 6 2 nil nil)
                     (constrain-to-field 6 4 t nil)))))"#,
        ),
        r#"OK ((5 5 7 (5 5) 5 2) (4 4 6))"#
    );
}

#[test]
fn replace_region_contents_preserves_source_properties_and_rejects_self_buffer() {
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (let ((src (get-buffer-create "*rrc-src*"))
                       (s (propertize "CD" 'face 'bold)))
                   (with-current-buffer src
                     (erase-buffer)
                     (insert "1234")
                     (put-text-property 2 4 'face 'italic))
                   (list
                    (progn
                      (erase-buffer)
                      (insert "abZZef")
                      (replace-region-contents 3 5 s)
                      (list
                       (buffer-substring-no-properties 1 (point-max))
                       (get-text-property 3 'face)))
                    (progn
                      (erase-buffer)
                      (insert "abZZef")
                      (replace-region-contents 3 5 (vector src 2 4))
                      (list
                       (buffer-substring-no-properties 1 (point-max))
                       (get-text-property 3 'face)
                       (get-text-property 4 'face)))
                    (condition-case err
                        (replace-region-contents 3 5 (current-buffer))
                      (error (list (car err) (car (cdr err))))))))"#,
        ),
        r#"OK (("abCDef" bold) ("ab23ef" italic italic) (error "Cannot replace a buffer with itself"))"#
    );
}

#[test]
fn subst_char_in_region_read_only_shape_and_noop_cases_match_gnu() {
    let results = eval_all(
        "(list
           (with-temp-buffer
             (insert \"abc\")
             (setq buffer-read-only t)
             (condition-case err
                 (subst-char-in-region 1 2 ?a ?b)
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (insert \"abc\")
             (setq buffer-read-only t)
             (list (subst-char-in-region 1 1 ?a ?b)
                   (subst-char-in-region 1 4 ?z ?b)
                   (buffer-substring-no-properties (point-min) (point-max)))))",
    );
    assert_eq!(results[0], r#"OK ((buffer-read-only t) (nil nil "abc"))"#);
}

#[test]
fn buffer_undo_list_reflects_recorded_edits() {
    let results = eval_all(
        "(with-temp-buffer
           (setq buffer-undo-list nil)
           (insert \"Hello\")
           (let ((after-insert (not (null buffer-undo-list))))
             (undo-boundary)
             (insert \" World\")
             (undo-boundary)
             (delete-region 1 6)
             (undo-boundary)
             (list after-insert
                   (not (null buffer-undo-list))
                   buffer-undo-list)))",
    );
    assert_eq!(
        results[0],
        "OK (t t (nil (\"Hello\" . 1) 12 nil (6 . 12) nil (1 . 6) (t . 0)))"
    );
}

#[test]
fn char_primitives_respect_narrowing() {
    let results = eval_all(
        "(with-temp-buffer
           (insert \"Hello, 世界\")
           (narrow-to-region 3 8)
           (goto-char (point-min))
           (list (following-char)
                 (preceding-char)
                 (char-after (point-min))
                 (char-before (point-min))))",
    );
    assert_eq!(results[0], "OK (108 0 108 nil)");
}

#[test]
fn delete_char_respects_narrowing_boundaries() {
    let results = eval_all(
        "(with-temp-buffer
           (insert \"abc\")
           (narrow-to-region 1 2)
           (list (progn
                   (goto-char (point-max))
                   (condition-case err
                       (delete-char 1)
                     (error (car err))))
                 (progn
                   (goto-char (point-min))
                   (condition-case err
                       (delete-char -1)
                     (error (car err))))))",
    );
    assert_eq!(results[0], "OK (end-of-buffer beginning-of-buffer)");
}

#[test]
fn navigation_predicates_and_line_positions_respect_narrowing() {
    let results = eval_all(
        "(with-temp-buffer
           (insert \"wx\nab\ncd\")
           (narrow-to-region 4 6)
           (goto-char (point-min))
           (list (list (bobp) (eobp) (bolp) (eolp)
                       (line-beginning-position) (line-end-position))
                 (progn
                   (goto-char (point-max))
                   (list (bobp) (eobp) (bolp) (eolp)
                         (line-beginning-position) (line-end-position)))))",
    );
    assert_eq!(results[0], "OK ((t nil t nil 4 6) (nil t nil t 4 6))");
}

#[test]
fn line_position_optional_argument_matches_gnu_current_rules() {
    let results = eval_all(
        "(with-temp-buffer
           (insert \"a\nbb\nccc\")
           (goto-char 2)
           (list (line-beginning-position 2)
                 (line-end-position 2)
                 (line-beginning-position 3)
                 (line-end-position 3)))",
    );
    assert_eq!(results[0], "OK (3 5 6 9)");
}

#[test]
fn save_match_data_restores_after_success_and_error() {
    let results = bootstrap_eval_all(
        "(set-match-data '(1 2))
         (save-match-data (set-match-data '(3 4)) (match-data))
         (match-data)
         (condition-case err
             (save-match-data
               (set-match-data '(5 6))
               (signal 'error '(\"boom\")))
           (error (car err)))
         (match-data)",
    );
    assert_eq!(results[1], "OK (3 4)");
    assert_eq!(results[2], "OK (1 2)");
    assert_eq!(results[3], "OK error");
    assert_eq!(results[4], "OK (1 2)");
}

#[test]
fn save_mark_and_excursion_restores_mark_and_mark_active() {
    let results = bootstrap_eval_all(
        "(save-current-buffer
           (let ((b (get-buffer-create \"smx-eval\")))
             (set-buffer b)
             (erase-buffer)
             (insert \"abcdef\")
             (goto-char 2)
             (set-mark 5)
             (setq mark-active nil)
             (let ((before (list (point) (mark) mark-active)))
               (save-mark-and-excursion
                 (goto-char 4)
                 (set-mark 3)
                 (setq mark-active t))
               (list before (point) (mark) mark-active))))",
    );
    assert_eq!(results[0], "OK ((2 5 nil) 2 5 nil)");
}

#[test]
fn save_window_excursion_restores_selected_window_on_success_and_error() {
    let results = bootstrap_eval_all(
        "(let ((w1 (selected-window))
               (w2 (split-window)))
           (prog1
               (list
                (save-window-excursion
                  (select-window w2)
                  (eq (selected-window) w2))
                (eq (selected-window) w1))
             (ignore-errors (delete-window w2))))
         (let ((w1 (selected-window))
               (w2 (split-window)))
           (prog1
               (list
                (condition-case err
                    (save-window-excursion
                      (select-window w2)
                      (error \"boom\"))
                  (error (car err)))
                (eq (selected-window) w1))
             (ignore-errors (delete-window w2))))",
    );
    assert_eq!(results[0], "OK (t t)");
    assert_eq!(results[1], "OK (error t)");
}

#[test]
fn save_window_excursion_restores_window_layout_after_split() {
    let results = eval_all(
        "(let ((before (length (window-list))))
           (list
            (save-window-excursion
              (split-window-internal (selected-window) nil nil nil)
              (length (window-list)))
            (length (window-list))
            before))",
    );
    assert_eq!(results[0], "OK (2 1 1)");
}

#[test]
fn save_window_excursion_restores_selected_window_point_and_requests_final_redisplay() {
    let mut ev = Evaluator::new();
    let buffer_id = ev.buffers.create_buffer("*scratch*");
    ev.buffers.set_current(buffer_id);
    ev.buffers
        .get_mut(buffer_id)
        .expect("scratch buffer")
        .insert("0123456789abcdefghijklmnopqrstuvwxyz");
    ev.frames.create_frame("F1", 960, 640, buffer_id);

    let redisplayed_points = Rc::new(RefCell::new(Vec::new()));
    let redisplayed_points_in_cb = Rc::clone(&redisplayed_points);
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Evaluator| {
        let point = crate::emacs_core::window_cmds::builtin_window_point(ev, vec![])
            .expect("window-point during redisplay");
        let Value::Int(point) = point else {
            panic!("window-point should produce an integer during redisplay, got {point:?}");
        };
        redisplayed_points_in_cb.borrow_mut().push(point);
    }));

    let forms = parse_forms(
        "(save-window-excursion
           (set-window-point (selected-window) 10)
           (redisplay))",
    )
    .expect("parse save-window-excursion redisplay form");
    ev.eval_expr(&forms[0])
        .expect("save-window-excursion should evaluate");

    assert_eq!(*redisplayed_points.borrow(), vec![10, 37]);
}

#[test]
fn current_window_configuration_saves_selected_window_live_point() {
    let mut ev = Evaluator::new();
    let buffer_id = ev.buffers.create_buffer("*scratch*");
    ev.buffers.set_current(buffer_id);
    ev.buffers
        .get_mut(buffer_id)
        .expect("scratch buffer")
        .insert("0123456789abcdefghijklmnopqrstuvwxyz");
    ev.frames.create_frame("F1", 960, 640, buffer_id);

    let forms = parse_forms(
        "(let* ((w (selected-window))
                (_ (goto-char 10))
                (cfg (current-window-configuration)))
           (goto-char 3)
           (set-window-configuration cfg)
           (list (window-point w) (point)))",
    )
    .expect("parse current-window-configuration point preservation form");

    let result = ev
        .eval_expr(&forms[0])
        .expect("current-window-configuration round-trip should evaluate");
    assert_eq!(result, Value::list(vec![Value::Int(10), Value::Int(10)]));
}

#[test]
fn save_selected_window_restores_selected_window_on_success_and_error() {
    let results = bootstrap_eval_all(
        "(let ((w1 (selected-window))
               (w2 (split-window)))
           (prog1
               (list
                (save-selected-window
                  (select-window w2)
                  (eq (selected-window) w2))
                (eq (selected-window) w1))
             (ignore-errors (delete-window w2))))
         (let ((w1 (selected-window))
               (w2 (split-window)))
           (prog1
               (list
                (condition-case err
                    (save-selected-window
                      (select-window w2)
                      (error \"boom\"))
                  (error (car err)))
                (eq (selected-window) w1))
             (ignore-errors (delete-window w2))))",
    );
    assert_eq!(results[0], "OK (t t)");
    assert_eq!(results[1], "OK (error t)");
}

#[test]
fn alist_get_comes_from_gnu_subr_runtime() {
    let results = bootstrap_eval_all(
        r#"(let ((foo '((a . 1) (b . 2))))
             (list
              (alist-get 'a foo)
              (alist-get 'z foo 'missing)
              (progn
                (setf (alist-get 'c foo) 3)
                foo)))"#,
    );
    assert_eq!(results[0], "OK (1 missing ((c . 3) (a . 1) (b . 2)))");
}

#[test]
fn with_local_quit_catches_quit_and_sets_quit_flag() {
    let results = bootstrap_eval_all(
        "(setq quit-flag nil)
         (with-local-quit
           (keyboard-quit)
           'after)
         quit-flag
         (setq quit-flag nil)
         (condition-case err
             (with-local-quit (error \"boom\"))
           (error (car err)))
         quit-flag
         (let ((inhibit-quit t)
               (quit-flag nil))
           (with-local-quit (keyboard-quit))
           (list inhibit-quit quit-flag))",
    );
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK t");
    assert_eq!(results[4], "OK error");
    assert_eq!(results[5], "OK nil");
    assert_eq!(results[6], "OK (t t)");
}

#[test]
fn with_temp_message_accepts_min_arity_and_runs_body() {
    let results = bootstrap_eval_all(
        "(with-temp-message nil 42)
         (with-temp-message \"tmp\" 7)
         (condition-case err
             (with-temp-message)
           (error (car err)))",
    );
    assert_eq!(results[0], "OK 42");
    assert_eq!(results[1], "OK 7");
    assert_eq!(results[2], "OK wrong-number-of-arguments");
}

#[test]
fn with_demoted_errors_runtime_semantics() {
    let results = bootstrap_eval_all(
        "(fboundp 'with-demoted-errors)
         (macrop 'with-demoted-errors)
         (with-demoted-errors \"DM %S\" (+ 1 2))
         (condition-case err
             (with-demoted-errors \"DM %S\" (/ 1 0))
           (error (list :error (car err) (cdr err))))
         (condition-case err
             (with-demoted-errors 1 (/ 1 0))
           (error (list :error (car err) (cdr err))))
         (with-demoted-errors \"DM %S\")
         (condition-case err
             (with-demoted-errors)
           (error err))",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK 3");
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK nil");
    assert_eq!(results[5], r#"OK "DM %S""#);
    assert_eq!(results[6], "OK (wrong-number-of-arguments (1 . 1) 0)");
}

#[test]
fn buffer_char_after_before() {
    let results = eval_all(
        "(get-buffer-create \"cb\")
         (set-buffer \"cb\")
         (insert \"abc\")
         (goto-char 2)
         (char-after)
         (char-before)",
    );
    assert_eq!(results[4], "OK 98"); // ?b = 98
    assert_eq!(results[5], "OK 97"); // ?a = 97
}

#[test]
fn buffer_list_and_kill() {
    let results = eval_all(
        "(get-buffer-create \"kill-me\")
         (kill-buffer \"kill-me\")
         (get-buffer \"kill-me\")",
    );
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK nil");
}

#[test]
fn buffer_generate_new_buffer() {
    let results = eval_all_with_subr(
        "(buffer-name (generate-new-buffer \"test\"))
         (buffer-name (generate-new-buffer \"test\"))",
    );
    assert_eq!(results[0], r#"OK "test""#);
    assert_eq!(results[1], r#"OK "test<2>""#);
}

#[test]
fn fillarray_string_writeback_updates_symbol_binding() {
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (fillarray s ?x) s)");
    assert_eq!(result, r#"OK "xxx""#);
}

#[test]
fn fillarray_alias_string_writeback_updates_symbol_binding() {
    let result = eval_one(
        "(progn
            (defalias 'vm-fillarray-alias 'fillarray)
            (let ((s (copy-sequence \"abc\")))
              (vm-fillarray-alias s ?y)
              s))",
    );
    assert_eq!(result, r#"OK "yyy""#);
}

#[test]
fn fillarray_string_writeback_updates_alias_from_prog1_expression() {
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (fillarray (prog1 s) ?x) s)");
    assert_eq!(result, r#"OK "xxx""#);
}

#[test]
fn fillarray_string_writeback_updates_alias_from_list_car_expression() {
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (fillarray (car (list s)) ?y) s)");
    assert_eq!(result, r#"OK "yyy""#);
}

#[test]
fn fillarray_string_writeback_updates_vector_alias_element() {
    let result =
        eval_one("(let* ((s (copy-sequence \"abc\")) (v (vector s))) (fillarray s ?x) (aref v 0))");
    assert_eq!(result, r#"OK "xxx""#);
}

#[test]
fn fillarray_string_writeback_updates_cons_alias_element() {
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (cell (cons s nil))) (fillarray s ?y) (car cell))",
    );
    assert_eq!(result, r#"OK "yyy""#);
}

#[test]
fn fillarray_string_writeback_preserves_eq_hash_key_lookup() {
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'eq)))
           (puthash s 'v ht)
           (fillarray s ?x)
           (gethash s ht))",
    );
    assert_eq!(result, "OK v");
}

#[test]
fn fillarray_string_writeback_preserves_eql_hash_key_lookup() {
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'eql)))
           (puthash s 'v ht)
           (fillarray s ?y)
           (gethash s ht))",
    );
    assert_eq!(result, "OK v");
}

#[test]
fn fillarray_string_writeback_equal_hash_key_lookup_stays_nil() {
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'equal)))
           (puthash s 'v ht)
           (fillarray s ?z)
           (gethash s ht))",
    );
    assert_eq!(result, "OK nil");
}

#[test]
fn aset_string_writeback_updates_symbol_binding() {
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (aset s 1 ?x) s)");
    assert_eq!(result, r#"OK "axc""#);
}

#[test]
fn aset_alias_string_writeback_updates_symbol_binding() {
    let result = eval_one(
        "(progn
            (defalias 'vm-aset-alias 'aset)
            (let ((s (copy-sequence \"abc\")))
              (vm-aset-alias s 1 ?y)
              s))",
    );
    assert_eq!(result, r#"OK "ayc""#);
}

#[test]
fn aset_string_writeback_updates_alias_from_prog1_expression() {
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (aset (prog1 s) 1 ?x) s)");
    assert_eq!(result, r#"OK "axc""#);
}

#[test]
fn aset_string_writeback_updates_alias_from_list_car_expression() {
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (aset (car (list s)) 1 ?y) s)");
    assert_eq!(result, r#"OK "ayc""#);
}

#[test]
fn aset_string_writeback_updates_vector_alias_element() {
    let result =
        eval_one("(let* ((s (copy-sequence \"abc\")) (v (vector s))) (aset s 1 ?x) (aref v 0))");
    assert_eq!(result, r#"OK "axc""#);
}

#[test]
fn aset_string_writeback_updates_cons_alias_element() {
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (cell (cons s nil))) (aset s 1 ?y) (car cell))",
    );
    assert_eq!(result, r#"OK "ayc""#);
}

#[test]
fn aset_string_writeback_preserves_eq_hash_key_lookup() {
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'eq)))
           (puthash s 'v ht)
           (aset s 1 ?x)
           (gethash s ht))",
    );
    assert_eq!(result, "OK v");
}

#[test]
fn aset_string_writeback_preserves_eql_hash_key_lookup() {
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'eql)))
           (puthash s 'v ht)
           (aset s 1 ?y)
           (gethash s ht))",
    );
    assert_eq!(result, "OK v");
}

#[test]
fn aset_string_writeback_equal_hash_key_lookup_stays_nil() {
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'equal)))
           (puthash s 'v ht)
           (aset s 1 ?z)
           (gethash s ht))",
    );
    assert_eq!(result, "OK nil");
}

// -----------------------------------------------------------------------
// GC integration tests
// -----------------------------------------------------------------------

#[test]
fn gc_collect_retains_reachable() {
    let mut ev = Evaluator::new();
    let forms = crate::emacs_core::parse_forms("(setq x (cons 1 2))").unwrap();
    ev.eval_forms(&forms);
    let before = ev.heap.allocated_count();
    ev.gc_collect();
    let after = ev.heap.allocated_count();
    // The cons stored in variable `x` must survive.
    assert!(after >= 1, "reachable cons was collected");
    assert!(after <= before, "gc should not increase count");
    // Verify the value is still accessible.
    let forms2 = crate::emacs_core::parse_forms("(car x)").unwrap();
    let results = ev.eval_forms(&forms2);
    assert_eq!(format_eval_result(&results[0]), "OK 1");
}

#[test]
fn gc_collect_frees_unreachable() {
    let mut ev = Evaluator::new();
    // Create orphaned conses that aren't bound to any variable.
    let forms =
        crate::emacs_core::parse_forms("(progn (cons 1 2) (cons 3 4) (cons 5 6) nil)").unwrap();
    ev.eval_forms(&forms);
    let before = ev.heap.allocated_count();
    ev.gc_collect();
    let after = ev.heap.allocated_count();
    // The orphaned conses should have been freed.
    assert!(
        after < before,
        "gc did not free unreachable objects: before={before}, after={after}"
    );
}

#[test]
fn gc_collect_handles_cycles() {
    let mut ev = Evaluator::new();
    // Create a circular list: (setq x (cons 1 nil)) (setcdr x x)
    let forms =
        crate::emacs_core::parse_forms("(progn (setq x (cons 1 nil)) (setcdr x x) t)").unwrap();
    ev.eval_forms(&forms);
    // GC should handle cycles without infinite loop.
    ev.gc_collect();
    // x is still reachable.
    let forms2 = crate::emacs_core::parse_forms("(car x)").unwrap();
    let results = ev.eval_forms(&forms2);
    assert_eq!(format_eval_result(&results[0]), "OK 1");

    // Now remove the root and collect — the cycle should be freed.
    let forms3 = crate::emacs_core::parse_forms("(setq x nil)").unwrap();
    ev.eval_forms(&forms3);
    let before = ev.heap.allocated_count();
    ev.gc_collect();
    let after = ev.heap.allocated_count();
    assert!(
        after < before,
        "cyclic cons not freed: before={before}, after={after}"
    );
}

#[test]
fn gc_safe_point_collects_when_threshold_reached() {
    let mut ev = Evaluator::new();
    ev.heap.set_gc_threshold(5);
    // Allocate enough conses to exceed threshold.
    let forms = crate::emacs_core::parse_forms(
        "(progn (cons 1 2) (cons 3 4) (cons 5 6) (cons 7 8) (cons 9 10) nil)",
    )
    .unwrap();
    ev.eval_forms(&forms);
    assert!(ev.heap.should_collect());
    // With incremental GC, safe point may need multiple calls to finish.
    while ev.gc_count == 0 {
        ev.gc_safe_point();
    }
    assert_eq!(ev.gc_count, 1);
    // After collection, threshold adapts and should_collect is false.
    assert!(!ev.heap.should_collect());
}

#[test]
fn gc_threshold_adapts_after_collection() {
    let mut ev = Evaluator::new();
    ev.heap.set_gc_threshold(3);
    // Create 3 conses that are reachable via variables.
    let forms = crate::emacs_core::parse_forms(
        "(progn (setq a (cons 1 2)) (setq b (cons 3 4)) (setq c (cons 5 6)))",
    )
    .unwrap();
    ev.eval_forms(&forms);
    ev.gc_collect();
    // Threshold should adapt to max(8192, alive_count*2).
    let alive = ev.heap.allocated_count();
    assert!(alive >= 3);
    let threshold = ev.heap.gc_threshold();
    assert!(
        threshold >= 8192,
        "threshold should be at least 8192, got {threshold}"
    );
}

// -----------------------------------------------------------------------
// GC stress tests — force collection between every top-level form
// -----------------------------------------------------------------------

fn eval_stress(src: &str) -> Vec<String> {
    let mut ev = Evaluator::new();
    let forms = crate::emacs_core::parse_forms(src).expect("parse");
    ev.gc_stress = true;
    // Force very low threshold so gc_safe_point triggers on every call
    ev.heap.set_gc_threshold(1);
    let mut results = Vec::new();
    for form in &forms {
        let r = ev.eval_expr(form);
        results.push(format_eval_result(&r));
        ev.gc_safe_point();
    }
    results
}

#[test]
fn gc_stress_arithmetic() {
    let r = eval_stress("(+ 1 2) (* 3 4) (- 10 5)");
    assert_eq!(r, vec!["OK 3", "OK 12", "OK 5"]);
}

#[test]
fn gc_stress_cons_operations() {
    let r = eval_stress(
        "(setq x (cons 1 (cons 2 (cons 3 nil))))
         (car x)
         (car (cdr x))
         (length x)",
    );
    assert_eq!(r, vec!["OK (1 2 3)", "OK 1", "OK 2", "OK 3"]);
}

#[test]
fn gc_stress_vector_operations() {
    let r = eval_stress(
        "(setq v (vector 10 20 30))
         (aref v 0)
         (aset v 1 99)
         (aref v 1)",
    );
    assert_eq!(r, vec!["OK [10 20 30]", "OK 10", "OK 99", "OK 99"]);
}

#[test]
fn gc_stress_hash_table() {
    let r = eval_stress(
        "(setq ht (make-hash-table :test 'equal))
         (puthash \"a\" 1 ht)
         (puthash \"b\" 2 ht)
         (gethash \"a\" ht)
         (gethash \"b\" ht)
         (hash-table-count ht)",
    );
    assert_eq!(r[3], "OK 1");
    assert_eq!(r[4], "OK 2");
    assert_eq!(r[5], "OK 2");
}

#[test]
fn gc_stress_closures() {
    // Test lambdas and funcall survive GC (dynamic binding).
    // Lexical capture across separate top-level forms is a
    // pre-existing limitation unrelated to GC.
    let r = eval_stress(
        "(defun my-add (a b) (+ a b))
         (setq f (lambda (x) (my-add x 10)))
         (funcall f 5)
         (funcall f 20)",
    );
    assert_eq!(r[2], "OK 15");
    assert_eq!(r[3], "OK 30");
}

#[test]
fn gc_stress_lambda_argument_closure_survives_binding_installation() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.heap.set_gc_threshold(1);
    let forms = parse_forms(
        r#"(let ((payload (list 1 2 3)))
             ((lambda (orig)
                (funcall orig))
              (lambda () payload)))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK (1 2 3)");
}

#[test]
fn gc_stress_direct_lambda_head_roots_fresh_closure_during_arg_eval() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.heap.set_gc_threshold(1);
    let forms = parse_forms(
        r#"((lambda (f value)
              (funcall f value))
            (lambda (x) x)
            (prog1 (list 1 2 3)
              (list 4 5 6)
              (list 7 8 9)))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK (1 2 3)");
}

#[test]
fn gc_stress_builtin_apply_roots_closure_function_argument() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.heap.set_gc_threshold(1);
    let forms = parse_forms(
        r#"(let ((payload (list 7 8 9)))
             (let ((f (lambda () payload)))
               (apply f nil)))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK (7 8 9)");
}

#[test]
fn gc_stress_let_star_lexical_binding_roots_evaluated_values() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.heap.set_gc_threshold(1);
    let forms = parse_forms(
        r#"(let ((build (lambda () (list 4 5 6))))
             (let* ((x (funcall build))
                    (y x))
               y))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK (4 5 6)");
}

#[test]
fn gc_stress_prog1_roots_first_value() {
    let r = eval_stress("(prog1 (list 1 2 3) (list 4 5 6) (list 7 8 9))");
    assert_eq!(r[0], "OK (1 2 3)");
}

#[test]
fn gc_stress_apply_env_expander_closure_capturing_uninterned_symbol() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    ev.lexenv = Value::list(vec![Value::True]);
    ev.gc_stress = true;
    ev.heap.set_gc_threshold(1);
    let forms = parse_forms(
        r#"
        (let ((newenv nil)
              (magic (make-symbol "vm-magic")))
          (let ((var (make-symbol "vm-var")))
            (setq newenv
                  (cons
                   (cons 'vm-head
                         (lambda (&rest args)
                           (if (eq (car args) magic)
                               (list magic var)
                             (cons 'funcall (cons var args)))))
                   newenv))
            (let* ((form '(vm-head 1 2 3))
                   (head (car form))
                   (env-expander (assq head newenv)))
              (apply (cdr env-expander) (cdr form)))))
        "#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK (funcall vm-var 1 2 3)");
}

#[test]
fn interpreted_closure_while_can_advance_lexical_loop_variable() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    let forms = parse_forms(
        r#"
        (funcall
         (let ((items '(a b c)))
           (lambda ()
             (let ((l items)
                   (count 0))
               (while l
                 (setq l (cdr l))
                 (setq count (1+ count)))
               count))))
        "#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK 3");
}

#[test]
fn gc_stress_aref_on_closure_survives_closure_vector_conversion() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.heap.set_gc_threshold(1);
    let forms = parse_forms(
        r#"(let ((payload (list 1 2 3)))
             (let ((closure (lambda () payload)))
               (not (null (aref closure 2)))))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK t");
}

#[test]
fn gc_stress_cdr_on_lambda_survives_cons_list_conversion() {
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.heap.set_gc_threshold(1);
    let forms = parse_forms(
        r#"(let ((payload (list 1 2 3)))
             (let ((closure (lambda () payload)))
               (not (null (car (cdr closure))))))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK t");
}

#[test]
fn gc_stress_recursive_function() {
    let r = eval_stress(
        "(defun my-length (lst)
           (if (null lst) 0
             (1+ (my-length (cdr lst)))))
         (my-length '(a b c d e))
         (my-length nil)",
    );
    assert_eq!(r[1], "OK 5");
    assert_eq!(r[2], "OK 0");
}

#[test]
fn gc_stress_setcar_setcdr() {
    let r = eval_stress(
        "(setq x (cons 1 2))
         (setcar x 10)
         (setcdr x 20)
         x",
    );
    assert_eq!(r[3], "OK (10 . 20)");
}

#[test]
fn gc_stress_let_bindings() {
    let r = eval_stress(
        "(let ((a (cons 1 2))
               (b (cons 3 4)))
           (cons (car a) (car b)))",
    );
    assert_eq!(r[0], "OK (1 . 3)");
}

#[test]
fn gc_stress_mapcar() {
    let r = eval_stress("(mapcar '1+ '(1 2 3 4 5))");
    assert_eq!(r[0], "OK (2 3 4 5 6)");
}

#[test]
fn gc_stress_string_operations() {
    let r = eval_stress(
        r#"(setq s (concat "hello" " " "world"))
           (length s)
           (substring s 0 5)"#,
    );
    assert_eq!(r[0], r#"OK "hello world""#);
    assert_eq!(r[1], "OK 11");
    assert_eq!(r[2], r#"OK "hello""#);
}

#[test]
fn gc_stress_nreverse() {
    let r = eval_stress(
        "(setq x (list 1 2 3 4 5))
         (setq y (nreverse x))
         y",
    );
    assert_eq!(r[2], "OK (5 4 3 2 1)");
}

#[test]
fn gc_stress_plist() {
    let r = eval_stress(
        "(setq pl '(a 1 b 2 c 3))
         (plist-get pl 'b)
         (setq pl (plist-put pl 'b 99))
         (plist-get pl 'b)",
    );
    assert_eq!(r[1], "OK 2");
    assert_eq!(r[3], "OK 99");
}

#[test]
fn gc_stress_circular_list_survives() {
    // Create circular list inside a single progn to avoid formatting
    // the circular cons (which would hang the Display impl).
    let r = eval_stress(
        "(progn
           (setq x (cons 42 nil))
           (setcdr x x)
           (car x))",
    );
    assert_eq!(r[0], "OK 42");
}

#[test]
fn gc_stress_many_allocations() {
    // Allocate many short-lived conses; only final result should survive
    let r = eval_stress(
        "(let ((result nil))
           (dotimes (i 100)
             (setq result (cons i result)))
           (length result))",
    );
    assert_eq!(r[0], "OK 100");
}

// -----------------------------------------------------------------------
// Lexical closure mutation visibility tests
// -----------------------------------------------------------------------

#[test]
fn lexical_closure_mutation_visible() {
    // Closures must share the same lexical frame — mutations through
    // one closure must be visible to the outer scope.
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    let forms = parse_forms(
        r#"(let ((x 0))
             (let ((f (lambda () (setq x (1+ x)))))
               (funcall f)
               (funcall f)
               x))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK 2");
}

#[test]
fn lexical_closure_shared_state() {
    // Two closures sharing the same binding (inc + get).
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    let forms = parse_forms(
        r#"(let ((x 0))
             (let ((inc (lambda () (setq x (1+ x))))
                   (get (lambda () x)))
               (funcall inc)
               (funcall inc)
               (funcall inc)
               (funcall get)))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK 3");
}

#[test]
fn lexical_closure_make_counter() {
    // Classic make-counter pattern with independent counters.
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    let forms = parse_forms(
        r#"(progn
             (defun make-counter ()
               (let ((n 0))
                 (lambda () (setq n (1+ n)))))
             (let ((c1 (make-counter))
                   (c2 (make-counter)))
               (funcall c1)
               (funcall c1)
               (funcall c1)
               (let ((r1 (funcall c1))
                     (r2 (funcall c2)))
                 (list r1 r2))))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    // c1 called 4 times → 4; c2 called once → 1; independent counters
    assert_eq!(result, "OK (4 1)");
}

#[test]
fn lexical_closure_outer_mutation_visible() {
    // Outer setq visible to closure.
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    let forms = parse_forms(
        r#"(let ((x 10))
             (let ((f (lambda () x)))
               (setq x 42)
               (funcall f)))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK 42");
}

#[test]
fn closure_inside_mapcar_lambda_captures_outer_param() {
    // Reproduces the pcase-compile-patterns pattern:
    // (mapcar (lambda (case)
    //           (list case
    //                 (lambda (vars) case)))
    //         '(a b c))
    // Each inner lambda should capture `case` from the outer lambda.
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    let forms = parse_forms(
        r#"(let ((closures
                 (mapcar (lambda (case)
                           (lambda () case))
                         '(a b c))))
             (list (funcall (car closures))
                   (funcall (car (cdr closures)))
                   (funcall (car (cdr (cdr closures))))))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK (a b c)");
}

#[test]
fn closure_inside_backquote_mapcar_captures_outer_param() {
    // More closely matches pcase-compile-patterns:
    // The inner lambda is created inside a backquote, after a function call.
    let mut ev = Evaluator::new();
    ev.set_lexical_binding(true);
    let forms = parse_forms(
        r#"(let ((closures
                 (mapcar (lambda (case)
                           (list (car case)
                                 (lambda (vars)
                                   (list case vars))))
                         '((a 1) (b 2) (c 3)))))
             (let ((fn2 (car (cdr (car closures)))))
               (funcall fn2 42)))"#,
    )
    .expect("parse");
    let result = format_eval_result(&ev.eval_expr(&forms[0]));
    assert_eq!(result, "OK ((a 1) 42)");
}

#[test]
fn closure_inside_real_backquote_with_fn_call_captures_outer_param() {
    // Replicates the exact pcase-compile-patterns pattern:
    // (mapcar (lambda (case)
    //           `(,(some-fn val (car case))
    //             ,(lambda (vars) (list case vars))))
    //         cases)
    // The inner lambda is inside a REAL backquote (macro), after a function call.
    // This requires loading backquote.el.
    let mut eval = Evaluator::new();
    load_minimal_backquote_runtime(&mut eval);

    let forms = parse_forms(
        r#"(progn
             (defun my-match (val upat) (list val upat))
             (let ((closures
                    (mapcar (lambda (case)
                              `(,(my-match 'x (car case))
                                ,(lambda (vars) (list case vars))))
                            '((a 1) (b 2)))))
               (let ((fn1 (car (cdr (car closures)))))
                 (funcall fn1 'matched))))"#,
    )
    .expect("parse");
    let result = format_eval_result(&eval.eval_expr(&forms[0]));
    assert_eq!(result, "OK ((a 1) matched)");
}

#[test]
fn real_backquote_computed_symbols_match_runtime_macro_semantics() {
    let mut eval = Evaluator::new();
    load_minimal_backquote_runtime(&mut eval);

    let forms = parse_forms(
        r#"(let ((prefix "neovm-bqc-test")
                 (suffixes '("x" "y" "z")))
             (let ((forms
                    (let ((i 0))
                      (mapcar (lambda (s)
                                (setq i (1+ i))
                                `(list ',(intern (concat prefix "-" s)) ,i))
                              suffixes))))
               (mapcar #'eval forms)))"#,
    )
    .expect("parse");
    let result = format_eval_result(&eval.eval_expr(&forms[0]));
    assert_eq!(
        result,
        "OK ((neovm-bqc-test-x 1) (neovm-bqc-test-y 2) (neovm-bqc-test-z 3))"
    );
}

#[test]
fn real_backquote_nested_eval_chain_matches_gnu_error_shape() {
    let mut eval = Evaluator::new();
    load_minimal_backquote_runtime(&mut eval);

    let forms = parse_forms(
        r#"(let ((x 10))
             (let ((template `(let ((y ,,x)) `(+ ,y ,,x))))
               (list template
                     (condition-case e (eval template) (error (cons 'ERR e)))
                     (condition-case e (eval (eval template)) (error (cons 'ERR e))))))"#,
    )
    .expect("parse");
    let result = format_eval_result(&eval.eval_expr(&forms[0]));
    assert_eq!(result, r#"ERR (void-function (\,))"#);
}

#[test]
fn condition_case_lexical_handler_binding_restores_outer_let() {
    let mut eval = Evaluator::new();
    eval.set_lexical_binding(true);

    let forms = parse_forms(
        r#"(let ((outer 'original))
             (list
              (condition-case outer
                  (/ 1 0)
                (arith-error
                 (setq outer (list 'caught (car outer)))
                 outer))
              outer))"#,
    )
    .expect("parse");
    let result = format_eval_result(&eval.eval_expr(&forms[0]));
    assert_eq!(result, "OK ((caught arith-error) original)");
}

#[test]
fn gc_stress_lexical_closure_mutation() {
    // GC stress variant of closure mutation.
    let r = eval_stress(
        "(let ((x 0))
           (let ((f (lambda () (setq x (1+ x)))))
             (funcall f)
             (funcall f)
             (funcall f)
             x))",
    );
    assert_eq!(r[0], "OK 3");
}

#[test]
fn evaluator_face_table_has_standard_faces() {
    let ev = Evaluator::new();
    let ft = ev.face_table();

    // Standard faces must exist
    assert!(ft.get("default").is_some(), "missing default face");
    assert!(ft.get("bold").is_some(), "missing bold face");
    assert!(ft.get("italic").is_some(), "missing italic face");
    assert!(ft.get("mode-line").is_some(), "missing mode-line face");
    assert!(
        ft.get("minibuffer-prompt").is_some(),
        "missing minibuffer-prompt face"
    );

    // Resolve should apply inheritance (bold inherits from default)
    let bold = ft.resolve("bold");
    assert!(
        bold.foreground.is_some(),
        "bold should inherit foreground from default"
    );
    assert!(
        bold.weight.map_or(false, |w| w.is_bold()),
        "bold face should have bold weight",
    );
}

#[test]
fn advice_around_compiler_macro_pattern() {
    // Reproduce the cl-macs pattern: macroexp--compiler-macro calls a
    // compiler-macro handler. condition-case-unless-debug should catch
    // wrong-number-of-arguments errors.
    let results = eval_all(
        r#"
        ;; Simulate a compiler-macro handler that needs 2 args
        (defun my-cmacro-handler (form arg)
          (list 'optimized form arg))

        ;; But it gets called with wrong arity via apply
        (condition-case err
            (apply 'my-cmacro-handler '((my-fn 1 2) 1 2))
          (wrong-number-of-arguments
           (list 'caught-wna err)))
        "#,
    );
    for (i, r) in results.iter().enumerate() {
        eprintln!("cmacro[{i}]: {r}");
    }
}

#[test]
fn oclosure_define_basic() {
    // Test basic oclosure-define usage - the pattern that fails in loadup
    let results = eval_all(
        r#"
        ;; oclosure-define should create a type
        (condition-case err
            (oclosure-define my-test-ocl "A test oclosure type.")
          (error (list 'error err)))
        ;; Check if it worked
        (condition-case err
            (oclosure-define my-test-ocl2 "Another test." (slot1))
          (error (list 'error err)))
        "#,
    );
    for (i, r) in results.iter().enumerate() {
        eprintln!("oclosure-define[{i}]: {r}");
    }
}

#[test]
fn oclosure_define_macroexpand() {
    // Trace what oclosure-define expands to
    let results = eval_all(
        r#"
        ;; Check if oclosure-define is a macro
        (fboundp 'oclosure-define)
        (macroexpand-1 '(oclosure-define my-test-ocl "Test type."))
        (macroexpand-1 '(oclosure-define my-test-ocl2 "Test2." (slot1)))
        "#,
    );
    for (i, r) in results.iter().enumerate() {
        eprintln!("macroexpand-ocl[{i}]: {r}");
    }
}

#[test]
fn cl_defstruct_keyword_handling() {
    // Test cl-defstruct with :copier/:constructor keywords
    // These fail with (invalid-function :copier) in loadup
    let results = eval_all(
        r#"
        ;; Check if cl-defstruct is a macro
        (fboundp 'cl-defstruct)
        (condition-case err
            (macroexpand '(cl-defstruct (my-test-struct (:copier nil)) field1 field2))
          (error (list 'macroexpand-error err)))
        (condition-case err
            (cl-defstruct (my-test-struct (:copier nil)) field1 field2)
          (error (list 'error err)))
        "#,
    );
    for (i, r) in results.iter().enumerate() {
        eprintln!("cl-defstruct[{i}]: {r}");
    }
}

#[test]
fn cl_deftype_basic() {
    // Test cl-deftype which fails in ring.el with (void-variable ring)
    let results = eval_all(
        r#"
        (condition-case err
            (cl-deftype my-ring-test nil '(satisfies ring-p))
          (error (list 'error err)))
        "#,
    );
    for (i, r) in results.iter().enumerate() {
        eprintln!("cl-deftype[{i}]: {r}");
    }
}
