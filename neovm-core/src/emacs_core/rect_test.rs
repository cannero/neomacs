use super::*;
use crate::emacs_core::autoload::is_autoload_value;
use crate::emacs_core::bytecode::opcode::Op;
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

fn eval_with_ldefs_boot_autoloads(names: &[&str]) -> Evaluator {
    let mut eval = Evaluator::new();
    for name in names {
        eval.obarray_mut().fmakunbound(name);
    }
    apply_ldefs_boot_autoloads_for_names(&mut eval, names).expect("ldefs-boot autoload restore");
    eval
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
    let eval = eval_with_ldefs_boot_autoloads(&["extract-rectangle"]);
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
    let eval = eval_with_ldefs_boot_autoloads(&["delete-rectangle"]);
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
    let eval = eval_with_ldefs_boot_autoloads(&["kill-rectangle"]);
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
    let eval = eval_with_ldefs_boot_autoloads(&["yank-rectangle"]);
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
fn insert_rectangle_startup_is_autoloaded() {
    let eval = eval_with_ldefs_boot_autoloads(&["insert-rectangle"]);
    let function = eval
        .obarray
        .symbol_function("insert-rectangle")
        .expect("missing insert-rectangle startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn insert_rectangle_loads_from_gnu_rect_el() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abc\ndef\n")
             (goto-char (point-min))
             (list (insert-rectangle '("hello" "world"))
                   (replace-regexp-in-string "\n" "|" (buffer-string) nil t)
                   (point)
                   (subrp (symbol-function 'insert-rectangle))))"#,
    );
    assert_eq!(result[0], r#"OK (nil "helloabc|worlddef|" 15 nil)"#);
}

#[test]
fn insert_rectangle_loaded_rejects_non_list_argument() {
    let result = bootstrap_eval_all(
        r#"(condition-case err
               (insert-rectangle 42)
             (error (list 'err (car err))))"#,
    );
    assert_eq!(result[0], r#"OK (err wrong-type-argument)"#);
}

#[test]
fn insert_rectangle_loaded_rejects_non_string_elements() {
    let result = bootstrap_eval_all(
        r#"(condition-case err
               (insert-rectangle '("a" 42))
             (error (list 'err (car err))))"#,
    );
    assert_eq!(result[0], r#"OK (err wrong-type-argument)"#);
}

#[test]
fn open_rectangle_startup_is_autoloaded() {
    let eval = eval_with_ldefs_boot_autoloads(&["open-rectangle"]);
    let function = eval
        .obarray
        .symbol_function("open-rectangle")
        .expect("missing open-rectangle startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn open_rectangle_loads_from_gnu_rect_el() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abcdef\n123456\n")
             (list (open-rectangle 1 9)
                   (replace-regexp-in-string "\n" "|" (buffer-string) nil t)
                   (point)
                   (subrp (symbol-function 'open-rectangle))))"#,
    );
    assert_eq!(result[0], r#"OK (1 " abcdef| 123456|" 1 nil)"#);
}

#[test]
fn clear_rectangle_startup_is_autoloaded() {
    let eval = eval_with_ldefs_boot_autoloads(&["clear-rectangle"]);
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
fn string_rectangle_startup_is_autoloaded() {
    let eval = eval_with_ldefs_boot_autoloads(&["string-rectangle"]);
    let function = eval
        .obarray
        .symbol_function("string-rectangle")
        .expect("missing string-rectangle startup function cell");
    assert!(is_autoload_value(&function));
}

#[test]
fn string_rectangle_loads_from_gnu_rect_el() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abcdef\n123456\n")
             (list (string-rectangle 2 10 "XX")
                   (replace-regexp-in-string "\n" "|" (buffer-string) nil t)
                   (point)
                   (subrp (symbol-function 'string-rectangle))))"#,
    );
    assert_eq!(result[0], r#"OK (12 "aXXcdef|1XX3456|" 12 nil)"#);
}

#[test]
fn string_rectangle_loaded_rejects_non_char_or_string() {
    let result = bootstrap_eval_all(
        r#"(condition-case err
               (string-rectangle 1 10 1.5)
             (error (list 'err (car err))))"#,
    );
    assert_eq!(result[0], r#"OK (err wrong-type-argument)"#);
}

#[test]
fn delete_extract_rectangle_startup_is_autoloaded() {
    let eval = eval_with_ldefs_boot_autoloads(&["delete-extract-rectangle"]);
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
