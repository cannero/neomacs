use super::*;
use crate::emacs_core::autoload::is_autoload_value;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};
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

#[test]
fn rectangle_state_default() {
    let state = RectangleState::new();
    assert!(state.killed.is_empty());
}

#[test]
fn rectangle_state_default_trait() {
    let state = RectangleState::default();
    assert!(state.killed.is_empty());
}

#[test]
fn extract_rectangle_startup_is_autoloaded() {
    let eval = super::super::eval::Evaluator::new();
    let function = eval
        .obarray
        .symbol_function("extract-rectangle")
        .expect("missing extract-rectangle startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn extract_rectangle_loads_from_gnu_rect_el() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abcdef\n123456\n")
             (list (extract-rectangle 1 9)
                   (subrp (symbol-function 'extract-rectangle))))"#,
    );
    assert_eq!(result[0], r#"OK (("a" "1") nil)"#);
}

#[test]
fn extract_rectangle_line_returns_string() {
    let result = builtin_extract_rectangle_line(vec![Value::Int(1), Value::Int(3)]).unwrap();
    assert_eq!(result.as_str(), Some(""));
}

#[test]
fn extract_rectangle_line_with_line_argument() {
    let result =
        builtin_extract_rectangle_line(vec![Value::Int(1), Value::Int(3), Value::string("abcdef")])
            .unwrap();
    assert_eq!(result.as_str(), Some("bc"));
}

#[test]
fn extract_rectangle_line_swapped_columns() {
    let result =
        builtin_extract_rectangle_line(vec![Value::Int(3), Value::Int(1), Value::string("abcdef")])
            .unwrap();
    assert_eq!(result.as_str(), Some("bc"));
}

#[test]
fn extract_rectangle_line_negative_column_errors() {
    assert!(
        builtin_extract_rectangle_line(vec![Value::Int(-1), Value::Int(1), Value::string("abc"),])
            .is_err()
    );
}

#[test]
fn extract_rectangle_line_validates_args() {
    assert!(builtin_extract_rectangle_line(vec![]).is_err());
    assert!(builtin_extract_rectangle_line(vec![Value::Int(1)]).is_err());
    assert!(
        builtin_extract_rectangle_line(vec![Value::Int(1), Value::Int(2), Value::Int(3)]).is_err()
    );
}

#[test]
fn delete_rectangle_wrong_type() {
    let mut eval = super::super::eval::Evaluator::new();
    let result =
        builtin_delete_rectangle(&mut eval, vec![Value::string("not-int"), Value::Int(10)]);
    assert!(result.is_err());
}

#[test]
fn delete_rectangle_returns_nil() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_delete_rectangle(&mut eval, vec![Value::Int(1), Value::Int(10)]);
    assert!(result.is_ok());
    assert!(matches!(result.unwrap(), Value::Int(_)));
}

#[test]
fn delete_rectangle_eval_mutates_buffer() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abcdef\n123456\n");
    }
    let result = builtin_delete_rectangle(&mut eval, vec![Value::Int(1), Value::Int(9)]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Value::Int(7));
    let buffer_after = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist")
        .buffer_string();
    assert_eq!(buffer_after, "bcdef\n23456\n");
}

#[test]
fn kill_rectangle_updates_state() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abcdef\n123456\n");
    }
    let result = builtin_kill_rectangle(&mut eval, vec![Value::Int(1), Value::Int(9)])
        .expect("kill-rectangle");
    assert_eq!(
        result,
        Value::list(vec![Value::string("a"), Value::string("1")])
    );
    assert_eq!(
        eval.rectangle.killed,
        vec!["a".to_string(), "1".to_string()]
    );
    assert_eq!(
        eval.obarray
            .symbol_value("killed-rectangle")
            .cloned()
            .expect("killed-rectangle set"),
        Value::list(vec![Value::string("a"), Value::string("1")])
    );
    let buffer_after = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist")
        .buffer_string();
    assert_eq!(buffer_after, "bcdef\n23456\n");
}

#[test]
fn yank_rectangle_empty_returns_nil() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_yank_rectangle(&mut eval, vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn yank_rectangle_after_kill() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abc\ndef\n");
        buf.goto_char(0);
    }
    eval.rectangle.killed = vec!["X".to_string(), "Y".to_string()];
    let result = builtin_yank_rectangle(&mut eval, vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
    let buf = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist");
    assert_eq!(buf.buffer_string(), "Xabc\nYdef\n");
    assert_eq!(buf.text.byte_to_char(buf.point()) as i64 + 1, 7);
}

#[test]
fn yank_rectangle_uses_killed_rectangle_symbol() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abc\ndef\n");
        buf.goto_char(0);
    }
    eval.obarray.set_symbol_value(
        "killed-rectangle",
        Value::list(vec![Value::string("Q"), Value::string("W")]),
    );
    let result = builtin_yank_rectangle(&mut eval, vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
    let buf = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist");
    assert_eq!(buf.buffer_string(), "Qabc\nWdef\n");
    assert_eq!(buf.text.byte_to_char(buf.point()) as i64 + 1, 7);
}

#[test]
fn yank_rectangle_non_list_symbol_errors() {
    let mut eval = super::super::eval::Evaluator::new();
    eval.obarray
        .set_symbol_value("killed-rectangle", Value::Int(1));
    let result = builtin_yank_rectangle(&mut eval, vec![]);
    assert!(result.is_err());
}

#[test]
fn insert_rectangle_validates_list() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_insert_rectangle(&mut eval, vec![Value::Int(42)]);
    assert!(result.is_err());
}

#[test]
fn insert_rectangle_validates_string_elements() {
    let mut eval = super::super::eval::Evaluator::new();
    let rect = Value::list(vec![Value::string("a"), Value::Int(42)]);
    let result = builtin_insert_rectangle(&mut eval, vec![rect]);
    assert!(result.is_err());
}

#[test]
fn insert_rectangle_valid() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abc\ndef\n");
        buf.goto_char(0);
    }
    let rect = Value::list(vec![Value::string("hello"), Value::string("world")]);
    let result = builtin_insert_rectangle(&mut eval, vec![rect]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
    let buf = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist");
    assert_eq!(buf.buffer_string(), "helloabc\nworlddef\n");
    assert_eq!(buf.text.byte_to_char(buf.point()) as i64 + 1, 15);
}

#[test]
fn insert_rectangle_extends_and_pads_lines() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abc");
        buf.goto_char(1);
    }
    let rect = Value::list(vec![
        Value::string("X"),
        Value::string("Y"),
        Value::string("Z"),
    ]);
    let result = builtin_insert_rectangle(&mut eval, vec![rect]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
    let buf = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist");
    assert_eq!(buf.buffer_string(), "aXbc\n Y\n Z");
    assert_eq!(buf.text.byte_to_char(buf.point()) as i64 + 1, 11);
}

#[test]
fn insert_rectangle_empty_keeps_point_and_text() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abc");
        buf.goto_char(1);
    }
    let result = builtin_insert_rectangle(&mut eval, vec![Value::Nil]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
    let buf = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist");
    assert_eq!(buf.buffer_string(), "abc");
    assert_eq!(buf.text.byte_to_char(buf.point()) as i64 + 1, 2);
}

#[test]
fn open_rectangle_returns_start() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_open_rectangle(&mut eval, vec![Value::Int(1), Value::Int(10)]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Value::Int(1));
}

#[test]
fn open_rectangle_eval_mutates_buffer_and_point() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abcdef\n123456\n");
    }
    let result = builtin_open_rectangle(&mut eval, vec![Value::Int(1), Value::Int(9)])
        .expect("open-rectangle");
    assert_eq!(result, Value::Int(1));
    let buf = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist");
    assert_eq!(buf.buffer_string(), " abcdef\n 123456\n");
    assert_eq!(buf.text.byte_to_char(buf.point()) as i64 + 1, 1);
}

#[test]
fn clear_rectangle_startup_is_autoloaded() {
    let eval = super::super::eval::Evaluator::new();
    let function = eval
        .obarray
        .symbol_function("clear-rectangle")
        .expect("missing clear-rectangle startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn clear_rectangle_loads_from_gnu_rect_el() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abcdef\n123456\n")
             (clear-rectangle 1 9)
             (list (replace-regexp-in-string "\n" "|" (buffer-string) nil t)
                   (point)
                   (subrp (symbol-function 'clear-rectangle))))"#,
    );
    assert_eq!(result[0], r#"OK (" bcdef| 23456|" 15 nil)"#);
}

#[test]
fn string_rectangle_returns_point() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_string_rectangle(
        &mut eval,
        vec![Value::Int(1), Value::Int(10), Value::string("hi")],
    );
    assert!(result.is_ok());
    assert!(matches!(result.unwrap(), Value::Int(_)));
}

#[test]
fn string_rectangle_wrong_type() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_string_rectangle(
        &mut eval,
        vec![
            Value::Int(1),
            Value::Int(10),
            Value::Float(1.5, next_float_id()),
        ],
    );
    assert!(result.is_err());
}

#[test]
fn string_rectangle_eval_mutates_buffer_and_point() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abcdef\n123456\n");
    }
    let result = builtin_string_rectangle(
        &mut eval,
        vec![Value::Int(2), Value::Int(10), Value::string("XX")],
    )
    .expect("string-rectangle");
    assert_eq!(result, Value::Int(12));
    let buf = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist");
    assert_eq!(buf.buffer_string(), "aXXcdef\n1XX3456\n");
    assert_eq!(buf.text.byte_to_char(buf.point()) as i64 + 1, 12);
}

#[test]
fn delete_extract_rectangle_returns_list() {
    let mut eval = super::super::eval::Evaluator::new();
    let result = builtin_delete_extract_rectangle(&mut eval, vec![Value::Int(1), Value::Int(10)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_list());
}

#[test]
fn delete_extract_rectangle_eval_basic_semantics() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abcdef\n123456\n");
    }
    let result = builtin_delete_extract_rectangle(&mut eval, vec![Value::Int(1), Value::Int(9)])
        .expect("delete-extract-rectangle");
    assert_eq!(
        result,
        Value::list(vec![Value::string("a"), Value::string("1")])
    );
    let buffer_after = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist")
        .buffer_string();
    assert_eq!(buffer_after, "bcdef\n23456\n");
}

#[test]
fn delete_extract_rectangle_eval_start_line_order() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abcdef\n123456\n");
    }
    let result = builtin_delete_extract_rectangle(&mut eval, vec![Value::Int(8), Value::Int(7)])
        .expect("delete-extract-rectangle order");
    assert_eq!(result, Value::list(vec![Value::string("123456")]));
    let buffer_after = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist")
        .buffer_string();
    assert_eq!(buffer_after, "abcdef\n\n");
}

#[test]
fn delete_extract_rectangle_eval_clamps_positions() {
    let mut eval = super::super::eval::Evaluator::new();
    {
        let buf = eval
            .buffers
            .current_buffer_mut()
            .expect("current buffer must exist");
        buf.insert("abcdef");
    }
    let result = builtin_delete_extract_rectangle(&mut eval, vec![Value::Int(20), Value::Int(1)])
        .expect("delete-extract-rectangle clamped");
    assert_eq!(result, Value::list(vec![Value::string("abcdef")]));
    let buffer_after = eval
        .buffers
        .current_buffer()
        .expect("current buffer must exist")
        .buffer_string();
    assert_eq!(buffer_after, "");
}

#[test]
fn replace_rectangle_startup_aliases_string_rectangle() {
    let eval = super::super::eval::Evaluator::new();
    assert_eq!(
        eval.obarray
            .symbol_function("replace-rectangle")
            .expect("missing replace-rectangle startup alias")
            .as_symbol_name(),
        Some("string-rectangle")
    );
}

#[test]
fn replace_rectangle_uses_runtime_alias_behavior() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abcdef\n123456\n")
             (replace-rectangle 1 9 "ZZ")
             (list (replace-regexp-in-string "\n" "|" (buffer-string) nil t)
                   (point)
                   (symbol-function 'replace-rectangle)))"#,
    );
    assert_eq!(result[0], r#"OK ("ZZbcdef|ZZ23456|" 11 string-rectangle)"#);
}
