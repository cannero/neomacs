use super::*;

// -----------------------------------------------------------------------
// RegisterManager unit tests
// -----------------------------------------------------------------------

#[test]
fn set_get_clear() {
    let mut mgr = RegisterManager::new();

    // Initially empty
    assert!(mgr.get('a').is_none());

    // Set text
    mgr.set('a', RegisterContent::Text("hello".to_string()));
    assert!(mgr.get('a').is_some());
    assert_eq!(mgr.get_text('a'), Some("hello"));

    // Overwrite
    mgr.set('a', RegisterContent::Number(42));
    assert!(mgr.get_text('a').is_none());
    match mgr.get('a') {
        Some(RegisterContent::Number(42)) => {}
        other => panic!("Expected Number(42), got {:?}", other),
    }

    // Clear
    mgr.clear('a');
    assert!(mgr.get('a').is_none());
}

#[test]
fn clear_all() {
    let mut mgr = RegisterManager::new();
    mgr.set('a', RegisterContent::Text("one".to_string()));
    mgr.set('b', RegisterContent::Text("two".to_string()));
    mgr.set('c', RegisterContent::Number(3));

    assert_eq!(mgr.list().len(), 3);
    mgr.clear_all();
    assert_eq!(mgr.list().len(), 0);
}

#[test]
fn text_append_and_prepend() {
    let mut mgr = RegisterManager::new();

    // Append to empty register creates text
    mgr.append_text('x', "hello", false);
    assert_eq!(mgr.get_text('x'), Some("hello"));

    // Append
    mgr.append_text('x', " world", false);
    assert_eq!(mgr.get_text('x'), Some("hello world"));

    // Prepend
    mgr.append_text('x', ">> ", true);
    assert_eq!(mgr.get_text('x'), Some(">> hello world"));
}

#[test]
fn append_to_non_text_replaces() {
    let mut mgr = RegisterManager::new();
    mgr.set('n', RegisterContent::Number(99));
    mgr.append_text('n', "new text", false);
    assert_eq!(mgr.get_text('n'), Some("new text"));
}

#[test]
fn position_storage() {
    let mut mgr = RegisterManager::new();
    mgr.set(
        'p',
        RegisterContent::Position {
            buffer: "*scratch*".to_string(),
            point: 42,
        },
    );
    match mgr.get('p') {
        Some(RegisterContent::Position { buffer, point }) => {
            assert_eq!(buffer, "*scratch*");
            assert_eq!(*point, 42);
        }
        other => panic!("Expected Position, got {:?}", other),
    }
}

#[test]
fn list_registers_sorted() {
    let mut mgr = RegisterManager::new();
    mgr.set('z', RegisterContent::Text("z-text".to_string()));
    mgr.set('a', RegisterContent::Number(1));
    mgr.set('m', RegisterContent::File("/tmp/foo".to_string()));

    let list = mgr.list();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].0, 'a');
    assert_eq!(list[0].1, "number");
    assert_eq!(list[1].0, 'm');
    assert_eq!(list[1].1, "file");
    assert_eq!(list[2].0, 'z');
    assert_eq!(list[2].1, "text");
}

#[test]
fn rectangle_and_kbd_macro() {
    let mut mgr = RegisterManager::new();

    let rect = vec![
        "line1".to_string(),
        "line2".to_string(),
        "line3".to_string(),
    ];
    mgr.set('r', RegisterContent::Rectangle(rect));
    match mgr.get('r') {
        Some(RegisterContent::Rectangle(lines)) => assert_eq!(lines.len(), 3),
        other => panic!("Expected Rectangle, got {:?}", other),
    }

    let macro_keys = vec![Value::Char('a'), Value::Char('b')];
    mgr.set('k', RegisterContent::KbdMacro(macro_keys));
    match mgr.get('k') {
        Some(RegisterContent::KbdMacro(keys)) => assert_eq!(keys.len(), 2),
        other => panic!("Expected KbdMacro, got {:?}", other),
    }
}

// -----------------------------------------------------------------------
// Builtin-level tests
// -----------------------------------------------------------------------

#[test]
fn test_expect_register() {
    // Char
    assert_eq!(expect_register(&Value::Char('a')).unwrap(), 'a');

    // Int (ASCII code)
    assert_eq!(expect_register(&Value::Int(65)).unwrap(), 'A');

    // Single-char string
    assert_eq!(expect_register(&Value::string("z")).unwrap(), 'z');

    // Multi-char string is an error
    assert!(expect_register(&Value::string("ab")).is_err());

    // Float is an error
    assert!(expect_register(&Value::Float(1.0, next_float_id())).is_err());
}

#[test]
fn test_builtin_copy_and_insert() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // copy-to-register
    let result = builtin_copy_to_register(
        &mut eval,
        vec![Value::Char('a'), Value::string("hello world")],
    );
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());

    // insert-register -> returns the text
    let result = builtin_insert_register(&mut eval, vec![Value::Char('a')]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_str(), Some("hello world"));

    // insert-register on empty register -> error
    let result = builtin_insert_register(&mut eval, vec![Value::Char('z')]);
    assert!(result.is_err());
}

#[test]
fn test_builtin_number_and_increment() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // number-to-register
    let result = builtin_number_to_register(&mut eval, vec![Value::Int(10), Value::Char('n')]);
    assert!(result.is_ok());

    // get-register -> returns 10
    let result = builtin_get_register(&mut eval, vec![Value::Char('n')]);
    assert!(result.is_ok());
    assert!(matches!(result.unwrap(), Value::Int(10)));

    // increment-register by 5
    let result = builtin_increment_register(&mut eval, vec![Value::Int(5), Value::Char('n')]);
    assert!(result.is_ok());

    // Now should be 15
    let result = builtin_get_register(&mut eval, vec![Value::Char('n')]);
    assert!(result.is_ok());
    assert!(matches!(result.unwrap(), Value::Int(15)));
}

#[test]
fn test_builtin_increment_empty_register() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // Incrementing empty register starts from 0
    let result = builtin_increment_register(&mut eval, vec![Value::Int(7), Value::Char('e')]);
    assert!(result.is_ok());

    let result = builtin_get_register(&mut eval, vec![Value::Char('e')]);
    assert!(result.is_ok());
    assert!(matches!(result.unwrap(), Value::Int(7)));
}

#[test]
fn test_builtin_set_and_get_register() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // Set string
    let result = builtin_set_register(
        &mut eval,
        vec![Value::Char('s'), Value::string("saved text")],
    );
    assert!(result.is_ok());

    let result = builtin_get_register(&mut eval, vec![Value::Char('s')]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_str(), Some("saved text"));

    // Set nil clears
    let result = builtin_set_register(&mut eval, vec![Value::Char('s'), Value::Nil]);
    assert!(result.is_ok());

    let result = builtin_get_register(&mut eval, vec![Value::Char('s')]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn test_builtin_view_register() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // Empty register
    let result = builtin_view_register(&mut eval, vec![Value::Char('v')]);
    assert!(result.is_ok());
    let desc = result.unwrap();
    assert!(desc.as_str().unwrap().contains("empty"));

    // Text register
    eval.registers
        .set('v', RegisterContent::Text("some text".to_string()));
    let result = builtin_view_register(&mut eval, vec![Value::Char('v')]);
    assert!(result.is_ok());
    let desc = result.unwrap();
    assert!(desc.as_str().unwrap().contains("text"));
    assert!(desc.as_str().unwrap().contains("some text"));

    // Number register
    eval.registers.set('v', RegisterContent::Number(99));
    let result = builtin_view_register(&mut eval, vec![Value::Char('v')]);
    assert!(result.is_ok());
    let desc = result.unwrap();
    assert!(desc.as_str().unwrap().contains("99"));
}

#[test]
fn test_builtin_register_to_string() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // Empty register => nil
    let empty = builtin_register_to_string(&mut eval, vec![Value::Char('r')]).unwrap();
    assert!(empty.is_nil());

    // Text register => string
    builtin_set_register(&mut eval, vec![Value::Char('r'), Value::string("abc")]).unwrap();
    let text = builtin_register_to_string(&mut eval, vec![Value::Char('r')]).unwrap();
    assert_eq!(text.as_str(), Some("abc"));
}

#[test]
fn test_wrong_arg_count() {
    use super::super::eval::Context;

    let mut eval = Context::new();

    // copy-to-register needs at least 2 args
    let result = builtin_copy_to_register(&mut eval, vec![Value::Char('a')]);
    assert!(result.is_err());

    // point-to-register needs exactly 1 arg
    let result = builtin_point_to_register(&mut eval, vec![]);
    assert!(result.is_err());
}
