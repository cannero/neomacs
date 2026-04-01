use super::super::eval::Context;
use super::*;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};
use std::fs;
use std::path::PathBuf;

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = super::super::parser::parse_forms(src).expect("parse forms");
    ev.eval_forms(&forms)
        .iter()
        .map(super::super::format_eval_result)
        .collect()
}

fn eval_first_form_after_marker(eval: &mut Context, source: &str, marker: &str) {
    let start = source
        .find(marker)
        .unwrap_or_else(|| panic!("missing GNU simple.el marker: {marker}"));
    let forms = super::super::parser::parse_forms(&source[start..])
        .unwrap_or_else(|err| panic!("parse GNU simple.el from {marker} failed: {:?}", err));
    let form = forms
        .first()
        .unwrap_or_else(|| panic!("no GNU simple.el form found after marker: {marker}"));
    eval.eval_expr(form)
        .unwrap_or_else(|err| panic!("evaluate GNU simple.el form {marker} failed: {:?}", err));
}

/// Install minimal `defun`/`defmacro`/`when`/`unless` shims so a bare
/// evaluator can evaluate forms extracted from GNU `.el` source files.
fn install_bare_elisp_shims(ev: &mut Context) {
    let shims = r#"
(defalias 'defun (cons 'macro #'(lambda (name arglist &rest body)
  (list 'defalias (list 'quote name) (cons 'function (list (cons 'lambda (cons arglist body))))))))
(defalias 'defmacro (cons 'macro #'(lambda (name arglist &rest body)
  (list 'defalias (list 'quote name)
        (list 'cons ''macro (cons 'function (list (cons 'lambda (cons arglist body)))))))))
(defalias 'when (cons 'macro #'(lambda (cond &rest body)
  (list 'if cond (cons 'progn body)))))
(defalias 'unless (cons 'macro #'(lambda (cond &rest body)
  (cons 'if (cons cond (cons nil body))))))
"#;
    let forms = super::super::parser::parse_forms(shims).expect("parse bare elisp shims");
    for form in &forms {
        ev.eval_expr(form).expect("install bare elisp shim");
    }
}

fn gnu_simple_indent_eval() -> Context {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let simple_path = project_root.join("lisp/simple.el");
    let simple_source = fs::read_to_string(&simple_path).expect("read GNU simple.el");

    let mut ev = Context::new();
    install_bare_elisp_shims(&mut ev);
    ev.set_lexical_binding(true);
    eval_first_form_after_marker(&mut ev, &simple_source, "(defun back-to-indentation ()");
    ev
}

fn gnu_indent_el_eval() -> Context {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let indent_path = project_root.join("lisp/indent.el");
    let indent_source = fs::read_to_string(&indent_path).expect("read GNU indent.el");
    let simple_path = project_root.join("lisp/simple.el");
    let simple_source = fs::read_to_string(&simple_path).expect("read GNU simple.el");
    let subr_path = project_root.join("lisp/subr.el");
    let subr_source = fs::read_to_string(&subr_path).expect("read GNU subr.el");
    let syntax_path = project_root.join("lisp/emacs-lisp/syntax.el");
    let syntax_source = fs::read_to_string(&syntax_path).expect("read GNU syntax.el");

    let mut ev = Context::new();
    install_bare_elisp_shims(&mut ev);
    ev.set_lexical_binding(true);
    let progress_stub_forms = super::super::parser::parse_forms(
        r#"
        (setq fill-prefix nil)
        (setq abbrev-mode nil)
        (defvar tab-always-indent t)
        (defvar tab-first-completion nil)
        (fset 'use-region-p (lambda () nil))
        (fset 'make-progress-reporter (lambda (&rest _args) nil))
        (fset 'progress-reporter-update (lambda (&rest _args) nil))
        (fset 'progress-reporter-done (lambda (&rest _args) nil))
        "#,
    )
    .expect("parse progress reporter stubs");
    ev.eval_forms(&progress_stub_forms);
    eval_first_form_after_marker(
        &mut ev,
        &syntax_source,
        "(defvar syntax-propertize-function nil",
    );
    eval_first_form_after_marker(&mut ev, &syntax_source, "(defun syntax-propertize (pos)");
    eval_first_form_after_marker(&mut ev, &indent_source, "(defvar indent-line-function ");
    eval_first_form_after_marker(
        &mut ev,
        &indent_source,
        "(defvar indent-line-ignored-functions ",
    );
    eval_first_form_after_marker(
        &mut ev,
        &indent_source,
        "(defun indent-according-to-mode (&optional inhibit-widen)",
    );
    eval_first_form_after_marker(
        &mut ev,
        &indent_source,
        "(defun indent--default-inside-comment ()",
    );
    eval_first_form_after_marker(
        &mut ev,
        &simple_source,
        "(defun delete-horizontal-space (&optional backward-only)",
    );
    eval_first_form_after_marker(
        &mut ev,
        &simple_source,
        "(defun delete-space--internal (chars backward-only)",
    );
    eval_first_form_after_marker(&mut ev, &subr_source, "(defun cadr (x)");
    eval_first_form_after_marker(&mut ev, &subr_source, "(defun last (list &optional n)");
    eval_first_form_after_marker(&mut ev, &indent_source, "(defun indent-line-to (column)");
    eval_first_form_after_marker(
        &mut ev,
        &indent_source,
        "(defun indent--funcall-widened (func)",
    );
    eval_first_form_after_marker(&mut ev, &indent_source, "(defun insert-tab (&optional arg)");
    eval_first_form_after_marker(
        &mut ev,
        &indent_source,
        "(defun indent-next-tab-stop (column &optional prev)",
    );
    eval_first_form_after_marker(&mut ev, &indent_source, "(defun tab-to-tab-stop ()");
    eval_first_form_after_marker(
        &mut ev,
        &indent_source,
        "(defun indent-region-line-by-line (start end)",
    );
    eval_first_form_after_marker(
        &mut ev,
        &indent_source,
        "(defvar indent-region-function #'indent-region-line-by-line",
    );
    eval_first_form_after_marker(
        &mut ev,
        &indent_source,
        "(defun indent-region (start end &optional column)",
    );
    eval_first_form_after_marker(&mut ev, &indent_source, "(defun indent-relative (&optional");
    eval_first_form_after_marker(
        &mut ev,
        &indent_source,
        "(defun indent-for-tab-command (&optional arg)",
    );
    ev
}

fn eval_all(ev: &mut Context, src: &str) -> Vec<String> {
    let forms = super::super::parser::parse_forms(src).expect("parse forms");
    ev.eval_forms(&forms)
        .iter()
        .map(super::super::format_eval_result)
        .collect()
}

#[test]
fn eval_column_and_indentation_subset() {
    let mut ev = super::super::eval::Context::new();
    let forms = super::super::parser::parse_forms(
        r#"
        (with-temp-buffer
          (insert "abc")
          (goto-char (+ (point-min) 2))
          (current-column))
        (with-temp-buffer
          (insert "  abc")
          (goto-char (point-max))
          (current-indentation))
        (with-temp-buffer
          (insert "a\tb")
          (goto-char (point-min))
          (move-to-column 5)
          (list (point) (current-column)))
        "#,
    )
    .expect("parse forms");

    let col = ev.eval(&forms[0]).expect("eval current-column");
    assert_val_eq!(col, Value::fixnum(2));

    let indent = ev.eval(&forms[1]).expect("eval current-indentation");
    assert_val_eq!(indent, Value::fixnum(2));

    let move_result = ev.eval(&forms[2]).expect("eval move-to-column");
    let items = list_to_vec(&move_result).expect("list result");
    assert_eq!(items, vec![Value::fixnum(3), Value::fixnum(8)]);
}

#[test]
fn eval_move_to_column_wholenump_validation() {
    let mut ev = super::super::eval::Context::new();
    let err = builtin_move_to_column(&mut ev, vec![Value::string("x")]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("wholenump"), Value::string("x")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn eval_move_to_column_force_subset() {
    let mut ev = super::super::eval::Context::new();
    let forms = super::super::parser::parse_forms(
        r#"
        (with-temp-buffer
          (insert "abc")
          (goto-char (point-min))
          (list (move-to-column 10 t) (point) (append (buffer-string) nil)))
        (with-temp-buffer
          (insert "a\tb")
          (goto-char (point-min))
          (list (move-to-column 5 t) (point) (append (buffer-string) nil)))
        "#,
    )
    .expect("parse forms");

    let first = ev.eval(&forms[0]).expect("eval first force case");
    let first_items = list_to_vec(&first).expect("first list");
    assert_val_eq!(first_items[0], Value::fixnum(10));
    assert_val_eq!(first_items[1], Value::fixnum(7));
    assert_eq!(
        list_to_vec(&first_items[2]).expect("first buffer bytes"),
        vec![
            Value::fixnum(97),
            Value::fixnum(98),
            Value::fixnum(99),
            Value::fixnum(9),
            Value::fixnum(32),
            Value::fixnum(32),
        ]
    );

    let second = ev.eval(&forms[1]).expect("eval second force case");
    let second_items = list_to_vec(&second).expect("second list");
    assert_val_eq!(second_items[0], Value::fixnum(5));
    assert_val_eq!(second_items[1], Value::fixnum(6));
    assert_eq!(
        list_to_vec(&second_items[2]).expect("second buffer bytes"),
        vec![
            Value::fixnum(97),
            Value::fixnum(32),
            Value::fixnum(32),
            Value::fixnum(32),
            Value::fixnum(32),
            Value::fixnum(9),
            Value::fixnum(98),
        ]
    );
}

#[test]
fn gnu_back_to_indentation_matches_simple_el() {
    let mut ev = gnu_simple_indent_eval();
    let results = eval_all(
        &mut ev,
        r#"(subrp (symbol-function 'back-to-indentation))
           (with-temp-buffer
             (insert "  abc")
             (goto-char (point-max))
             (back-to-indentation)
             (point))
           (with-temp-buffer
             (insert "   ")
             (goto-char (point-max))
             (back-to-indentation)
             (point))
           (with-temp-buffer
             (insert (string 9 97 98 99))
             (goto-char (point-max))
             (back-to-indentation)
             (point))
           (with-temp-buffer
             (insert (string 10 32 32 97 98 99))
             (goto-char (point-max))
             (back-to-indentation)
             (point))"#,
    );

    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK 3");
    assert_eq!(results[2], "OK 4");
    assert_eq!(results[3], "OK 2");
    assert_eq!(results[4], "OK 4");
}

#[test]
fn gnu_indent_region_matches_indent_el() {
    let mut ev = gnu_indent_el_eval();
    let forms = super::super::parser::parse_forms(
        r#"
        (with-temp-buffer
          (insert (string 97 10 32 32 98 10 10 9 99))
          (indent-region (point-min) (point-max) 2)
          (append (buffer-string) nil))
        (with-temp-buffer
          (insert (string 97 10 32 32 98))
          (indent-region (point-min) (point-max))
          (append (buffer-string) nil))
        (with-temp-buffer
          (insert (string 97 10 98))
          (indent-region (point-max) (point-min) 1)
          (append (buffer-string) nil))
        (with-temp-buffer
          (insert "a")
          (indent-region (point-min) (point-max) "x"))
        "#,
    )
    .expect("parse indent-region forms");

    let first = ev.eval(&forms[0]).expect("eval indent-region column");
    assert_eq!(
        list_to_vec(&first).expect("first byte list"),
        vec![
            Value::fixnum(32),
            Value::fixnum(32),
            Value::fixnum(97),
            Value::fixnum(10),
            Value::fixnum(32),
            Value::fixnum(32),
            Value::fixnum(98),
            Value::fixnum(10),
            Value::fixnum(10),
            Value::fixnum(32),
            Value::fixnum(32),
            Value::fixnum(99),
        ]
    );

    let second = ev.eval(&forms[1]).expect("eval indent-region nil column");
    assert_eq!(
        list_to_vec(&second).expect("second byte list"),
        vec![Value::fixnum(97), Value::fixnum(10), Value::fixnum(98)]
    );

    let third = ev
        .eval(&forms[2])
        .expect("eval indent-region swapped bounds");
    assert_eq!(
        list_to_vec(&third).expect("third byte list"),
        vec![Value::fixnum(97), Value::fixnum(10), Value::fixnum(98)]
    );

    let fourth = ev
        .eval(&forms[3])
        .expect("eval indent-region non-numeric column");
    assert_val_eq!(fourth, Value::T);
}

#[test]
fn gnu_indent_according_to_mode_matches_indent_el() {
    let mut ev = gnu_indent_el_eval();
    let forms = super::super::parser::parse_forms(
        r#"
        (with-temp-buffer
          (insert (string 32 32 97))
          (goto-char (point-max))
          (indent-according-to-mode)
          (append (buffer-string) nil))
        (with-temp-buffer
          (insert (string 32 32 97))
          (goto-char (point-max))
          (indent-according-to-mode)
          (point))
        "#,
    )
    .expect("parse forms");

    let first = match ev.eval(&forms[0]) {
        Ok(value) => value,
        Err(Flow::Signal(sig)) => panic!(
            "eval indent-according-to-mode: {} {:?}",
            sig.symbol_name(),
            sig.data
                .iter()
                .map(|value| value.as_symbol_name().unwrap_or("<non-symbol>"))
                .collect::<Vec<_>>()
        ),
        Err(err) => panic!("eval indent-according-to-mode: {err:?}"),
    };
    assert_eq!(
        list_to_vec(&first).expect("first byte list"),
        vec![Value::fixnum(97)]
    );

    let second = match ev.eval(&forms[1]) {
        Ok(value) => value,
        Err(Flow::Signal(sig)) => panic!(
            "eval indent-according-to-mode point: {} {:?}",
            sig.symbol_name(),
            sig.data
                .iter()
                .map(|value| value.as_symbol_name().unwrap_or("<non-symbol>"))
                .collect::<Vec<_>>()
        ),
        Err(err) => panic!("eval indent-according-to-mode point: {err:?}"),
    };
    assert_val_eq!(second, Value::fixnum(2));
}

#[test]
fn bootstrap_self_insert_command_uses_last_command_event() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (let ((last-command-event 10))
               (list (self-insert-command 1)
                     (point)
                     (append (buffer-string) nil))))"#,
    );
    assert_eq!(results[0], "OK (nil 2 (10))");
}

#[test]
fn bootstrap_newline_inserts_lf_in_simple_el() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "ab")
             (goto-char 2)
             (list (newline)
                   (point)
                   (append (buffer-string) nil)))"#,
    );
    assert_eq!(results[0], "OK (nil 3 (97 10 98))");
}

#[test]
fn bootstrap_newline_marker_round_trip_in_simple_el() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "ab")
             (goto-char 2)
             (let ((pos (point-marker)))
               (newline)
               (goto-char pos)
               (list (point)
                     (marker-position pos)
                     (append (buffer-string) nil))))"#,
    );
    assert_eq!(results[0], "OK (2 2 (97 10 98))");
}

#[test]
fn bootstrap_newline_copy_marker_sequence_matches_simple_el() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "a b")
             (goto-char 3)
             (let ((pos (point-marker)))
               (newline)
               (save-excursion
                 (goto-char pos)
                 (setq pos (copy-marker pos t))
                 (list (point)
                       (marker-position pos)
                       (marker-insertion-type pos)
                       (append (buffer-string) nil)))))"#,
    );
    assert_eq!(results[0], "OK (3 3 t (97 32 10 98))");
}

#[test]
fn bootstrap_reindent_delete_horizontal_space_step_matches_simple_el() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "a b")
             (goto-char 3)
             (let ((pos (point-marker)))
               (newline)
               (save-excursion
                 (goto-char pos)
                 (setq pos (copy-marker pos t))
                 (indent-according-to-mode)
                 (goto-char pos)
                 (delete-horizontal-space t))
               (list (point)
                     (append (buffer-string) nil))))"#,
    );
    assert_eq!(results[0], "OK (3 (97 10 98))");
}

#[test]
fn reindent_then_newline_and_indent_normalizes_split_whitespace() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "a b")
             (goto-char 3)
             (list (reindent-then-newline-and-indent)
                   (point)
                   (append (buffer-string) nil)))"#,
    );
    assert_eq!(results[0], "OK (nil 3 (97 10 98))");
}

#[test]
fn wrong_arg_count_errors() {
    let mut eval = super::super::eval::Context::new();
    // current-indentation takes no args
    assert!(builtin_current_indentation(&mut eval, vec![Value::fixnum(1)]).is_err());
    // indent-to requires at least 1 arg
    assert!(builtin_indent_to(&mut eval, vec![]).is_err());
    // indent-to accepts at most 2 args
    assert!(
        builtin_indent_to(
            &mut eval,
            vec![Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]
        )
        .is_err()
    );
    // current-column takes no args
    assert!(builtin_current_column(&mut eval, vec![Value::fixnum(1)]).is_err());
}

#[test]
fn indent_to_rejects_non_integer() {
    let mut eval = super::super::eval::Context::new();
    assert!(builtin_indent_to(&mut eval, vec![Value::string("foo")]).is_err());
}

#[test]
fn init_indent_vars_sets_defaults() {
    let mut obarray = super::super::symbol::Obarray::new();
    init_indent_vars(&mut obarray);

    assert_eq!(obarray.symbol_value("tab-width").unwrap().as_int(), Some(8));
    assert!(
        obarray
            .symbol_value("indent-tabs-mode")
            .unwrap()
            .is_truthy()
    );
    assert_eq!(
        obarray.symbol_value("standard-indent").unwrap().as_int(),
        Some(4)
    );
    assert!(obarray.symbol_value("tab-stop-list").unwrap().is_nil());

    // All should be special (dynamically bound)
    assert!(obarray.is_special("tab-width"));
    assert!(obarray.is_special("indent-tabs-mode"));
    assert!(obarray.is_special("standard-indent"));
    assert!(obarray.is_special("tab-stop-list"));
}

#[test]
fn indent_for_tab_command_inserts_tab() {
    let mut ev = gnu_indent_el_eval();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "x")
             (goto-char 1)
             (indent-for-tab-command)
             (buffer-string))"#,
    )
    .expect("parse forms");
    let value = ev.eval(&forms[0]).expect("eval");
    assert_eq!(value.as_str(), Some("\tx"));
}

#[test]
fn eval_indent_to_inserts_padding_and_returns_column() {
    let mut ev = super::super::eval::Context::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "abcdef")
             (goto-char (point-max))
             (list (current-column)
                   (indent-to 2)
                   (current-column)))
           (with-temp-buffer
             (list (current-column)
                   (indent-to 2 5)
                   (current-column)))"#,
    )
    .expect("parse forms");

    let first = ev.eval(&forms[0]).expect("first indent-to");
    assert_eq!(super::super::print::print_value(&first), "(6 6 6)");

    let second = ev.eval(&forms[1]).expect("second indent-to");
    assert_eq!(super::super::print::print_value(&second), "(0 5 5)");
}

#[test]
fn eval_indent_to_rejects_non_fixnump_minimum() {
    let mut ev = super::super::eval::Context::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer (condition-case err (indent-to 4 nil) (error err)))
           (with-temp-buffer (condition-case err (indent-to 4 "x") (error err)))
           (with-temp-buffer (condition-case err (indent-to 4 t) (error err)))
           (with-temp-buffer (condition-case err (indent-to "x") (error err)))"#,
    )
    .expect("parse forms");

    let results = ev.eval_forms(&forms);
    let printed: Vec<String> = results
        .iter()
        .map(super::super::format_eval_result)
        .collect();

    assert_eq!(printed[0], "OK 4");
    assert_eq!(printed[1], r#"OK (wrong-type-argument fixnump "x")"#);
    assert_eq!(printed[2], "OK (wrong-type-argument fixnump t)");
    assert_eq!(printed[3], r#"OK (wrong-type-argument fixnump "x")"#);
}

#[test]
fn eval_indent_builtins_respect_dynamic_and_buffer_local_settings() {
    let mut ev = super::super::eval::Context::new();
    let forms = super::super::parser::parse_forms(
        r#"(let ((tab-width 4))
             (with-temp-buffer
               (insert "a\tb")
               (goto-char (point-min))
               (forward-char 2)
               (list (current-column)
                     (current-indentation)
                     (move-to-column 3)
                     (current-column))))
           (let ((tab-width 4) (indent-tabs-mode t))
             (with-temp-buffer
               (list (indent-to 6 1)
                     (current-column)
                     (append (buffer-string) nil))))
           (with-temp-buffer
             (setq tab-width 4)
             (insert "\tb")
             (goto-char (point-max))
             (list (current-indentation) (current-column)))"#,
    )
    .expect("parse forms");

    let results = ev.eval_forms(&forms);
    let printed: Vec<String> = results
        .iter()
        .map(super::super::format_eval_result)
        .collect();

    assert_eq!(printed[0], "OK (4 0 4 4)");
    assert_eq!(printed[1], "OK (6 6 (9 32 32))");
    assert_eq!(printed[2], "OK (4 5)");
}

#[test]
fn indent_for_tab_command_normalizes_leading_whitespace_at_point() {
    let mut ev = gnu_indent_el_eval();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "  x")
             (goto-char 3)
             (list (indent-for-tab-command) (point) (append (buffer-string) nil)))"#,
    )
    .expect("parse forms");
    let value = ev.eval(&forms[0]).expect("eval");
    let printed = super::super::print::print_value(&value);
    assert_eq!(printed, "(nil 2 (9 120))");
}

#[test]
fn save_restriction_restores_full_buffer_after_widen_insert() {
    let mut ev = super::super::eval::Context::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "x")
             (save-restriction
               (widen)
               (goto-char 1)
               (insert "\t"))
             (append (buffer-string) nil))"#,
    )
    .expect("parse forms");
    let value = ev.eval(&forms[0]).expect("eval");
    assert_eq!(super::super::print::print_value(&value), "(9 120)");
}
