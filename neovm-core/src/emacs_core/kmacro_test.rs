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
// KmacroManager unit tests
// -----------------------------------------------------------------------

#[test]
fn new_manager_defaults() {
    let mgr = KmacroManager::new();
    assert!(!mgr.recording);
    assert!(!mgr.executing);
    assert!(mgr.current_macro.is_empty());
    assert!(mgr.last_macro.is_none());
    assert!(mgr.macro_ring.is_empty());
    assert_eq!(mgr.counter, 0);
    assert_eq!(mgr.counter_format, "%d");
}

#[test]
fn start_stop_recording() {
    let mut mgr = KmacroManager::new();

    // Start recording
    mgr.start_recording(false);
    assert!(mgr.recording);

    // Store some events
    mgr.store_event(Value::Char('a'));
    mgr.store_event(Value::Char('b'));
    mgr.store_event(Value::Char('c'));
    assert_eq!(mgr.current_macro.len(), 3);

    // Stop recording
    let result = mgr.stop_recording();
    assert!(!mgr.recording);
    assert!(result.is_some());
    let recorded = result.unwrap();
    assert_eq!(recorded.len(), 3);

    // last_macro should be set
    assert!(mgr.last_macro.is_some());
    assert_eq!(mgr.last_macro.as_ref().unwrap().len(), 3);

    // current_macro should be cleared
    assert!(mgr.current_macro.is_empty());
}

#[test]
fn stop_recording_empty() {
    let mut mgr = KmacroManager::new();
    mgr.start_recording(false);
    // Don't store any events
    let result = mgr.stop_recording();
    assert!(result.is_none());
    assert!(mgr.last_macro.is_none());
}

#[test]
fn append_recording() {
    let mut mgr = KmacroManager::new();

    // Record first macro
    mgr.start_recording(false);
    mgr.store_event(Value::Char('x'));
    mgr.store_event(Value::Char('y'));
    mgr.stop_recording();

    // Record second macro with append
    mgr.start_recording(true);
    assert_eq!(mgr.current_macro.len(), 2); // starts with previous
    mgr.store_event(Value::Char('z'));
    assert_eq!(mgr.current_macro.len(), 3);
    mgr.stop_recording();

    assert_eq!(mgr.last_macro.as_ref().unwrap().len(), 3);
}

#[test]
fn macro_ring_push() {
    let mut mgr = KmacroManager::new();

    // Record first macro
    mgr.start_recording(false);
    mgr.store_event(Value::Char('a'));
    mgr.stop_recording();
    assert!(mgr.macro_ring.is_empty()); // first macro, nothing to push

    // Record second macro (pushes first onto ring)
    mgr.start_recording(false);
    mgr.store_event(Value::Char('b'));
    mgr.stop_recording();
    assert_eq!(mgr.macro_ring.len(), 1);
    assert_eq!(mgr.macro_ring[0].len(), 1); // the 'a' macro

    // Record third macro (pushes second onto ring)
    mgr.start_recording(false);
    mgr.store_event(Value::Char('c'));
    mgr.stop_recording();
    assert_eq!(mgr.macro_ring.len(), 2);
}

#[test]
fn store_event_not_recording() {
    let mut mgr = KmacroManager::new();
    // Not recording -- store_event should be a no-op
    mgr.store_event(Value::Char('a'));
    assert!(mgr.current_macro.is_empty());
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
    assert!(eval.kmacro.recording);

    // Double-start should error
    let result = builtin_start_kbd_macro(&mut eval, vec![]);
    assert!(result.is_err());

    // Store some events
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('h')]);
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('i')]);

    // End recording
    let result = builtin_end_kbd_macro(&mut eval, vec![]);
    assert!(result.is_ok());
    assert!(!eval.kmacro.recording);
    assert!(eval.kmacro.last_macro.is_some());
    assert_eq!(eval.kmacro.last_macro.as_ref().unwrap().len(), 2);

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
        builtin_defining_kbd_macro(&mut eval, vec![Value::Nil, Value::Nil, Value::Nil]).is_err()
    );

    // APPEND with no prior macro should signal wrong-type-argument.
    let append_without_last = builtin_defining_kbd_macro(&mut eval, vec![Value::True]);
    assert!(append_without_last.is_err());

    // Fresh recording with APPEND=nil should succeed.
    assert_eq!(
        builtin_defining_kbd_macro(&mut eval, vec![Value::Nil]).unwrap(),
        Value::Nil
    );
    assert!(eval.kmacro.recording);

    // Re-entry while recording should signal `error`.
    let already = builtin_defining_kbd_macro(&mut eval, vec![Value::Nil, Value::True]);
    assert!(already.is_err());

    // Finish recording and ensure append path works once a last macro exists.
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('a')]);
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);
    assert!(eval.kmacro.last_macro.is_some());
    assert_eq!(
        builtin_defining_kbd_macro(&mut eval, vec![Value::True, Value::True]).unwrap(),
        Value::Nil
    );
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);
}

#[test]
fn test_start_with_append() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // Record a macro
    let _ = builtin_start_kbd_macro(&mut eval, vec![]);
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('a')]);
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);

    // Append to it
    let _ = builtin_start_kbd_macro(&mut eval, vec![Value::True]);
    assert_eq!(eval.kmacro.current_macro.len(), 1); // 'a' carried over
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('b')]);
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);

    assert_eq!(eval.kmacro.last_macro.as_ref().unwrap().len(), 2);
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
        Value::Nil
    );
    assert_eq!(
        builtin_executing_kbd_macro_p(&mut eval, vec![]).unwrap(),
        Value::Nil
    );

    eval.kmacro.recording = true;
    assert_eq!(
        builtin_defining_kbd_macro_p(&mut eval, vec![]).unwrap(),
        Value::True
    );
    eval.kmacro.recording = false;

    eval.kmacro.executing = true;
    assert_eq!(
        builtin_executing_kbd_macro_p(&mut eval, vec![]).unwrap(),
        Value::True
    );

    assert!(builtin_defining_kbd_macro_p(&mut eval, vec![Value::Nil]).is_err());
    assert!(builtin_executing_kbd_macro_p(&mut eval, vec![Value::Nil]).is_err());
}

#[test]
fn test_last_kbd_macro_builtin() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    assert_eq!(
        builtin_last_kbd_macro(&mut eval, vec![]).unwrap(),
        Value::Nil
    );

    eval.kmacro.last_macro = Some(vec![Value::Char('x'), Value::Char('y')]);
    let value = builtin_last_kbd_macro(&mut eval, vec![]).unwrap();
    match value {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(*items, vec![Value::Char('x'), Value::Char('y')]);
        }
        other => panic!("expected vector, got {other:?}"),
    }

    assert!(builtin_last_kbd_macro(&mut eval, vec![Value::Nil]).is_err());
}

#[test]
fn test_kmacro_p_builtin_subset() {
    assert_eq!(builtin_kmacro_p(vec![Value::Nil]).unwrap(), Value::Nil);
    assert_eq!(
        builtin_kmacro_p(vec![Value::vector(vec![])]).unwrap(),
        Value::True
    );
    assert_eq!(
        builtin_kmacro_p(vec![Value::string("abc")]).unwrap(),
        Value::True
    );
    assert_eq!(builtin_kmacro_p(vec![Value::Int(1)]).unwrap(), Value::Nil);
    assert!(builtin_kmacro_p(vec![]).is_err());
    assert!(builtin_kmacro_p(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn test_kmacro_set_counter_builtin() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    assert_eq!(
        builtin_kmacro_set_counter(&mut eval, vec![Value::Int(42)]).unwrap(),
        Value::Nil
    );
    assert_eq!(eval.kmacro.counter, 42);

    assert_eq!(
        builtin_kmacro_set_counter(&mut eval, vec![Value::Int(-3), Value::Nil]).unwrap(),
        Value::Nil
    );
    assert_eq!(eval.kmacro.counter, -3);

    assert!(builtin_kmacro_set_counter(&mut eval, vec![]).is_err());
    assert!(builtin_kmacro_set_counter(&mut eval, vec![Value::Nil]).is_err());
    assert!(
        builtin_kmacro_set_counter(&mut eval, vec![Value::Int(1), Value::Nil, Value::Nil]).is_err()
    );
}

#[test]
fn test_kmacro_add_counter_builtin() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    eval.kmacro.counter = 10;
    assert_eq!(
        builtin_kmacro_add_counter(&mut eval, vec![Value::Int(5)]).unwrap(),
        Value::Nil
    );
    assert_eq!(eval.kmacro.counter, 15);

    assert_eq!(
        builtin_kmacro_add_counter(&mut eval, vec![Value::Int(-2)]).unwrap(),
        Value::Nil
    );
    assert_eq!(eval.kmacro.counter, 13);

    assert!(builtin_kmacro_add_counter(&mut eval, vec![]).is_err());
    assert!(builtin_kmacro_add_counter(&mut eval, vec![Value::Nil]).is_err());
    assert!(builtin_kmacro_add_counter(&mut eval, vec![Value::Int(1), Value::Nil]).is_err());
}

#[test]
fn test_kmacro_set_format_builtin() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    assert_eq!(eval.kmacro.counter_format, "%d");

    assert_eq!(
        builtin_kmacro_set_format(&mut eval, vec![Value::string("item-%d")]).unwrap(),
        Value::Nil
    );
    assert_eq!(eval.kmacro.counter_format, "item-%d");

    assert_eq!(
        builtin_kmacro_set_format(&mut eval, vec![Value::string("")]).unwrap(),
        Value::Nil
    );
    assert_eq!(eval.kmacro.counter_format, "%d");

    assert!(builtin_kmacro_set_format(&mut eval, vec![]).is_err());
    assert!(builtin_kmacro_set_format(&mut eval, vec![Value::Nil]).is_err());
    assert!(builtin_kmacro_set_format(&mut eval, vec![Value::string("%d"), Value::Nil]).is_err());
}

#[test]
fn test_kmacro_builtin_arity_contracts() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    assert_eq!(
        builtin_start_kbd_macro(&mut eval, vec![Value::Nil, Value::Nil]).unwrap(),
        Value::Nil
    );
    assert!(builtin_start_kbd_macro(&mut eval, vec![Value::Nil, Value::Nil]).is_err());
    assert_eq!(
        builtin_end_kbd_macro(&mut eval, vec![]).unwrap(),
        Value::Nil
    );
    assert!(builtin_start_kbd_macro(&mut eval, vec![Value::Nil, Value::Nil, Value::Nil]).is_err());
    assert!(builtin_end_kbd_macro(&mut eval, vec![Value::Nil, Value::Nil, Value::Nil]).is_err());
    assert!(
        builtin_call_last_kbd_macro(&mut eval, vec![Value::Nil, Value::Nil, Value::Nil]).is_err()
    );
    assert!(builtin_execute_kbd_macro(&mut eval, vec![]).is_err());
    assert!(
        builtin_execute_kbd_macro(
            &mut eval,
            vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil]
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
    eval.kmacro.start_recording(false);
    eval.kmacro
        .store_event(Value::Symbol(intern("forward-char")));
    eval.kmacro.stop_recording();

    // Name it
    let result = builtin_name_last_kbd_macro(&mut eval, vec![Value::symbol("my-macro")]);
    assert!(result.is_ok());

    // Check that the symbol has a function binding
    let func = eval.obarray.symbol_function("my-macro");
    assert!(func.is_some());
    match func.unwrap() {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(*v).clone());
            assert_eq!(items.len(), 1);
        }
        other => panic!("Expected Vector, got {:?}", other),
    }
}

#[test]
fn test_name_last_kbd_macro_wrong_type() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    let result = builtin_name_last_kbd_macro(&mut eval, vec![Value::Int(42)]);
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
    let v = Value::vector(vec![Value::Char('a'), Value::Char('b')]);
    let events = resolve_macro_events(&v).unwrap();
    assert_eq!(events.len(), 2);
}

#[test]
fn test_resolve_macro_events_string() {
    let s = Value::string("hello");
    let events = resolve_macro_events(&s).unwrap();
    assert_eq!(events.len(), 5);
    match &events[0] {
        Value::Char('h') => {}
        other => panic!("Expected Char('h'), got {:?}", other),
    }
}

#[test]
fn test_resolve_macro_events_list() {
    let list = Value::list(vec![Value::Char('x'), Value::Char('y')]);
    let events = resolve_macro_events(&list).unwrap();
    assert_eq!(events.len(), 2);
}

#[test]
fn test_resolve_macro_events_wrong_type() {
    let result = resolve_macro_events(&Value::Int(42));
    assert!(result.is_err());
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
