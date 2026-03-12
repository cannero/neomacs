use super::*;
use crate::emacs_core::autoload::is_autoload_value;
use crate::emacs_core::bytecode::opcode::Op;
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
fn delete_rectangle_startup_is_autoloaded() {
    let eval = super::super::eval::Evaluator::new();
    let function = eval
        .obarray
        .symbol_function("delete-rectangle")
        .expect("missing delete-rectangle startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn delete_rectangle_loads_from_gnu_rect_el() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abcdef\n123456\n")
             (delete-rectangle 1 9)
             (list (replace-regexp-in-string "\n" "|" (buffer-string) nil t)
                   (point)
                   (subrp (symbol-function 'delete-rectangle))))"#,
    );
    assert_eq!(result[0], r#"OK ("bcdef|23456|" 13 nil)"#);
}

#[test]
fn kill_rectangle_startup_is_autoloaded() {
    let eval = super::super::eval::Evaluator::new();
    let function = eval
        .obarray
        .symbol_function("kill-rectangle")
        .expect("missing kill-rectangle startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn kill_rectangle_loads_from_gnu_rect_el() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abcdef\n123456\n")
             (list (kill-rectangle 1 9)
                   (replace-regexp-in-string "\n" "|" (buffer-string) nil t)
                   (point)
                   killed-rectangle
                   (subrp (symbol-function 'kill-rectangle))))"#,
    );
    assert_eq!(
        result[0],
        r#"OK (("a" "1") "bcdef|23456|" 13 ("a" "1") nil)"#
    );
}

#[test]
fn yank_rectangle_startup_is_autoloaded() {
    let eval = super::super::eval::Evaluator::new();
    let function = eval
        .obarray
        .symbol_function("yank-rectangle")
        .expect("missing yank-rectangle startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn yank_rectangle_loads_from_gnu_rect_el() {
    let result = bootstrap_eval_all(
        r#"(progn
             (setq killed-rectangle '("X" "Y"))
             (with-temp-buffer
               (insert "abc\ndef\n")
               (goto-char (point-min))
               (list (yank-rectangle)
                     (replace-regexp-in-string "\n" "|" (buffer-string) nil t)
                     (point)
                     (subrp (symbol-function 'yank-rectangle)))))"#,
    );
    assert_eq!(result[0], r#"OK (nil "Xabc|Ydef|" 7 nil)"#);
}

#[test]
fn yank_rectangle_loaded_rejects_non_list_killed_rectangle() {
    let result = bootstrap_eval_all(
        r#"(progn
             (setq killed-rectangle 1)
             (condition-case err
                 (yank-rectangle)
               (error (list 'err (car err)))))"#,
    );
    assert_eq!(result[0], r#"OK (err wrong-type-argument)"#);
}

#[test]
fn yank_rectangle_loaded_function_is_simple_bytecode_call() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let forms = parse_forms(r#"(load "rect")"#).expect("parse rect load");
    let results = eval.eval_forms(&forms);
    assert!(
        results.iter().all(Result::is_ok),
        "rect load failed: {:?}",
        results.iter().map(format_eval_result).collect::<Vec<_>>()
    );

    let function = eval
        .obarray
        .symbol_function("yank-rectangle")
        .cloned()
        .expect("loaded yank-rectangle function cell");
    let bytecode = function
        .get_bytecode_data()
        .expect("yank-rectangle should load as bytecode");

    assert_eq!(
        bytecode
            .constants
            .iter()
            .map(Value::as_symbol_name)
            .collect::<Vec<_>>(),
        vec![Some("killed-rectangle"), Some("insert-rectangle")]
    );
    assert_eq!(
        bytecode.ops,
        vec![Op::Constant(1), Op::VarRef(0), Op::Call(1), Op::Return]
    );
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
fn delete_extract_rectangle_startup_is_autoloaded() {
    let eval = super::super::eval::Evaluator::new();
    let function = eval
        .obarray
        .symbol_function("delete-extract-rectangle")
        .expect("missing delete-extract-rectangle startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn delete_extract_rectangle_loads_from_gnu_rect_el() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abcdef\n123456\n")
             (list (delete-extract-rectangle 1 9)
                   (replace-regexp-in-string "\n" "|" (buffer-string) nil t)
                   (point)
                   (subrp (symbol-function 'delete-extract-rectangle))))"#,
    );
    assert_eq!(result[0], r#"OK (("a" "1") "bcdef|23456|" 13 nil)"#);
}

#[test]
fn delete_extract_rectangle_after_explicit_rect_load_matches_gnu() {
    let result = bootstrap_eval_all(
        r#"(progn
             (load "rect")
             (defvar neovm--orig-derl nil)
             (defvar neovm--trace nil)
             (setq neovm--orig-derl (symbol-function 'delete-extract-rectangle-line))
             (fset 'delete-extract-rectangle-line
                   (lambda (startcol endcol lines fill)
                     (setq neovm--trace
                           (cons (list :before
                                       (point)
                                       startcol
                                       endcol
                                       (replace-regexp-in-string "\n" "|" (buffer-string) nil t))
                                 neovm--trace))
                     (prog1
                         (funcall neovm--orig-derl startcol endcol lines fill)
                       (setq neovm--trace
                             (cons (list :after
                                         (point)
                                         startcol
                                         endcol
                                         (car (cdr lines))
                                         (replace-regexp-in-string "\n" "|" (buffer-string) nil t))
                                   neovm--trace)))))
             (with-temp-buffer
               (insert "abcdef\n123456\n")
               (list (delete-extract-rectangle 1 9)
                     (replace-regexp-in-string "\n" "|" (buffer-string) nil t)
                     (point)
                     (nreverse neovm--trace))))"#,
    );
    assert_eq!(
        result[0],
        r#"OK (("a" "1") "bcdef|23456|" 13 ((:before 1 0 1 "abcdef|123456|") (:after 1 0 1 "a" "bcdef|123456|") (:before 7 0 1 "bcdef|123456|") (:after 7 0 1 "1" "bcdef|23456|")))"#
    );
}

#[test]
fn delete_extract_rectangle_line_loaded_state_matches_gnu() {
    let result = bootstrap_eval_all(
        r#"(progn
             (load "rect")
             (with-temp-buffer
               (insert "abcdef\n123456\n")
               (let* ((startcol 0)
                      (endcol 1)
                      (startpt (progn (goto-char 1) (line-beginning-position)))
                      (endpt (progn (goto-char 9) (copy-marker (line-end-position))))
                      (states nil)
                      (lines (list nil)))
                 (goto-char startpt)
                 (while (progn
                          (setq states (cons (list :before (point) (marker-position endpt)) states))
                          (delete-extract-rectangle-line startcol endcol lines nil)
                          (setq states
                                (cons (list :after
                                            (point)
                                            (marker-position endpt)
                                            (car (cdr lines))
                                            (replace-regexp-in-string "\n" "|" (buffer-string) nil t))
                                      states))
                          (and (= 0 (forward-line 1)) (bolp) (<= (point) endpt))))
                 (list (nreverse states) (nreverse (cdr lines))))))"#,
    );
    assert_eq!(
        result[0],
        r#"OK (((:before 1 14) (:after 1 13 "a" "bcdef|123456|") (:before 7 13) (:after 7 12 "1" "bcdef|23456|")) ("a" "1"))"#
    );
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
