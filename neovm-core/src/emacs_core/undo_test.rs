use super::*;

#[test]
fn test_undo_boundary_no_args() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let result = builtin_undo_boundary(&mut eval, vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn test_undo_boundary_eval_inserts_boundary_marker() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("x");
    }
    let result = builtin_undo_boundary(&mut eval, vec![]);
    assert!(result.is_ok());
    let buffer = eval.buffers.current_buffer().expect("scratch buffer");
    let ul = buffer.get_undo_list();
    assert!(crate::buffer::undo_list_has_trailing_boundary(&ul));
}

#[test]
fn test_undo_boundary_wrong_args() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let result = builtin_undo_boundary(&mut eval, vec![Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn test_primitive_undo_with_count_and_list() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    let mut eval = Context::new();
    let list = Value::list(vec![Value::NIL, Value::NIL, Value::NIL]);
    let result = builtin_primitive_undo(&mut eval, vec![Value::fixnum(1), list]);
    assert!(result.is_ok());
    // All-nil list: one group of nothing returns unconsumed tail.
}

#[test]
fn test_primitive_undo_zero_count() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    let mut eval = Context::new();
    let list = Value::list(vec![Value::NIL, Value::NIL]);
    let result = builtin_primitive_undo(&mut eval, vec![Value::fixnum(0), list]);
    assert!(result.is_ok());
    // Zero count returns list unchanged.
    assert_eq!(format!("{:?}", result.unwrap()), format!("{:?}", list));
}

#[test]
fn test_primitive_undo_negative_count() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    let mut eval = Context::new();
    let list = Value::list(vec![Value::NIL]);
    let result = builtin_primitive_undo(&mut eval, vec![Value::fixnum(-5), list]);
    assert!(result.is_ok());
    // Negative count returns list unchanged.
    assert_eq!(format!("{:?}", result.unwrap()), format!("{:?}", list));
}

#[test]
fn test_primitive_undo_invalid_count() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    let mut eval = Context::new();
    let list = Value::list(vec![]);
    let result = builtin_primitive_undo(&mut eval, vec![Value::make_float(1.5), list]);
    assert!(result.is_err());
}

#[test]
fn test_primitive_undo_non_list_signals_wrong_type() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    let mut eval = Context::new();
    let result = builtin_primitive_undo(&mut eval, vec![Value::fixnum(1), Value::fixnum(7)]);
    assert!(result.is_err());
}

#[test]
fn test_primitive_undo_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    let mut eval = Context::new();
    let result = builtin_primitive_undo(&mut eval, vec![Value::fixnum(1)]);
    assert!(result.is_err());

    let result = builtin_primitive_undo(&mut eval, vec![]);
    assert!(result.is_err());

    let result = builtin_primitive_undo(&mut eval, vec![Value::fixnum(1), Value::NIL, Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn test_primitive_undo_reverts_insertion() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    let mut eval = Context::new();
    // Insert text into the current buffer.
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("hello");
    }
    // Build an undo list that describes the insertion: (1 . 6)
    // meaning bytes [1,6) were inserted (1-indexed).
    let entry = Value::cons(Value::fixnum(1), Value::fixnum(6));
    let list = Value::cons(entry, Value::NIL);
    let result = builtin_primitive_undo(&mut eval, vec![Value::fixnum(1), list]);
    assert!(result.is_ok());
    let contents = eval
        .buffers
        .current_buffer()
        .expect("scratch buffer")
        .buffer_string();
    assert_eq!(contents, "");
}

#[test]
fn test_primitive_undo_reverts_deletion() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    let mut eval = Context::new();
    // Buffer starts empty; the undo entry says "hello" was deleted at pos 1.
    let entry = Value::cons(Value::string("hello"), Value::fixnum(1));
    let list = Value::cons(entry, Value::NIL);
    let result = builtin_primitive_undo(&mut eval, vec![Value::fixnum(1), list]);
    assert!(result.is_ok());
    let contents = eval
        .buffers
        .current_buffer()
        .expect("scratch buffer")
        .buffer_string();
    assert_eq!(contents, "hello");
}

#[test]
fn test_primitive_undo_reverts_raw_unibyte_deletion() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    let mut eval = Context::new();
    eval.buffers
        .current_buffer_mut()
        .expect("scratch buffer")
        .set_multibyte_value(false);
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF]));
    let entry = Value::cons(raw, Value::fixnum(1));
    let list = Value::cons(entry, Value::NIL);
    let result = builtin_primitive_undo(&mut eval, vec![Value::fixnum(1), list]);
    assert!(result.is_ok());
    let contents = eval
        .buffers
        .current_buffer()
        .expect("scratch buffer")
        .buffer_substring_lisp_string(0, 1);
    assert!(!contents.is_multibyte());
    assert_eq!(contents.as_bytes(), &[0xFF]);
}

#[test]
fn test_undo_no_args() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();
    let result = builtin_undo(&mut eval, vec![]);
    assert!(result.is_err());
}

#[test]
fn test_undo_with_arg() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();
    let result = builtin_undo(&mut eval, vec![Value::fixnum(5)]);
    assert!(result.is_err());
}

#[test]
fn test_undo_with_invalid_arg() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();
    let result = builtin_undo(&mut eval, vec![Value::make_float(1.5)]);
    assert!(result.is_err());
}

#[test]
fn test_undo_with_multiple_args() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();
    let result = builtin_undo(&mut eval, vec![Value::fixnum(2), Value::fixnum(3)]);
    assert!(result.is_err());
}

#[test]
fn test_undo_reverts_inserted_text() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("abc");
        let mut ul = buffer.get_undo_list();
        crate::buffer::undo::undo_list_boundary(&mut ul);
        buffer.set_undo_list(ul);
    }
    let result = builtin_undo(&mut eval, vec![Value::fixnum(1)]);
    assert!(result.is_ok());
    let contents = eval
        .buffers
        .current_buffer()
        .expect("scratch buffer")
        .buffer_string();
    assert_eq!(contents, "");
}

#[test]
fn test_undo_without_boundary_signals_user_error_after_apply() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("x");
    }
    let result = builtin_undo(&mut eval, vec![Value::fixnum(1)]);
    assert!(result.is_err());
    let contents = eval
        .buffers
        .current_buffer()
        .expect("scratch buffer")
        .buffer_string();
    assert_eq!(contents, "");
}

#[test]
fn test_undo_with_non_positive_arg_and_boundary_returns_undo() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let mut eval = Context::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("x");
        let mut ul = buffer.get_undo_list();
        crate::buffer::undo::undo_list_boundary(&mut ul);
        buffer.set_undo_list(ul);
    }
    let result = builtin_undo(&mut eval, vec![Value::fixnum(0)]).unwrap();
    assert_eq!(result, Value::string("Undo"));
    let contents = eval
        .buffers
        .current_buffer()
        .expect("scratch buffer")
        .buffer_string();
    assert_eq!(contents, "x");
}
