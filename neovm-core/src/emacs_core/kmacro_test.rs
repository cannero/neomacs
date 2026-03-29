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
        Value::True
    );

    builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('a')]).expect("store a");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('b')]).expect("store b");
    crate::emacs_core::builtins::builtin_cancel_kbd_macro_events(&mut eval, vec![])
        .expect("cancel current command events");
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end");

    assert_eq!(
        eval.command_loop.last_kbd_macro(),
        Some([Value::Char('a')].as_slice())
    );
    assert_eq!(
        builtin_last_kbd_macro(&mut eval, vec![]).expect("last-kbd-macro"),
        Value::vector(vec![Value::Char('a')])
    );
    assert_eq!(
        eval.eval_symbol("last-kbd-macro")
            .expect("last-kbd-macro var"),
        Value::vector(vec![Value::Char('a')])
    );
    assert_eq!(
        eval.eval_symbol("defining-kbd-macro")
            .expect("defining-kbd-macro"),
        Value::Nil
    );
}

#[test]
fn macro_ring_pushes_previous_keyboard_runtime_macro() {
    let mut eval = Context::new();

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start first");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('a')]).expect("store a");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end first");
    assert!(eval.kmacro.macro_ring.is_empty());

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start second");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('b')]).expect("store b");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end second");
    assert_eq!(eval.kmacro.macro_ring, vec![vec![Value::Char('a')]]);

    builtin_start_kbd_macro(&mut eval, vec![]).expect("start third");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('c')]).expect("store c");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end third");
    assert_eq!(
        eval.kmacro.macro_ring,
        vec![vec![Value::Char('a')], vec![Value::Char('b')]]
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
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('h')]);
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('i')]);
    eval.finalize_kbd_macro_runtime_chars();

    // End recording
    let result = builtin_end_kbd_macro(&mut eval, vec![]);
    assert!(result.is_ok());
    assert!(!eval.command_loop.keyboard.kboard.defining_kbd_macro);
    assert_eq!(
        eval.command_loop.last_kbd_macro(),
        Some([Value::Char('h'), Value::Char('i')].as_slice())
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
    assert!(eval.command_loop.keyboard.kboard.defining_kbd_macro);

    // Re-entry while recording should signal `error`.
    let already = builtin_defining_kbd_macro(&mut eval, vec![Value::Nil, Value::True]);
    assert!(already.is_err());

    // Finish recording and ensure append path works once a last macro exists.
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('a')]);
    eval.finalize_kbd_macro_runtime_chars();
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);
    assert_eq!(
        eval.command_loop.last_kbd_macro(),
        Some([Value::Char('a')].as_slice())
    );
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
    eval.finalize_kbd_macro_runtime_chars();
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);

    // Append to it
    let _ = builtin_start_kbd_macro(&mut eval, vec![Value::True]);
    assert_eq!(eval.command_loop.keyboard.kboard.kbd_macro_events.len(), 1);
    let _ = builtin_store_kbd_macro_event(&mut eval, vec![Value::Char('b')]);
    eval.finalize_kbd_macro_runtime_chars();
    let _ = builtin_end_kbd_macro(&mut eval, vec![]);

    assert_eq!(
        eval.command_loop.last_kbd_macro(),
        Some([Value::Char('a'), Value::Char('b')].as_slice())
    );
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

    eval.start_kbd_macro_runtime(None).unwrap();
    assert_eq!(
        builtin_defining_kbd_macro_p(&mut eval, vec![]).unwrap(),
        Value::True
    );
    let _ = eval.end_kbd_macro_runtime().unwrap();

    eval.begin_executing_kbd_macro_runtime(vec![Value::Char('x')]);
    assert_eq!(
        builtin_executing_kbd_macro_p(&mut eval, vec![]).unwrap(),
        Value::True
    );
    eval.finish_executing_kbd_macro_runtime();

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

    eval.command_loop.keyboard.kboard.last_kbd_macro =
        Some(vec![Value::Char('x'), Value::Char('y')]);
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
    builtin_start_kbd_macro(&mut eval, vec![]).expect("start");
    builtin_store_kbd_macro_event(&mut eval, vec![Value::Symbol(intern("forward-char"))])
        .expect("store");
    eval.finalize_kbd_macro_runtime_chars();
    builtin_end_kbd_macro(&mut eval, vec![]).expect("end");

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
