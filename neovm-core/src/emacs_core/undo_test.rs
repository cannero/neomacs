use super::*;

#[test]
fn test_undo_boundary_no_args() {
    let result = builtin_undo_boundary_inner(vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn test_undo_boundary_eval_inserts_boundary_marker() {
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
    let result = builtin_undo_boundary_inner(vec![Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn test_primitive_undo_with_count_and_list() {
    let list = Value::list(vec![Value::Nil, Value::Nil, Value::Nil]);
    let result = builtin_primitive_undo(vec![Value::Int(1), list]);
    assert!(result.is_ok());
    // Stub returns list unchanged
    assert_eq!(format!("{:?}", result.unwrap()), format!("{:?}", list));
}

#[test]
fn test_primitive_undo_zero_count() {
    let list = Value::list(vec![Value::Nil, Value::Nil]);
    let result = builtin_primitive_undo(vec![Value::Int(0), list]);
    assert!(result.is_ok());
    assert_eq!(format!("{:?}", result.unwrap()), format!("{:?}", list));
}

#[test]
fn test_primitive_undo_negative_count() {
    let list = Value::list(vec![Value::Nil]);
    let result = builtin_primitive_undo(vec![Value::Int(-5), list]);
    assert!(result.is_ok());
    // Negative count still returns list
    assert_eq!(format!("{:?}", result.unwrap()), format!("{:?}", list));
}

#[test]
fn test_primitive_undo_invalid_count() {
    let list = Value::list(vec![]);
    let result = builtin_primitive_undo(vec![Value::Float(1.5, next_float_id()), list]);
    assert!(result.is_err());
}

#[test]
fn test_primitive_undo_non_list_signals_wrong_type() {
    let result = builtin_primitive_undo(vec![Value::Int(1), Value::Int(7)]);
    assert!(result.is_err());
}

#[test]
fn test_primitive_undo_wrong_arg_count() {
    let result = builtin_primitive_undo(vec![Value::Int(1)]);
    assert!(result.is_err());

    let result = builtin_primitive_undo(vec![]);
    assert!(result.is_err());

    let result = builtin_primitive_undo(vec![Value::Int(1), Value::Nil, Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn test_undo_no_args() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let result = builtin_undo(&mut eval, vec![]);
    assert!(result.is_err());
}

#[test]
fn test_undo_with_arg() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let result = builtin_undo(&mut eval, vec![Value::Int(5)]);
    assert!(result.is_err());
}

#[test]
fn test_undo_with_invalid_arg() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let result = builtin_undo(&mut eval, vec![Value::Float(1.5, next_float_id())]);
    assert!(result.is_err());
}

#[test]
fn test_undo_with_multiple_args() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    let result = builtin_undo(&mut eval, vec![Value::Int(2), Value::Int(3)]);
    assert!(result.is_err());
}

#[test]
fn test_undo_reverts_inserted_text() {
    use super::super::eval::Context;

    let mut eval = Context::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("abc");
        let mut ul = buffer.get_undo_list();
        crate::buffer::undo::undo_list_boundary(&mut ul);
        buffer.set_undo_list(ul);
    }
    let result = builtin_undo(&mut eval, vec![Value::Int(1)]);
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
    use super::super::eval::Context;

    let mut eval = Context::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("x");
    }
    let result = builtin_undo(&mut eval, vec![Value::Int(1)]);
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
    use super::super::eval::Context;

    let mut eval = Context::new();
    {
        let buffer = eval.buffers.current_buffer_mut().expect("scratch buffer");
        buffer.insert("x");
        let mut ul = buffer.get_undo_list();
        crate::buffer::undo::undo_list_boundary(&mut ul);
        buffer.set_undo_list(ul);
    }
    let result = builtin_undo(&mut eval, vec![Value::Int(0)]).unwrap();
    assert_eq!(result, Value::string("Undo"));
    let contents = eval
        .buffers
        .current_buffer()
        .expect("scratch buffer")
        .buffer_string();
    assert_eq!(contents, "x");
}
