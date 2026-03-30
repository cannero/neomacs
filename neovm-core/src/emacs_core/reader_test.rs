use super::*;
use crate::emacs_core::eval::Context;
use crate::emacs_core::load::{
    apply_ldefs_boot_autoloads_for_names, apply_runtime_startup_state,
    create_bootstrap_evaluator_cached,
};
use crate::emacs_core::parse_forms;
use crate::emacs_core::{format_eval_result, parse_forms as parse_bootstrap_forms};
use std::collections::VecDeque;

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let forms = parse_bootstrap_forms(src).expect("parse");
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

// ===================================================================
// read-from-string tests
// ===================================================================

#[test]
fn read_from_string_integer() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("42")]).unwrap();
    // Should be (42 . 2)
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Int(42)));
            assert!(matches!(&pair.cdr, Value::Int(2)));
        }
        _ => panic!("Expected cons, got {:?}", result),
    }
}

#[test]
fn read_from_string_symbol() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("hello")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Symbol(id) if resolve_sym(*id) == "hello"));
            assert!(matches!(&pair.cdr, Value::Int(5)));
        }
        _ => panic!("Expected cons, got {:?}", result),
    }
}

#[test]
fn read_from_string_string_value() {
    let mut ev = Context::new();
    let result =
        builtin_read_from_string(&mut ev, vec![Value::string(r#""hello world""#)]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert_eq!(pair.car.as_str(), Some("hello world"));
            assert!(matches!(&pair.cdr, Value::Int(13)));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_list() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("(+ 1 2)")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            // car should be a list (+ 1 2)
            assert!(pair.car.is_cons());
            assert!(matches!(&pair.cdr, Value::Int(7)));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_with_start() {
    let mut ev = Context::new();
    // "  42 rest" — start at 2
    let result =
        builtin_read_from_string(&mut ev, vec![Value::string("  42 rest"), Value::Int(2)]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Int(42)));
            assert!(matches!(&pair.cdr, Value::Int(4)));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_float() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("3.14")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Float(f, _) if (*f - 3.14).abs() < 1e-10));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_char() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("?a")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Char('a')));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_nil() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("nil")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(pair.car.is_nil());
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_t() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("t")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::True));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_vector() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("[1 2 3]")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(pair.car.is_vector());
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_quoted() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("'foo")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            // Should be (quote foo) as a list
            assert!(pair.car.is_cons());
            assert!(matches!(&pair.cdr, Value::Int(4)));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_dotted_pair() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("(a . b)")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            // car should be a dotted pair (a . b)
            assert!(pair.car.is_cons());
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_keyword() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string(":test")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Keyword(id) if resolve_sym(*id) == ":test"));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_uninterned_symbol() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("#:test")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            match pair.car {
                Value::Symbol(id) => {
                    assert_eq!(resolve_sym(id), "test");
                    assert_ne!(id, crate::emacs_core::intern::intern("test"));
                }
                other => panic!("expected uninterned symbol, got {other:?}"),
            }
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_empty_error() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("")]);
    assert!(result.is_err());
}

#[test]
fn read_from_string_whitespace_only_error() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("   ")]);
    assert!(result.is_err());
}

#[test]
fn read_from_string_multiple_forms_reads_first() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("42 99")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Int(42)));
            // End position should be after "42" (position 2), not after "99"
            match &pair.cdr {
                Value::Int(n) => assert!(*n <= 3, "end pos {} should be <= 3", n),
                _ => panic!("Expected int end position"),
            }
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_with_start_and_end() {
    let mut ev = Context::new();
    // "xxx42yyy" with start=3, end=5 -> substring "42"
    let result = builtin_read_from_string(
        &mut ev,
        vec![Value::string("xxx42yyy"), Value::Int(3), Value::Int(5)],
    )
    .unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Int(42)));
            assert!(matches!(&pair.cdr, Value::Int(5)));
        }
        _ => panic!("Expected cons"),
    }
}

// ===================================================================
// read tests
// ===================================================================

#[test]
fn read_from_string_stream() {
    let mut ev = Context::new();
    let result = builtin_read(&mut ev, vec![Value::string("42")]).unwrap();
    assert!(matches!(result, Value::Int(42)));
}

#[test]
fn read_nil_stream() {
    let mut ev = Context::new();
    let result = builtin_read(&mut ev, vec![Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn read_no_args() {
    let mut ev = Context::new();
    let result = builtin_read(&mut ev, vec![]);
    assert!(result.is_err());
}

#[test]
fn read_rejects_extra_args() {
    let mut ev = Context::new();
    let result = builtin_read(&mut ev, vec![Value::string("a"), Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn read_non_stream_type_is_invalid_function() {
    let mut ev = Context::new();
    let result = builtin_read(&mut ev, vec![Value::Int(1)]);
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "invalid-function"),
        other => panic!("expected invalid-function signal, got {other:?}"),
    }
}

// ===================================================================
// Stub function tests
// ===================================================================

#[test]
fn read_from_minibuffer_signals_end_of_file() {
    let mut ev = Context::new();
    let result = builtin_read_from_minibuffer(&mut ev, vec![Value::string("Prompt: ")]);
    assert!(result.is_err());
}

#[test]
fn read_from_minibuffer_non_character_event_stays_queued_and_signals_end_of_file() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo")]),
    );
    let result = builtin_read_from_minibuffer(&mut ev, vec![Value::string("Prompt: ")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::symbol("foo")]))
    );
}

#[test]
fn read_from_minibuffer_ignores_initial_and_signals_end_of_file() {
    let mut ev = Context::new();
    let result = builtin_read_from_minibuffer(
        &mut ev,
        vec![Value::string("Prompt: "), Value::string("initial")],
    );
    assert!(result.is_err());
}

#[test]
fn read_from_minibuffer_rejects_non_stringish_initial_input() {
    let mut ev = Context::new();
    let result =
        builtin_read_from_minibuffer(&mut ev, vec![Value::string("Prompt: "), Value::Int(1)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn read_from_minibuffer_rejects_cons_initial_with_non_string_car() {
    let mut ev = Context::new();
    let cons_initial = Value::cons(Value::Int(1), Value::Int(1));
    let result =
        builtin_read_from_minibuffer(&mut ev, vec![Value::string("Prompt: "), cons_initial]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn read_from_minibuffer_rejects_more_than_seven_args() {
    let mut ev = Context::new();
    let result = builtin_read_from_minibuffer(
        &mut ev,
        vec![
            Value::string("Prompt: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn shared_read_from_minibuffer_runtime_runs_setup_and_exit_hooks_around_edit() {
    let mut ev = Context::new();
    let order = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let order_in_setup = std::rc::Rc::clone(&order);
    let order_in_exit = std::rc::Rc::clone(&order);
    let order_in_edit = std::rc::Rc::clone(&order);
    let args = vec![Value::string("Prompt: ")];

    let result = finish_read_from_minibuffer_in_state_with_recursive_edit(
        &mut ev.obarray,
        &mut ev.buffers,
        &mut ev.frames,
        &mut ev.minibuffers,
        &mut ev.minibuffer_selected_window,
        &mut ev.active_minibuffer_window,
        ev.command_loop.recursive_depth,
        &args,
        move || {
            order_in_setup.borrow_mut().push("setup");
            Ok(Value::Nil)
        },
        move || {
            order_in_exit.borrow_mut().push("exit");
            Ok(Value::Nil)
        },
        move || {
            order_in_edit.borrow_mut().push("edit");
            Err(Flow::Throw {
                tag: Value::symbol("exit"),
                value: Value::Nil,
            })
        },
    )
    .expect("shared read-from-minibuffer should exit normally");

    let Value::Str(result_id) = result else {
        panic!("expected string result, got {result:?}");
    };
    assert_eq!(
        crate::emacs_core::value::with_heap(|heap| heap.get_string(result_id).to_owned()),
        ""
    );
    assert_eq!(*order.borrow(), vec!["setup", "edit", "exit"]);
}

#[test]
fn shared_read_from_minibuffer_runtime_swallows_exit_hook_signals() {
    let mut ev = Context::new();
    let args = vec![Value::string("Prompt: ")];

    let result = finish_read_from_minibuffer_in_state_with_recursive_edit(
        &mut ev.obarray,
        &mut ev.buffers,
        &mut ev.frames,
        &mut ev.minibuffers,
        &mut ev.minibuffer_selected_window,
        &mut ev.active_minibuffer_window,
        ev.command_loop.recursive_depth,
        &args,
        || Ok(Value::Nil),
        || Err(signal("error", vec![Value::string("ignored")])),
        || {
            Err(Flow::Throw {
                tag: Value::symbol("exit"),
                value: Value::Nil,
            })
        },
    );

    assert!(result.is_ok(), "result={result:?}");
}

#[test]
fn activate_minibuffer_window_switches_displayed_buffer_and_restores_state() {
    let mut ev = Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut ev);
    let minibuffer_window = ev
        .frame_manager()
        .get(frame_id)
        .and_then(|frame| frame.minibuffer_window)
        .expect("initial frame minibuffer window");
    let previous_selected_window = ev
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    let previous_minibuffer_buffer = ev
        .frame_manager()
        .get(frame_id)
        .and_then(|frame| frame.find_window(minibuffer_window))
        .and_then(|window| window.buffer_id())
        .expect("inactive minibuffer buffer");

    let active_buffer = ev.buffer_manager_mut().create_buffer(" *Minibuf-1*");
    let saved = activate_minibuffer_window(&mut ev, active_buffer).expect("activate minibuffer");

    let frame = ev
        .frame_manager()
        .get(frame_id)
        .expect("frame should stay live");
    assert_eq!(frame.selected_window, minibuffer_window);
    assert_eq!(
        frame
            .find_window(minibuffer_window)
            .and_then(|window| window.buffer_id()),
        Some(active_buffer)
    );
    assert_eq!(ev.buffer_manager().current_buffer_id(), Some(active_buffer));
    assert_eq!(ev.active_minibuffer_window, Some(minibuffer_window));
    assert_eq!(
        ev.minibuffer_selected_window,
        Some(previous_selected_window)
    );

    restore_minibuffer_window(&mut ev, saved);

    let frame = ev
        .frame_manager()
        .get(frame_id)
        .expect("frame should stay live");
    assert_eq!(frame.selected_window, previous_selected_window);
    assert_eq!(
        frame
            .find_window(minibuffer_window)
            .and_then(|window| window.buffer_id()),
        Some(previous_minibuffer_buffer)
    );
    assert_eq!(ev.active_minibuffer_window, None);
    assert_eq!(ev.minibuffer_selected_window, None);
}

#[test]
fn read_string_signals_end_of_file() {
    let mut ev = Context::new();
    let result = builtin_read_string(&mut ev, vec![Value::string("Prompt: ")]);
    assert!(result.is_err());
}

#[test]
fn read_string_non_character_event_stays_queued_and_signals_end_of_file() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo")]),
    );
    let result = builtin_read_string(&mut ev, vec![Value::string("Prompt: ")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::symbol("foo")]))
    );
}

#[test]
fn read_string_ignores_initial_and_signals_end_of_file() {
    let mut ev = Context::new();
    let result = builtin_read_string(
        &mut ev,
        vec![Value::string("Prompt: "), Value::string("initial")],
    );
    assert!(result.is_err());
}

#[test]
fn read_string_rejects_non_stringish_initial_input() {
    let mut ev = Context::new();
    let result = builtin_read_string(&mut ev, vec![Value::string("Prompt: "), Value::Int(1)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn read_string_rejects_cons_initial_with_non_string_car() {
    let mut ev = Context::new();
    let cons_initial = Value::cons(Value::Int(1), Value::Int(1));
    let result = builtin_read_string(&mut ev, vec![Value::string("Prompt: "), cons_initial]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn read_string_rejects_more_than_five_args() {
    let mut ev = Context::new();
    let result = builtin_read_string(
        &mut ev,
        vec![
            Value::string("Prompt: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn finish_read_string_with_minibuffer_builds_expected_args() {
    let result = finish_read_string_with_minibuffer(
        &[
            Value::string("Prompt: "),
            Value::string("seed"),
            Value::symbol("hist"),
            Value::string("fallback"),
            Value::True,
        ],
        |minibuffer_args| {
            assert_eq!(
                minibuffer_args,
                &[
                    Value::string("Prompt: "),
                    Value::string("seed"),
                    Value::Nil,
                    Value::Nil,
                    Value::symbol("hist"),
                    Value::string("fallback"),
                    Value::True,
                ]
            );
            Ok(Value::string("result"))
        },
    )
    .unwrap();
    assert_eq!(result, Value::string("result"));
}

#[test]
fn completing_read_minibuffer_args_choose_completion_keymap_by_require_match() {
    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "minibuffer-local-completion-map",
        Value::symbol("completion-map"),
    );
    eval.obarray.set_symbol_value(
        "minibuffer-local-must-match-map",
        Value::symbol("must-match-map"),
    );

    let default_args = completing_read_minibuffer_args(
        eval.obarray(),
        &[
            Value::string("Prompt: "),
            Value::list(vec![Value::string("alpha")]),
            Value::Nil,
            Value::Nil,
            Value::string("seed"),
            Value::symbol("hist"),
            Value::string("fallback"),
            Value::True,
        ],
    );
    assert_eq!(
        default_args,
        [
            Value::string("Prompt: "),
            Value::string("seed"),
            Value::symbol("completion-map"),
            Value::Nil,
            Value::symbol("hist"),
            Value::string("fallback"),
            Value::True,
        ]
    );

    let must_match_args = completing_read_minibuffer_args(
        eval.obarray(),
        &[
            Value::string("Prompt: "),
            Value::list(vec![Value::string("alpha")]),
            Value::Nil,
            Value::True,
        ],
    );
    assert_eq!(must_match_args[2], Value::symbol("must-match-map"));
}

#[test]
fn read_number_signals_end_of_file_even_with_default() {
    let mut ev = Context::new();
    let result = builtin_read_number(&mut ev, vec![Value::string("Number: "), Value::Int(42)]);
    assert!(result.is_err());
}

#[test]
fn read_number_non_character_event_stays_queued_and_signals_end_of_file() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo")]),
    );
    let result = builtin_read_number(&mut ev, vec![Value::string("Number: ")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::symbol("foo")]))
    );
}

#[test]
fn read_number_signals_end_of_file_without_default() {
    let mut ev = Context::new();
    let result = builtin_read_number(&mut ev, vec![Value::string("Number: ")]);
    assert!(result.is_err());
}

#[test]
fn read_number_rejects_non_numeric_default() {
    let mut ev = Context::new();
    let result = builtin_read_number(&mut ev, vec![Value::string("Number: "), Value::string("x")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn read_number_accepts_numeric_default_and_signals_end_of_file() {
    let mut ev = Context::new();
    let result = builtin_read_number(
        &mut ev,
        vec![
            Value::string("Number: "),
            Value::Float(1.5, next_float_id()),
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"
    ));
}

#[test]
fn read_number_rejects_more_than_three_args() {
    let mut ev = Context::new();
    let result = builtin_read_number(
        &mut ev,
        vec![
            Value::string("Number: "),
            Value::Int(42),
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn read_number_rejects_non_string_prompt() {
    let mut ev = Context::new();
    let result = builtin_read_number(&mut ev, vec![Value::Int(123)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn read_passwd_startup_is_autoloaded() {
    let eval = eval_with_ldefs_boot_autoloads(&["read-passwd"]);
    let function = eval
        .obarray
        .symbol_function("read-passwd")
        .expect("missing read-passwd startup function cell");
    assert!(crate::emacs_core::autoload::is_autoload_value(&function));
}

#[test]
fn read_passwd_loads_from_gnu_auth_source() {
    let results = bootstrap_eval_all(
        r#"
        (condition-case err
            (read-passwd "")
          (error (list 'err (car err))))
        (subrp (symbol-function 'read-passwd))
        "#,
    );
    assert_eq!(results[0], r#"OK (err end-of-file)"#);
    assert_eq!(results[1], "OK nil");
}

#[test]
fn read_passwd_loaded_accepts_optional_args_and_signals_end_of_file() {
    let results = bootstrap_eval_all(
        r#"
        (condition-case err
            (read-passwd "" t "default")
          (error (list 'err (car err))))
        "#,
    );
    assert_eq!(results[0], r#"OK (err end-of-file)"#);
}

#[test]
fn read_passwd_loaded_rejects_non_string_prompt() {
    let results = bootstrap_eval_all(
        r#"
        (condition-case err
            (read-passwd 123)
          (error (list 'err (car err))))
        "#,
    );
    assert_eq!(results[0], r#"OK (err wrong-type-argument)"#);
}

#[test]
fn read_passwd_loaded_rejects_wrong_arity() {
    let results = bootstrap_eval_all(
        r#"
        (condition-case err
            (read-passwd)
          (error (list 'err (car err))))
        (condition-case err
            (read-passwd "" nil nil nil)
          (error (list 'err (car err))))
        "#,
    );
    assert_eq!(results[0], r#"OK (err wrong-number-of-arguments)"#);
    assert_eq!(results[1], r#"OK (err wrong-number-of-arguments)"#);
}

#[test]
fn completing_read_signals_end_of_file() {
    let mut ev = Context::new();
    let result = builtin_completing_read(&mut ev, vec![Value::string("Choose: "), Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn completing_read_non_character_event_stays_queued_and_signals_end_of_file() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo")]),
    );
    let result = builtin_completing_read(&mut ev, vec![Value::string("Choose: "), Value::Nil]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::symbol("foo")]))
    );
}

#[test]
fn completing_read_ignores_default_and_signals_end_of_file() {
    let mut ev = Context::new();
    let result = builtin_completing_read(
        &mut ev,
        vec![
            Value::string("Choose: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::string("fallback"),
        ],
    );
    assert!(result.is_err());
}

#[test]
fn completing_read_rejects_non_stringish_initial_input() {
    let mut ev = Context::new();
    let result = builtin_completing_read(
        &mut ev,
        vec![
            Value::string("Choose: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Int(1),
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn completing_read_accepts_cons_initial_with_string_and_position() {
    let mut ev = Context::new();
    let cons_initial = Value::cons(Value::string("x"), Value::Int(1));
    let result = builtin_completing_read(
        &mut ev,
        vec![
            Value::string("Choose: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            cons_initial,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"
    ));
}

#[test]
fn completing_read_rejects_cons_initial_with_non_string_car() {
    let mut ev = Context::new();
    let cons_initial = Value::cons(Value::Int(1), Value::Int(1));
    let result = builtin_completing_read(
        &mut ev,
        vec![
            Value::string("Choose: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            cons_initial,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn completing_read_rejects_cons_initial_with_non_numeric_position() {
    let mut ev = Context::new();
    let cons_initial = Value::cons(Value::string("x"), Value::Nil);
    let result = builtin_completing_read(
        &mut ev,
        vec![
            Value::string("Choose: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            cons_initial,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn completing_read_rejects_more_than_eight_args() {
    let mut ev = Context::new();
    let result = builtin_completing_read(
        &mut ev,
        vec![
            Value::string("Choose: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn y_or_n_p_signals_end_of_file() {
    let mut ev = Context::new();
    let result = builtin_y_or_n_p(&mut ev, vec![Value::string("Continue? ")]);
    assert!(result.is_err());
}

#[test]
fn y_or_n_p_rejects_non_sequence_prompt() {
    let mut ev = Context::new();
    let result = builtin_y_or_n_p(&mut ev, vec![Value::Int(123)]);
    assert!(result.is_err());
}

#[test]
fn y_or_n_p_rejects_extra_arg() {
    let mut ev = Context::new();
    let result = builtin_y_or_n_p(&mut ev, vec![Value::string("Continue? "), Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn y_or_n_p_accepts_nil_and_vector_prompts() {
    let mut ev_nil = Context::new();
    let nil_prompt = builtin_y_or_n_p(&mut ev_nil, vec![Value::Nil]);
    assert!(nil_prompt.is_err());

    let mut ev_vec = Context::new();
    let vector_prompt = builtin_y_or_n_p(
        &mut ev_vec,
        vec![Value::vector(vec![
            Value::Int(121),
            Value::Int(47),
            Value::Int(110),
        ])],
    );
    assert!(vector_prompt.is_err());
}

#[test]
fn y_or_n_p_rejects_list_prompt() {
    let mut ev = Context::new();
    let result = builtin_y_or_n_p(
        &mut ev,
        vec![Value::list(vec![
            Value::Int(121),
            Value::Int(47),
            Value::Int(110),
        ])],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn y_or_n_p_ignores_unread_events_and_eofs() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(121)]));
    let result = builtin_y_or_n_p(&mut ev, vec![Value::string("Continue? ")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"
    ));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(121)]))
    );
}

#[test]
fn y_or_n_p_unread_events_do_not_change() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(110)]));
    let result = builtin_y_or_n_p(&mut ev, vec![Value::string("Continue? ")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"
    ));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(110)]))
    );
}

#[test]
fn y_or_n_p_rejects_invalid_character_event() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(48)]));
    let result = builtin_y_or_n_p(&mut ev, vec![Value::string("Continue? ")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(48)]))
    );
}

#[test]
fn yes_or_no_p_signals_end_of_file() {
    let mut ev = Context::new();
    let result = builtin_yes_or_no_p(&mut ev, vec![Value::string("Confirm? ")]);
    assert!(result.is_err());
}

#[test]
fn yes_or_no_p_rejects_non_string_prompt() {
    let mut ev = Context::new();
    let result = builtin_yes_or_no_p(&mut ev, vec![Value::Int(123)]);
    assert!(result.is_err());
}

#[test]
fn yes_or_no_p_rejects_extra_arg() {
    let mut ev = Context::new();
    let result = builtin_yes_or_no_p(&mut ev, vec![Value::string("Confirm? "), Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn yes_or_no_p_ignores_unread_events_and_eofs() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(89)]));
    let result = builtin_yes_or_no_p(&mut ev, vec![Value::string("Confirm? ")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"
    ));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(89)]))
    );
}

#[test]
fn yes_or_no_p_unread_events_do_not_change() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(110)]));
    let result = builtin_yes_or_no_p(&mut ev, vec![Value::string("Confirm? ")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"
    ));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(110)]))
    );
}

#[test]
fn yes_or_no_p_rejects_invalid_character_event() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(48)]));
    let result = builtin_yes_or_no_p(&mut ev, vec![Value::string("Confirm? ")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(48)]))
    );
}

#[test]
fn yes_or_no_p_rejects_nil_prompt() {
    let mut ev = Context::new();
    let result = builtin_yes_or_no_p(&mut ev, vec![Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn finish_yes_or_no_p_with_minibuffer_retries_until_valid_answer() {
    let mut prompts = Vec::new();
    let mut answers = [Value::string("maybe"), Value::string(" no ")].into_iter();
    let result = finish_yes_or_no_p_with_minibuffer(&[Value::string("Confirm?")], |args| {
        prompts.push(args[0].as_str().unwrap().to_owned());
        Ok(answers.next().expect("enough answers"))
    })
    .unwrap();
    assert_eq!(result, Value::Nil);
    assert_eq!(
        prompts,
        vec![
            "Confirm? (yes or no) ".to_string(),
            "Confirm? (yes or no) ".to_string()
        ]
    );
}

#[test]
fn input_pending_p_returns_nil_without_events() {
    let mut ev = Context::new();
    let result = builtin_input_pending_p(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn input_pending_p_returns_t_with_unread_events() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_input_pending_p(&mut ev, vec![]).unwrap();
    assert_eq!(result, Value::True);
}

#[test]
fn input_pending_p_uses_dynamic_unread_command_events_binding() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let forms = parse_forms("(let ((unread-command-events nil)) (input-pending-p))").unwrap();
    let result = ev.eval_expr(&forms[0]).unwrap();
    assert!(result.is_nil());
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(97)]))
    );
}

#[test]
fn input_pending_p_returns_nil_for_non_list_unread_command_events() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::Int(7));
    let result = builtin_input_pending_p(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn input_pending_p_accepts_optional_check_timers_arg() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo")]),
    );
    let result = builtin_input_pending_p(&mut ev, vec![Value::symbol("timers")]).unwrap();
    assert_eq!(result, Value::True);
}

#[test]
fn input_pending_p_returns_t_with_host_keypress() {
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue keypress");
    ev.input_rx = Some(rx);

    let result = builtin_input_pending_p(&mut ev, vec![]).unwrap();
    assert_eq!(result, Value::True);

    let event = ev.read_char().expect("keypress should remain available");
    assert_eq!(event, Value::Int('a' as i64));
}

#[test]
fn input_pending_p_ignores_focus_events_by_default() {
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::Focus {
        focused: true,
        emacs_frame_id: 0,
    })
    .expect("queue focus event");
    ev.input_rx = Some(rx);

    let result = builtin_input_pending_p(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn input_pending_p_ignores_mouse_move_without_track_mouse() {
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::MouseMove {
        x: 10.0,
        y: 20.0,
        modifiers: crate::keyboard::Modifiers::none(),
        target_frame_id: 0,
    })
    .expect("queue mouse move");
    ev.input_rx = Some(rx);

    let result = builtin_input_pending_p(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn input_pending_p_reports_mouse_move_with_track_mouse() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value("track-mouse", Value::True);
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::MouseMove {
        x: 10.0,
        y: 20.0,
        modifiers: crate::keyboard::Modifiers::none(),
        target_frame_id: 0,
    })
    .expect("queue mouse move");
    ev.input_rx = Some(rx);

    let result = builtin_input_pending_p(&mut ev, vec![]).unwrap();
    assert_eq!(result, Value::True);
}

#[test]
fn read_char_skips_mouse_move_without_track_mouse() {
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::MouseMove {
        x: 10.0,
        y: 20.0,
        modifiers: crate::keyboard::Modifiers::none(),
        target_frame_id: 0,
    })
    .expect("queue mouse move");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue keypress");
    ev.input_rx = Some(rx);

    let result = ev.read_char().expect("keypress should remain readable");
    assert_eq!(result, Value::Int('a' as i64));
}

#[test]
fn read_char_returns_mouse_move_with_track_mouse() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value("track-mouse", Value::True);
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::MouseMove {
        x: 10.0,
        y: 20.0,
        modifiers: crate::keyboard::Modifiers::none(),
        target_frame_id: 0,
    })
    .expect("queue mouse move");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue keypress");
    ev.input_rx = Some(rx);

    let result = ev.read_char().expect("mouse movement should be readable");
    let slots = crate::emacs_core::value::list_to_vec(&result).expect("mouse movement event");
    assert_eq!(slots[0], Value::symbol("mouse-movement"));

    let next = ev.read_char().expect("keypress should remain readable");
    assert_eq!(next, Value::Int('a' as i64));
}

#[test]
fn read_char_mouse_move_updates_mouse_position_even_without_track_mouse() {
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::MouseMove {
        x: 24.0,
        y: 40.0,
        modifiers: crate::keyboard::Modifiers::none(),
        target_frame_id: 0,
    })
    .expect("queue mouse move");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue keypress");
    ev.input_rx = Some(rx);

    let result = ev.read_char().expect("keypress should remain readable");
    assert_eq!(result, Value::Int('a' as i64));

    let pixel = crate::emacs_core::builtins::symbols::builtin_mouse_pixel_position(&mut ev, vec![])
        .expect("mouse-pixel-position should succeed");
    let Value::Cons(cell) = pixel else {
        panic!("expected dotted mouse pixel position");
    };
    let outer = read_cons(cell);
    let Value::Cons(inner) = outer.cdr else {
        panic!("expected inner cons");
    };
    let inner = read_cons(inner);
    assert_eq!(inner.car, Value::Int(24));
    assert_eq!(inner.cdr, Value::Int(40));
}

#[test]
fn input_pending_p_check_timers_does_not_run_timer_when_input_is_already_pending() {
    let mut ev = Context::new();
    let setup = parse_forms(
        r#"(progn
             (setq input-pending-timer-fired nil)
             (fset 'input-pending-timer-callback
                   (lambda () (setq input-pending-timer-fired 'done))))"#,
    )
    .expect("parse input-pending-p timer setup");
    ev.eval_expr(&setup[0])
        .expect("install input-pending-p timer setup");
    ev.timers.add_timer(
        0.0,
        0.0,
        Value::symbol("input-pending-timer-callback"),
        vec![],
        false,
    );

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue keypress");
    ev.input_rx = Some(rx);

    let result = builtin_input_pending_p(&mut ev, vec![Value::True]).unwrap();
    assert_eq!(result, Value::True);
    assert!(
        ev.eval_symbol("input-pending-timer-fired")
            .expect("timer callback flag")
            .is_nil()
    );

    let event = ev.read_char().expect("keypress should remain available");
    assert_eq!(event, Value::Int('a' as i64));
}

#[test]
fn input_pending_p_returns_t_when_quit_flag_is_set() {
    let mut ev = Context::new();
    ev.set_quit_flag_value(Value::True);
    let result = builtin_input_pending_p(&mut ev, vec![]).unwrap();
    assert_eq!(result, Value::True);
}

#[test]
fn input_pending_p_rejects_more_than_one_arg() {
    let mut ev = Context::new();
    let result = builtin_input_pending_p(&mut ev, vec![Value::Nil, Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn discard_input_returns_nil() {
    let mut ev = Context::new();
    let result = builtin_discard_input(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn discard_input_clears_unread_command_events() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_discard_input(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::Nil)
    );
}

#[test]
fn discard_input_uses_dynamic_unread_command_events_binding() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let forms = parse_forms(
        "(let ((unread-command-events (list 98))) (discard-input) unread-command-events)",
    )
    .unwrap();
    let result = ev.eval_expr(&forms[0]).unwrap();
    assert!(result.is_nil());
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(97)]))
    );
}

#[test]
fn discard_input_rejects_args() {
    let mut ev = Context::new();
    let result = builtin_discard_input(&mut ev, vec![Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn current_input_mode_returns_batch_tuple() {
    let mut ev = Context::new();
    let result = builtin_current_input_mode(&mut ev, vec![]).unwrap();
    assert_eq!(
        result,
        Value::list(vec![Value::True, Value::Nil, Value::True, Value::Int(7)])
    );
}

#[test]
fn current_input_mode_rejects_args() {
    let mut ev = Context::new();
    let result = builtin_current_input_mode(&mut ev, vec![Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn set_input_mode_toggles_interrupt_only() {
    let mut ev = Context::new();
    let _ = builtin_set_input_mode(
        &mut ev,
        vec![Value::Nil, Value::True, Value::Nil, Value::Int(65)],
    )
    .unwrap();
    assert_eq!(
        builtin_current_input_mode(&mut ev, vec![]).unwrap(),
        Value::list(vec![Value::Nil, Value::Nil, Value::True, Value::Int(65)])
    );

    let _ = builtin_set_input_mode(
        &mut ev,
        vec![Value::symbol("x"), Value::Nil, Value::Nil, Value::Nil],
    )
    .unwrap();
    assert_eq!(
        builtin_current_input_mode(&mut ev, vec![]).unwrap(),
        Value::list(vec![Value::True, Value::Nil, Value::True, Value::Int(65)])
    );
}

#[test]
fn set_input_mode_rejects_wrong_arity() {
    let mut ev = Context::new();
    let too_few = builtin_set_input_mode(&mut ev, vec![Value::Nil, Value::Nil]);
    assert!(matches!(
        too_few,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));

    let too_many = builtin_set_input_mode(
        &mut ev,
        vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil, Value::Nil],
    );
    assert!(matches!(
        too_many,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn set_input_mode_accepts_three_args() {
    let mut ev = Context::new();
    let result = builtin_set_input_mode(&mut ev, vec![Value::Nil, Value::True, Value::True])
        .expect("set-input-mode should accept 3 args");
    assert!(result.is_nil());
    assert_eq!(
        builtin_current_input_mode(&mut ev, vec![]).unwrap(),
        Value::list(vec![Value::Nil, Value::Nil, Value::True, Value::Int(7)])
    );
}

#[test]
fn set_input_interrupt_mode_toggles_interrupt_state() {
    let mut ev = Context::new();
    let _ = builtin_set_input_interrupt_mode(&mut ev, vec![Value::Nil]).unwrap();
    assert_eq!(
        builtin_current_input_mode(&mut ev, vec![]).unwrap(),
        Value::list(vec![Value::Nil, Value::Nil, Value::True, Value::Int(7)])
    );
    let _ = builtin_set_input_interrupt_mode(&mut ev, vec![Value::symbol("x")]).unwrap();
    assert_eq!(
        builtin_current_input_mode(&mut ev, vec![]).unwrap(),
        Value::list(vec![Value::True, Value::Nil, Value::True, Value::Int(7)])
    );
}

#[test]
fn set_input_interrupt_mode_rejects_wrong_arity() {
    let mut ev = Context::new();
    let result = builtin_set_input_interrupt_mode(&mut ev, vec![Value::Nil, Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn set_input_meta_mode_accepts_one_arg_and_returns_nil() {
    let result = builtin_set_input_meta_mode(vec![Value::Nil]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn set_input_meta_mode_accepts_optional_terminal_arg() {
    let result = builtin_set_input_meta_mode(vec![Value::symbol("encoded"), Value::Nil]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn set_input_meta_mode_rejects_wrong_arity() {
    let result = builtin_set_input_meta_mode(vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
    let result = builtin_set_input_meta_mode(vec![Value::Nil, Value::Nil, Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn set_output_flow_control_accepts_one_arg_and_returns_nil() {
    let result = builtin_set_output_flow_control(vec![Value::True]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn set_output_flow_control_accepts_two_args_and_returns_nil() {
    let result = builtin_set_output_flow_control(vec![Value::True, Value::Nil]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn set_output_flow_control_rejects_wrong_arity() {
    let result = builtin_set_output_flow_control(vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn set_quit_char_accepts_one_arg_and_returns_nil() {
    let mut ev = Context::new();
    let result = builtin_set_quit_char(&mut ev, vec![Value::Int(65)]).unwrap();
    assert!(result.is_nil());
    assert_eq!(
        builtin_current_input_mode(&mut ev, vec![]).unwrap(),
        Value::list(vec![Value::True, Value::Nil, Value::True, Value::Int(65)])
    );
}

#[test]
fn set_quit_char_rejects_non_ascii_values() {
    let mut ev = Context::new();
    let result = builtin_set_quit_char(&mut ev, vec![Value::Int(0o401)]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "error"));
}

#[test]
fn set_quit_char_rejects_wrong_arity() {
    let mut ev = Context::new();
    let result = builtin_set_quit_char(&mut ev, vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn waiting_for_user_input_p_returns_nil() {
    let result = builtin_waiting_for_user_input_p(vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn waiting_for_user_input_p_eval_tracks_runtime_flag() {
    let mut eval = Context::new();
    eval.set_waiting_for_user_input(true);
    let result = builtin_waiting_for_user_input_p_ctx(&mut eval, vec![]).unwrap();
    assert!(matches!(result, Value::True));
}

#[test]
fn waiting_for_user_input_p_rejects_args() {
    let result = builtin_waiting_for_user_input_p(vec![Value::Nil]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn read_char_returns_nil() {
    let mut ev = Context::new();
    let result = builtin_read_char(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn read_char_rejects_non_string_prompt() {
    let mut ev = Context::new();
    let result = builtin_read_char(&mut ev, vec![Value::Int(123)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn read_char_consumes_unread_command_event() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_char(&mut ev, vec![]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(ev.recent_input_events(), &[Value::Int(97)]);
    assert_eq!(ev.read_command_keys(), &[Value::Int(97)]);
}

#[test]
fn read_char_with_seconds_does_not_set_command_keys_when_empty() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_char(&mut ev, vec![Value::Nil, Value::Nil, Value::Int(0)]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(ev.read_command_keys(), &[]);
}

#[test]
fn read_char_with_nil_seconds_sets_command_keys_when_empty() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_char(&mut ev, vec![Value::Nil, Value::Nil, Value::Nil]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(ev.read_command_keys(), &[Value::Int(97)]);
}

#[test]
fn read_char_with_interactive_timeout_returns_nil() {
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);

    let start = std::time::Instant::now();
    let result = builtin_read_char(
        &mut ev,
        vec![Value::Nil, Value::Nil, Value::Float(0.01, next_float_id())],
    )
    .unwrap();
    drop(tx);

    assert!(result.is_nil());
    assert!(start.elapsed() < std::time::Duration::from_millis(250));
}

#[test]
fn read_char_preserves_existing_command_keys_context() {
    let mut ev = Context::new();
    ev.set_read_command_keys(vec![Value::Int(97)]);
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(98)]));
    let result = builtin_read_char(&mut ev, vec![Value::Nil, Value::Nil, Value::Int(0)]).unwrap();
    assert_eq!(result.as_int(), Some(98));
    assert_eq!(ev.read_command_keys(), &[Value::Int(97)]);
}

#[test]
fn read_char_host_quit_char_returns_event_and_sets_quit_flag() {
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('g', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-g");
    ev.input_rx = Some(rx);

    let result = builtin_read_char(&mut ev, vec![]).unwrap();
    assert_eq!(result, Value::Int(7));
    assert_eq!(ev.quit_flag_value(), Value::True);
}

#[test]
fn read_char_signals_error_on_non_character_event() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo")]),
    );
    let result = builtin_read_char(&mut ev, vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
                && sig.data == vec![Value::string("Non-character input-event")]
    ));
    assert_eq!(ev.recent_input_events(), &[Value::symbol("foo")]);
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::symbol("foo")]))
    );
}

#[test]
fn read_char_non_character_truncates_unread_tail_to_offending_event() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::symbol("foo"), Value::Int(97)]),
    );
    let result = builtin_read_char(&mut ev, vec![]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
                && sig.data == vec![Value::string("Non-character input-event")]
    ));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::symbol("foo")]))
    );
    assert_eq!(ev.recent_input_events(), &[Value::symbol("foo")]);
}

#[test]
fn read_char_consumes_character_event_and_preserves_tail() {
    let mut ev = Context::new();
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::Int(97), Value::symbol("foo")]),
    );
    let result = builtin_read_char(&mut ev, vec![]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::symbol("foo")]))
    );
}

#[test]
fn read_char_rejects_more_than_three_args() {
    let mut ev = Context::new();
    let result = builtin_read_char(
        &mut ev,
        vec![
            Value::string("key: "),
            Value::Nil,
            Value::Int(0),
            Value::Nil,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn read_key_consumes_unread_command_event() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_key(&mut ev, vec![]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(ev.read_command_keys(), &[Value::Int(97)]);
}

#[test]
fn read_key_rejects_non_string_prompt() {
    let mut ev = Context::new();
    let result = builtin_read_key(&mut ev, vec![Value::Int(123)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-type-argument"
    ));
}

#[test]
fn read_key_accepts_second_optional_arg() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_key(&mut ev, vec![Value::string("key: "), Value::Int(1)]).unwrap();
    assert_eq!(result.as_int(), Some(97));
}

#[test]
fn read_key_rejects_more_than_two_args() {
    let mut ev = Context::new();
    let result = builtin_read_key(
        &mut ev,
        vec![Value::string("key: "), Value::Nil, Value::Int(123)],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn read_key_returns_non_integer_event() {
    let mut ev = Context::new();
    let event = Value::symbol("f");
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![event]));
    let result = builtin_read_key(&mut ev, vec![Value::string("key: ")]).unwrap();
    assert_eq!(result, event);
    assert_eq!(ev.read_command_keys(), std::slice::from_ref(&event));
}

#[test]
fn read_key_consumes_unread_character_and_keeps_tail() {
    let mut ev = Context::new();
    let event = Value::symbol("foo");
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![event, Value::Int(97)]),
    );
    let result = builtin_read_key(&mut ev, vec![Value::string("key: ")]).unwrap();
    assert_eq!(result, event);
    assert_eq!(ev.read_command_keys(), std::slice::from_ref(&event));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(97)]))
    );
}

#[test]
fn read_key_consumes_character_event_and_preserves_tail() {
    let mut ev = Context::new();
    let event = Value::symbol("foo");
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::Int(97), event]),
    );
    let result = builtin_read_key(&mut ev, vec![Value::string("key: ")]).unwrap();
    assert_eq!(result.as_int(), Some(97));
    assert_eq!(ev.read_command_keys(), &[Value::Int(97)]);
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![event]))
    );
}

#[test]
fn read_key_sequence_returns_empty_string() {
    let mut ev = Context::new();
    let result = builtin_read_key_sequence(&mut ev, vec![Value::string("key: ")]).unwrap();
    assert!(matches!(result, Value::Str(_)) && result.as_str() == Some(""));
}

#[test]
fn read_key_sequence_consumes_unread_command_event() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_key_sequence(&mut ev, vec![Value::string("key: ")]).unwrap();
    assert!(matches!(result, Value::Str(_)) && result.as_str() == Some("a"));
    assert_eq!(ev.read_command_keys(), &[Value::Int(97)]);
}

#[test]
fn read_key_sequence_consumes_non_character_event() {
    let mut ev = Context::new();
    let event = Value::symbol("f");
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![event]));
    let result = builtin_read_key_sequence(&mut ev, vec![Value::string("key: ")]).unwrap();
    match result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], event);
        }
        other => panic!("expected vector event payload, got {other:?}"),
    }
    assert_eq!(ev.read_command_keys(), std::slice::from_ref(&event));
}

#[test]
fn read_key_sequence_consumes_non_character_event_and_preserves_tail() {
    let mut ev = Context::new();
    let event = Value::symbol("foo");
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![event, Value::Int(97)]),
    );
    let result = builtin_read_key_sequence(&mut ev, vec![Value::string("key: ")]).unwrap();
    match result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], event);
        }
        other => panic!("expected vector event payload, got {other:?}"),
    }
    assert_eq!(ev.read_command_keys(), std::slice::from_ref(&event));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(97)]))
    );
}

#[test]
fn read_key_sequence_consumes_character_and_preserves_tail() {
    let mut ev = Context::new();
    let event = Value::symbol("foo");
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::Int(97), event]),
    );
    let result = builtin_read_key_sequence(&mut ev, vec![Value::string("key: ")]).unwrap();
    assert!(matches!(result, Value::Str(_)) && result.as_str() == Some("a"));
    assert_eq!(ev.read_command_keys(), &[Value::Int(97)]);
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![event]))
    );
}

#[test]
fn read_key_sequence_accepts_nil_prompt() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_key_sequence(&mut ev, vec![Value::Nil]).unwrap();
    assert!(matches!(result, Value::Str(_)) && result.as_str() == Some("a"));
}

#[test]
fn read_key_sequence_treats_host_quit_char_as_ordinary_input() {
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('g', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-g");
    ev.input_rx = Some(rx);

    let result = builtin_read_key_sequence(&mut ev, vec![Value::string("key: ")]).unwrap();
    assert!(matches!(result, Value::Str(_)) && result.as_str() == Some("\u{7}"));
    assert!(ev.quit_flag_value().is_nil());
    assert_eq!(ev.read_command_keys(), &[Value::Int(7)]);
}

#[test]
fn read_key_sequence_rejects_more_than_six_args() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_key_sequence(
        &mut ev,
        vec![
            Value::string("key: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn read_key_sequence_vector_returns_empty_vector() {
    let mut ev = Context::new();
    let result = builtin_read_key_sequence_vector(&mut ev, vec![Value::string("key: ")]).unwrap();
    match result {
        Value::Vector(v) => assert!(with_heap(|h| h.get_vector(v).is_empty())),
        other => panic!("expected vector, got {other:?}"),
    }
}

#[derive(Default)]
struct BlockingKeySequenceRuntime {
    unread: VecDeque<Value>,
    read_command_keys: Vec<Value>,
    blocking_keys: Vec<Value>,
    last_options: Option<crate::keyboard::ReadKeySequenceOptions>,
}

impl KeyboardInputRuntime for BlockingKeySequenceRuntime {
    fn pop_unread_command_event(&mut self) -> Option<Value> {
        self.unread.pop_front()
    }

    fn peek_unread_command_event(&self) -> Option<Value> {
        self.unread.front().copied()
    }

    fn replace_unread_command_event_with_singleton(&mut self, event: Value) {
        self.unread.clear();
        self.unread.push_back(event);
    }

    fn record_input_event(&mut self, _event: Value) {}

    fn record_nonmenu_input_event(&mut self, _event: Value) {}

    fn set_read_command_keys(&mut self, keys: Vec<Value>) {
        self.read_command_keys = keys;
    }

    fn clear_read_command_keys(&mut self) {
        self.read_command_keys.clear();
    }

    fn read_command_keys(&self) -> &[Value] {
        &self.read_command_keys
    }

    fn has_input_receiver(&self) -> bool {
        true
    }

    fn read_char_blocking(&mut self) -> Result<Value, Flow> {
        unreachable!("read-char should not be used in this test runtime")
    }

    fn read_char_with_timeout(
        &mut self,
        _timeout: Option<std::time::Duration>,
    ) -> Result<Option<Value>, Flow> {
        unreachable!("read-char should not be used in this test runtime")
    }

    fn read_key_sequence_blocking(
        &mut self,
        options: crate::keyboard::ReadKeySequenceOptions,
    ) -> Result<(Vec<Value>, Value), Flow> {
        self.last_options = Some(options);
        Ok((self.blocking_keys.clone(), Value::Nil))
    }
}

#[test]
fn read_key_sequence_vector_interactive_runtime_returns_blocking_sequence() {
    let mut runtime = BlockingKeySequenceRuntime {
        blocking_keys: vec![Value::Int(97), Value::symbol("f1")],
        ..Default::default()
    };
    let result = finish_read_key_sequence_vector_interactive_in_runtime(
        &mut runtime,
        crate::keyboard::ReadKeySequenceOptions::default(),
    )
    .expect("vector read");
    assert_eq!(
        result,
        Value::vector(vec![Value::Int(97), Value::symbol("f1")])
    );
}

#[test]
fn read_key_sequence_interactive_runtime_passes_prompt_options() {
    let mut runtime = BlockingKeySequenceRuntime {
        blocking_keys: vec![Value::Int(97)],
        ..Default::default()
    };
    let result = finish_read_key_sequence_interactive_in_runtime(
        &mut runtime,
        crate::keyboard::ReadKeySequenceOptions::new(Value::string("Prompt> "), true, true),
    )
    .expect("interactive read");
    assert_eq!(result, Value::string("a"));
    assert_eq!(
        runtime.last_options,
        Some(crate::keyboard::ReadKeySequenceOptions::new(
            Value::string("Prompt> "),
            true,
            true,
        ))
    );
}

#[test]
fn read_key_sequence_vector_consumes_unread_command_event() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_key_sequence_vector(&mut ev, vec![Value::string("key: ")]).unwrap();
    match result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].as_int(), Some(97));
        }
        other => panic!("expected vector, got {other:?}"),
    }
    assert_eq!(ev.read_command_keys(), &[Value::Int(97)]);
}

#[test]
fn read_key_sequence_vector_consumes_non_character_event() {
    let mut ev = Context::new();
    let event = Value::symbol("x");
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![event]));
    let result = builtin_read_key_sequence_vector(&mut ev, vec![Value::string("key: ")]).unwrap();
    match result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], event);
        }
        other => panic!("expected vector event payload, got {other:?}"),
    }
    assert_eq!(ev.read_command_keys(), std::slice::from_ref(&event));
}

#[test]
fn read_key_sequence_vector_consumes_non_character_event_and_preserves_tail() {
    let mut ev = Context::new();
    let event = Value::symbol("bar");
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![event, Value::Int(97)]),
    );
    let result = builtin_read_key_sequence_vector(&mut ev, vec![Value::string("key: ")]).unwrap();
    match result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.len(), 1);
            assert_eq!(items[0], event);
        }
        other => panic!("expected vector, got {other:?}"),
    }
    assert_eq!(ev.read_command_keys(), std::slice::from_ref(&event));
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![Value::Int(97)]))
    );
}

#[test]
fn read_key_sequence_vector_consumes_character_and_preserves_tail() {
    let mut ev = Context::new();
    let event = Value::symbol("bar");
    ev.obarray.set_symbol_value(
        "unread-command-events",
        Value::list(vec![Value::Int(97), event]),
    );
    let result = builtin_read_key_sequence_vector(&mut ev, vec![Value::string("key: ")]).unwrap();
    match result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].as_int(), Some(97));
        }
        other => panic!("expected vector, got {other:?}"),
    }
    assert_eq!(ev.read_command_keys(), &[Value::Int(97)]);
    assert_eq!(
        ev.obarray.symbol_value("unread-command-events"),
        Some(&Value::list(vec![event]))
    );
}

#[test]
fn read_key_sequence_vector_accepts_nil_prompt() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_key_sequence_vector(&mut ev, vec![Value::Nil]).unwrap();
    match result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].as_int(), Some(97));
        }
        other => panic!("expected vector, got {other:?}"),
    }
}

#[test]
fn read_key_sequence_vector_rejects_more_than_six_args() {
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("unread-command-events", Value::list(vec![Value::Int(97)]));
    let result = builtin_read_key_sequence_vector(
        &mut ev,
        vec![
            Value::string("key: "),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

// ===================================================================
// with-output-to-string tests
// ===================================================================

#[test]
fn with_output_to_string_captures_print_output() {
    let mut ev = Context::new();
    let forms =
        parse_forms(r#"(with-output-to-string (princ "a") (prin1 '(1 2)) (print "x"))"#).unwrap();
    let result = ev.eval_expr(&forms[0]).unwrap();
    assert_eq!(result.as_str(), Some("a(1 2)\n\"x\"\n"));
}

#[test]
fn with_output_to_string_keeps_explicit_destination_working() {
    let mut ev = Context::new();
    let forms = parse_forms(
        r#"(with-temp-buffer
             (let ((buf (current-buffer)))
               (with-output-to-string
                 (princ "captured")
                 (princ " to-buf" buf))
               (buffer-string)))"#,
    )
    .unwrap();
    let result = ev.eval_expr(&forms[0]).unwrap();
    assert_eq!(result.as_str(), Some(" to-buf"));
}

// ===================================================================
// Edge case / integration tests
// ===================================================================

#[test]
fn read_from_string_nested_list() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("((a b) (c d))")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(pair.car.is_cons());
            assert!(matches!(&pair.cdr, Value::Int(13)));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_with_leading_whitespace() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("   42")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Int(42)));
            // End position should be 5 (after "   42")
            assert!(matches!(&pair.cdr, Value::Int(5)));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_negative_number() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("-7")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Int(-7)));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_wrong_type() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::Int(42)]);
    assert!(result.is_err());
}

#[test]
fn read_from_string_no_args() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![]);
    assert!(result.is_err());
}

#[test]
fn read_from_string_hash_syntax() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("#xff")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(matches!(&pair.car, Value::Int(255)));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_hash_space_payload_matches_oracle() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("# ")]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-read-syntax");
            assert_eq!(sig.data, vec![Value::string("# ")]);
        }
        other => panic!("expected invalid-read-syntax, got {other:?}"),
    }
}

#[test]
fn read_from_string_hash_unknown_dispatch_payload_matches_oracle() {
    let mut ev = Context::new();

    let result = builtin_read_from_string(&mut ev, vec![Value::string("#a")]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-read-syntax");
            assert_eq!(sig.data, vec![Value::string("#a")]);
        }
        other => panic!("expected invalid-read-syntax, got {other:?}"),
    }

    let result = builtin_read_from_string(&mut ev, vec![Value::string("#0")]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-read-syntax");
            assert_eq!(sig.data, vec![Value::string("#0")]);
        }
        other => panic!("expected invalid-read-syntax, got {other:?}"),
    }
}

#[test]
fn read_from_string_hash_radix_missing_digits_payload_matches_oracle() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("#x")]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-read-syntax");
            assert_eq!(sig.data, vec![Value::string("integer, radix 16")]);
        }
        other => panic!("expected invalid-read-syntax, got {other:?}"),
    }
}

#[test]
fn read_from_string_hash_s_without_list_payload_matches_oracle() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("#s")]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-read-syntax");
            assert_eq!(sig.data, vec![Value::string("#s")]);
        }
        other => panic!("expected invalid-read-syntax, got {other:?}"),
    }
}

#[test]
fn read_from_string_unmatched_close_paren_payload_matches_oracle() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string(")")]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-read-syntax");
            assert_eq!(sig.data, vec![Value::string(")")]);
        }
        other => panic!("expected invalid-read-syntax, got {other:?}"),
    }
}

#[test]
fn read_from_string_char_literal_requires_gnu_emacs_delimiter() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("?child")]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-read-syntax");
            assert_eq!(sig.data, vec![Value::string("?")]);
        }
        other => panic!("expected invalid-read-syntax, got {other:?}"),
    }
}

#[test]
fn read_from_string_hash_skip_without_length_signals_eof() {
    let mut ev = Context::new();

    let result = builtin_read_from_string(&mut ev, vec![Value::string("#@")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));

    let result = builtin_read_from_string(&mut ev, vec![Value::string("#@x")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
}

#[test]
fn read_from_string_hash_skip_with_payload_signals_eof() {
    let mut ev = Context::new();

    let result = builtin_read_from_string(&mut ev, vec![Value::string("#@0x")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));

    let result = builtin_read_from_string(&mut ev, vec![Value::string("#@4data42")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
}

#[test]
fn read_from_string_hash_dollar_uses_load_file_name() {
    let mut ev = Context::new();
    ev.set_variable("load-file-name", Value::string("/tmp/reader-probe.elc"));
    let result = builtin_read_from_string(&mut ev, vec![Value::string("#$")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert_eq!(pair.car.as_str(), Some("/tmp/reader-probe.elc"));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_hash_dollar_defaults_to_nil() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("#$")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert!(pair.car.is_nil());
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_hash_skip_then_hash_dollar_signals_eof() {
    let mut ev = Context::new();
    ev.set_variable("load-file-name", Value::string("/tmp/reader-skip.elc"));
    let result = builtin_read_from_string(&mut ev, vec![Value::string("#@4data#$")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
}

#[test]
fn read_from_string_hash_hash_reads_empty_symbol() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("##")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert_eq!(pair.car.as_symbol_name(), Some(""));
            assert_eq!(pair.cdr, Value::Int(2));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_escaped_hash_hash_reads_literal_symbol() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("\\#\\#")]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert_eq!(pair.car.as_symbol_name(), Some("##"));
            assert_eq!(pair.cdr, Value::Int(4));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_hash_skip_bytes_signals_eof() {
    let mut ev = Context::new();
    let result = builtin_read_from_string(&mut ev, vec![Value::string("#@4data42 rest")]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
}

#[test]
fn read_from_string_hash_bracket_end_position() {
    let mut ev = Context::new();
    let input = "#[(x) \"\\bT\\207\" [x] 1 (#$ . 83)] tail";
    let expected_end = input.find(" tail").unwrap() as i64;
    let result = builtin_read_from_string(&mut ev, vec![Value::string(input)]).unwrap();
    match &result {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            assert_eq!(pair.cdr, Value::Int(expected_end));
        }
        _ => panic!("Expected cons"),
    }
}

#[test]
fn read_from_string_hash_table_literal_returns_hash_table() {
    let mut ev = Context::new();
    let input = "#s(hash-table size 3 test equal data (\"a\" 1 \"b\" 2))";
    let result = builtin_read_from_string(&mut ev, vec![Value::string(input)]).unwrap();
    let Value::Cons(cell) = result else {
        panic!("Expected cons");
    };
    let pair = read_cons(cell);
    let Value::HashTable(table_ref) = &pair.car else {
        panic!("expected hash table object");
    };
    let table = with_heap(|h| h.get_hash_table(*table_ref).clone());
    assert!(matches!(table.test, HashTableTest::Equal));
    assert_eq!(table.size, 3);
    assert_eq!(table.data.len(), 2);
    assert_eq!(table.key_snapshots.len(), 2);
    assert!(matches!(
        table.data.get(&HashKey::from_str("a")),
        Some(Value::Int(1))
    ));
    assert!(matches!(
        table.data.get(&HashKey::from_str("b")),
        Some(Value::Int(2))
    ));
}

#[test]
fn read_buffer_hash_table_literal_returns_hash_table() {
    let mut ev = Context::new();
    let buf_id = ev.buffers.create_buffer(" *reader-hash-table*");
    {
        let buf = ev.buffers.get_mut(buf_id).expect("buffer");
        buf.insert("#s(hash-table size 3 test equal data (\"a\" 1 \"b\" 2))");
        buf.goto_byte(0);
    }
    let value = builtin_read(&mut ev, vec![Value::Buffer(buf_id)]).expect("read from buffer");
    let Value::HashTable(table_ref) = value else {
        panic!("expected hash table object");
    };
    let table = with_heap(|h| h.get_hash_table(table_ref).clone());
    assert!(matches!(table.test, HashTableTest::Equal));
    assert_eq!(table.size, 3);
    assert_eq!(table.data.len(), 2);
    assert!(matches!(
        table.data.get(&HashKey::from_str("a")),
        Some(Value::Int(1))
    ));
    assert!(matches!(
        table.data.get(&HashKey::from_str("b")),
        Some(Value::Int(2))
    ));
}

#[test]
fn read_from_buffer_advances_point_across_multiple_forms() {
    let mut ev = Context::new();
    let buf_id = ev.buffers.create_buffer(" *reader-multi*");
    let source = "(setq reader-first 1)\n(setq reader-second 2)\n";
    {
        let buf = ev.buffers.get_mut(buf_id).expect("buffer");
        buf.insert(source);
        buf.goto_byte(0);
    }

    let first = builtin_read(&mut ev, vec![Value::Buffer(buf_id)]).expect("first form");
    ev.eval_value(&first).expect("first eval");
    let after_first = ev.buffers.get(buf_id).expect("buffer").pt;
    assert!(after_first > 0, "first read should advance point");

    let second = builtin_read(&mut ev, vec![Value::Buffer(buf_id)]).expect("second form");
    ev.eval_value(&second).expect("second eval");
    let after_second = ev.buffers.get(buf_id).expect("buffer").pt;
    assert_eq!(
        after_second,
        source.len() - 1,
        "second read should stop after the form, leaving trailing whitespace unread"
    );

    let eof = builtin_read(&mut ev, vec![Value::Buffer(buf_id)]);
    assert!(matches!(eof, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
    assert_eq!(
        ev.obarray.symbol_value("reader-first").cloned(),
        Some(Value::Int(1))
    );
    assert_eq!(
        ev.obarray.symbol_value("reader-second").cloned(),
        Some(Value::Int(2))
    );
}

#[test]
fn read_from_buffer_preserves_string_literals_during_eval() {
    let mut ev = Context::new();
    let buf_id = ev.buffers.create_buffer(" *reader-string-eval*");
    {
        let buf = ev.buffers.get_mut(buf_id).expect("buffer");
        buf.insert(r#"(progn (setq reader-string nil) (setq reader-string "abc") reader-string)"#);
        buf.goto_byte(0);
    }

    let form = builtin_read(&mut ev, vec![Value::Buffer(buf_id)]).expect("read form");
    let result = ev.eval_value(&form).expect("eval form");
    assert_eq!(result.as_str(), Some("abc"));
}

#[test]
fn read_from_buffer_incomplete_list_signals_end_of_file_like_gnu_emacs() {
    let mut ev = Context::new();
    let buf_id = ev.buffers.create_buffer(" *reader-incomplete-list*");
    {
        let buf = ev.buffers.get_mut(buf_id).expect("buffer");
        buf.insert("(progn (list 1 2)");
        buf.goto_byte(0);
    }

    let result = builtin_read(&mut ev, vec![Value::Buffer(buf_id)]);
    assert!(matches!(result, Err(Flow::Signal(sig)) if sig.symbol_name() == "end-of-file"));
}

#[test]
fn read_from_buffer_invalid_read_syntax_reports_line_and_column_like_gnu_emacs() {
    let mut ev = Context::new();
    let buf_id = ev.buffers.create_buffer(" *reader-invalid-syntax*");
    {
        let buf = ev.buffers.get_mut(buf_id).expect("buffer");
        buf.insert("?child");
        buf.goto_byte(0);
    }

    let result = builtin_read(&mut ev, vec![Value::Buffer(buf_id)]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-read-syntax");
            assert_eq!(
                sig.data,
                vec![Value::string("?"), Value::Int(1), Value::Int(2)]
            );
        }
        other => panic!("expected invalid-read-syntax, got {other:?}"),
    }
}

#[test]
fn read_from_buffer_unmatched_close_paren_reports_post_consumption_column_like_gnu_emacs() {
    let mut ev = Context::new();
    let buf_id = ev.buffers.create_buffer(" *reader-invalid-close-paren*");
    {
        let buf = ev.buffers.get_mut(buf_id).expect("buffer");
        buf.insert(")");
        buf.goto_byte(0);
    }

    let result = builtin_read(&mut ev, vec![Value::Buffer(buf_id)]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-read-syntax");
            assert_eq!(
                sig.data,
                vec![Value::string(")"), Value::Int(1), Value::Int(1)]
            );
        }
        other => panic!("expected invalid-read-syntax, got {other:?}"),
    }
}

#[test]
fn read_from_buffer_invalid_hash_dispatch_reports_post_consumption_column_like_gnu_emacs() {
    let mut ev = Context::new();
    let buf_id = ev.buffers.create_buffer(" *reader-invalid-hash-dispatch*");
    {
        let buf = ev.buffers.get_mut(buf_id).expect("buffer");
        buf.insert("#t");
        buf.goto_byte(0);
    }

    let result = builtin_read(&mut ev, vec![Value::Buffer(buf_id)]);
    match result {
        Err(Flow::Signal(sig)) => {
            assert_eq!(sig.symbol_name(), "invalid-read-syntax");
            assert_eq!(
                sig.data,
                vec![Value::string("#t"), Value::Int(1), Value::Int(2)]
            );
        }
        other => panic!("expected invalid-read-syntax, got {other:?}"),
    }
}

#[test]
fn read_from_string_hash_bracket_preserves_vector() {
    let mut ev = Context::new();
    let input = "#[nil \"\\300\\207\" [0] 1]";
    let result = builtin_read_from_string(&mut ev, vec![Value::string(input)]).unwrap();
    match result {
        Value::Cons(cell) => {
            let pair = read_cons(cell);
            assert!(matches!(pair.car, Value::Vector(_)));
        }
        other => panic!("Expected cons from read-from-string, got {other:?}"),
    }
}

#[test]
fn read_from_string_hash_dollar_inside_dotted_pair_uses_load_file_name() {
    let mut ev = Context::new();
    ev.set_variable("load-file-name", Value::string("/tmp/reader-dotted.elc"));
    let result = builtin_read_from_string(&mut ev, vec![Value::string("(#$ . 83)")]).unwrap();

    match result {
        Value::Cons(cell) => {
            let pair = read_cons(cell);
            let Value::Cons(data_cell) = pair.car else {
                panic!("expected dotted pair");
            };
            let data = read_cons(data_cell);
            assert_eq!(data.car.as_str(), Some("/tmp/reader-dotted.elc"));
            assert_eq!(data.cdr.as_int(), Some(83));
        }
        other => panic!("Expected cons from read-from-string, got {other:?}"),
    }
}
