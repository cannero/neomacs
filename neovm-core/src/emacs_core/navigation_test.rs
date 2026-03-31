use super::super::eval::Context;
use super::super::value::{Value, ValueKind};
use crate::emacs_core::load::{
    apply_ldefs_boot_autoloads_for_names, apply_runtime_startup_state,
    create_bootstrap_evaluator_cached,
};
use std::fs;
use std::path::PathBuf;

/// Helper: create an evaluator, insert text, and position point.
fn eval_with_text(text: &str) -> Context {
    let mut ev = Context::new();
    {
        let buf = ev.buffers.current_buffer_mut().unwrap();
        buf.insert(text);
        // Point is now at the end. Reset to beginning.
        buf.goto_char(0);
    }
    ev
}

fn bootstrap_eval_with_text(text: &str) -> Context {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    {
        let buf = ev.buffers.current_buffer_mut().unwrap();
        buf.insert(text);
        buf.goto_char(0);
    }
    ev
}

fn eval_with_ldefs_boot_autoloads(names: &[&str]) -> Context {
    let mut ev = Context::new();
    for name in names {
        ev.obarray_mut().fmakunbound(name);
    }
    apply_ldefs_boot_autoloads_for_names(&mut ev, names).expect("ldefs-boot autoload restore");
    ev
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

fn gnu_simple_line_eval() -> Context {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let simple_path = project_root.join("lisp/simple.el");
    let subr_path = project_root.join("lisp/subr.el");
    let simple_source = fs::read_to_string(&simple_path)
        .expect("read GNU simple.el")
        .replace(
            "(with-suppressed-warnings ((obsolete inhibit-point-motion-hooks))",
            "(progn",
        )
        .replace("(called-interactively-p 'interactive)", "nil");
    let subr_source = fs::read_to_string(&subr_path)
        .expect("read GNU subr.el")
        .replace(
            "(defsubst buffer-narrowed-p ()",
            "(defun buffer-narrowed-p ()",
        );

    let mut ev = Context::new();
    install_bare_elisp_shims(&mut ev);
    ev.set_lexical_binding(true);
    eval_first_form_after_marker(&mut ev, &subr_source, "(defun zerop (number)");
    eval_first_form_after_marker(&mut ev, &subr_source, "(defun buffer-narrowed-p ()");
    for marker in [
        "(defun beginning-of-buffer (&optional arg)",
        "(defun end-of-buffer (&optional arg)",
        "(defun goto-line (line &optional buffer relative interactive)",
        "(defun next-line (&optional arg try-vscroll)",
        "(defun previous-line (&optional arg try-vscroll)",
        "(defun line-move (arg &optional noerror _to-end try-vscroll)",
        "(defun line-move-1 (arg &optional noerror _to-end)",
        "(defun line-move-finish (column opoint forward &optional not-ipmh)",
        "(defun line-move-to-column (col)",
    ] {
        eval_first_form_after_marker(&mut ev, &simple_source, marker);
    }
    eval_str(
        &mut ev,
        "(setq next-line-add-newlines nil
               track-eol nil
               goal-column nil
               temporary-goal-column 0
               selective-display nil
               widen-automatically nil
               line-move-ignore-invisible t
               line-move-visual t)",
    );
    ev
}

/// Evaluate an Elisp string and return the result Value.
fn eval_str(ev: &mut Context, src: &str) -> Value {
    let forms = super::super::parser::parse_forms(src).unwrap();
    let results = ev.eval_forms(&forms);
    results.into_iter().last().unwrap().unwrap()
}

/// Evaluate and expect an integer result.
fn eval_int(ev: &mut Context, src: &str) -> i64 {
    match eval_str(ev, src).kind() {
        ValueKind::Fixnum(n) => n,
        other => panic!("expected Int, got {:?}", other),
    }
}

// -----------------------------------------------------------------------
// Position predicates
// -----------------------------------------------------------------------

#[test]
fn test_bobp_at_beginning() {
    let mut ev = eval_with_text("hello");
    let val = eval_str(&mut ev, "(bobp)");
    assert!(val.is_truthy());
}

#[test]
fn test_bobp_not_at_beginning() {
    let mut ev = eval_with_text("hello");
    eval_str(&mut ev, "(forward-char 2)");
    let val = eval_str(&mut ev, "(bobp)");
    assert!(val.is_nil());
}

#[test]
fn test_eobp_at_end() {
    let mut ev = eval_with_text("hello");
    eval_str(&mut ev, "(goto-char 6)"); // past last char (1-based)
    let val = eval_str(&mut ev, "(eobp)");
    assert!(val.is_truthy());
}

#[test]
fn test_eobp_not_at_end() {
    let mut ev = eval_with_text("hello");
    let val = eval_str(&mut ev, "(eobp)");
    assert!(val.is_nil());
}

#[test]
fn test_bolp_at_beginning_of_buffer() {
    let mut ev = eval_with_text("hello");
    let val = eval_str(&mut ev, "(bolp)");
    assert!(val.is_truthy());
}

#[test]
fn test_bolp_after_newline() {
    let mut ev = eval_with_text("abc\ndef");
    eval_str(&mut ev, "(goto-char 5)"); // right after newline
    let val = eval_str(&mut ev, "(bolp)");
    assert!(val.is_truthy());
}

#[test]
fn test_bolp_not_at_bol() {
    let mut ev = eval_with_text("hello");
    eval_str(&mut ev, "(forward-char 2)");
    let val = eval_str(&mut ev, "(bolp)");
    assert!(val.is_nil());
}

#[test]
fn test_eolp_at_newline() {
    let mut ev = eval_with_text("abc\ndef");
    eval_str(&mut ev, "(goto-char 4)"); // at newline
    let val = eval_str(&mut ev, "(eolp)");
    assert!(val.is_truthy());
}

#[test]
fn test_eolp_at_eob() {
    let mut ev = eval_with_text("hello");
    eval_str(&mut ev, "(goto-char 6)");
    let val = eval_str(&mut ev, "(eolp)");
    assert!(val.is_truthy());
}

#[test]
fn test_eolp_not_at_eol() {
    let mut ev = eval_with_text("hello");
    eval_str(&mut ev, "(goto-char 2)");
    let val = eval_str(&mut ev, "(eolp)");
    assert!(val.is_nil());
}

// -----------------------------------------------------------------------
// Line operations
// -----------------------------------------------------------------------

#[test]
fn test_line_beginning_position() {
    let mut ev = eval_with_text("abc\ndef\nghi");
    eval_str(&mut ev, "(goto-char 6)"); // middle of "def"
    let pos = eval_int(&mut ev, "(line-beginning-position)");
    assert_eq!(pos, 5); // start of "def" line
}

#[test]
fn test_line_end_position() {
    let mut ev = eval_with_text("abc\ndef\nghi");
    eval_str(&mut ev, "(goto-char 6)"); // middle of "def"
    let pos = eval_int(&mut ev, "(line-end-position)");
    assert_eq!(pos, 8); // end of "def" (position of newline)
}

#[test]
fn test_line_beginning_position_with_offset() {
    let mut ev = eval_with_text("aaa\nbbb\nccc");
    eval_str(&mut ev, "(goto-char 1)"); // beginning of first line
    let pos = eval_int(&mut ev, "(line-beginning-position 2)");
    assert_eq!(pos, 5); // beginning of second line
}

#[test]
fn test_line_end_position_with_offset() {
    let mut ev = eval_with_text("aaa\nbbb\nccc");
    eval_str(&mut ev, "(goto-char 1)");
    let pos = eval_int(&mut ev, "(line-end-position 2)");
    assert_eq!(pos, 8); // end of second line (position of newline)
}

#[test]
fn test_line_positions_with_zero_offset() {
    let mut ev = eval_with_text("hello world\nfoo bar\nbaz qux\n");
    eval_str(&mut ev, "(goto-char 14)");
    assert_eq!(eval_int(&mut ev, "(line-beginning-position 0)"), 1);
    assert_eq!(eval_int(&mut ev, "(line-end-position 0)"), 12);
}

#[test]
fn test_line_end_position_zero_offset_clips_to_point_min() {
    let mut ev = eval_with_text("hello world\nfoo bar\n");
    eval_str(&mut ev, "(goto-char 5)");
    assert_eq!(eval_int(&mut ev, "(line-end-position 0)"), 1);
}

#[test]
fn test_line_number_at_pos() {
    let mut ev = eval_with_text("abc\ndef\nghi");
    let n = eval_int(&mut ev, "(line-number-at-pos 6)");
    assert_eq!(n, 2); // "def" is line 2
}

#[test]
fn test_line_number_at_pos_default() {
    let mut ev = eval_with_text("abc\ndef\nghi");
    // Point is at 1 (first char)
    let n = eval_int(&mut ev, "(line-number-at-pos)");
    assert_eq!(n, 1);
}

#[test]
fn test_forward_line() {
    let mut ev = eval_with_text("abc\ndef\nghi");
    let remainder = eval_int(&mut ev, "(forward-line 1)");
    assert_eq!(remainder, 0);
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 5); // beginning of "def" line
}

#[test]
fn test_forward_line_past_end() {
    let mut ev = eval_with_text("abc\ndef");
    let remainder = eval_int(&mut ev, "(forward-line 5)");
    assert!(remainder > 0);
}

#[test]
fn test_forward_line_negative_from_middle_of_line() {
    let mut ev = eval_with_text("aaa\nbbb\nccc");
    eval_str(&mut ev, "(goto-char 6)");
    let remainder = eval_int(&mut ev, "(forward-line -1)");
    assert_eq!(remainder, 0);
    assert_eq!(eval_int(&mut ev, "(point)"), 1);
}

#[test]
fn bootstrap_next_and_previous_line_match_simple_el() {
    let mut ev = gnu_simple_line_eval();
    let ownership = eval_str(
        &mut ev,
        "(list (subrp (symbol-function 'next-line))
               (subrp (symbol-function 'previous-line)))",
    );
    assert_eq!(ownership, Value::list(vec![Value::NIL, Value::NIL]));

    let next_line_pos = eval_int(
        &mut ev,
        "(progn
           (erase-buffer)
           (insert \"abc\ndef\")
           (goto-char 1)
           (next-line)
           (point))",
    );
    assert_eq!(next_line_pos, 5);

    let next_line_err = eval_str(
        &mut ev,
        "(progn
           (erase-buffer)
           (insert \"abc\")
           (goto-char 1)
           (condition-case err (next-line) (error (car err))))",
    );
    assert_eq!(next_line_err.as_symbol_name(), Some("end-of-buffer"));

    let previous_line_pos = eval_int(
        &mut ev,
        "(progn
           (erase-buffer)
           (insert \"abc\ndef\")
           (goto-char 5)
           (previous-line)
           (point))",
    );
    assert_eq!(previous_line_pos, 1);

    let previous_line_err = eval_str(
        &mut ev,
        "(progn
           (erase-buffer)
           (insert \"abc\")
           (goto-char 1)
           (condition-case err (previous-line) (error (car err))))",
    );
    assert_eq!(
        previous_line_err.as_symbol_name(),
        Some("beginning-of-buffer")
    );

    let previous_line_mid_err = eval_str(
        &mut ev,
        "(progn
           (erase-buffer)
           (insert \"abc\")
           (goto-char 2)
           (condition-case err (previous-line) (error (car err))))",
    );
    assert_eq!(
        previous_line_mid_err.as_symbol_name(),
        Some("beginning-of-buffer")
    );
}

#[test]
fn test_beginning_of_line() {
    let mut ev = eval_with_text("abc\ndef");
    eval_str(&mut ev, "(goto-char 6)");
    eval_str(&mut ev, "(beginning-of-line)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 5);
}

#[test]
fn test_end_of_line() {
    let mut ev = eval_with_text("abc\ndef");
    eval_str(&mut ev, "(goto-char 1)");
    eval_str(&mut ev, "(end-of-line)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 4); // position of '\n'
}

#[test]
fn bootstrap_beginning_and_end_of_buffer_match_simple_el() {
    let mut ev = gnu_simple_line_eval();
    let buf = ev.buffers.current_buffer_id().expect("current buffer");
    ev.frames.create_frame("F1", 800, 600, buf);

    let ownership = eval_str(
        &mut ev,
        "(list (subrp (symbol-function 'beginning-of-buffer))
               (subrp (symbol-function 'end-of-buffer)))",
    );
    assert_eq!(ownership, Value::list(vec![Value::NIL, Value::NIL]));
    eval_str(&mut ev, "(fset 'push-mark (lambda (&rest _args) nil))");
    eval_str(&mut ev, "(fset 'region-active-p (lambda () nil))");

    let beginning_default = eval_int(
        &mut ev,
        "(progn
           (erase-buffer)
           (insert \"abc\ndef\")
           (goto-char 5)
           (beginning-of-buffer)
           (point))",
    );
    assert_eq!(beginning_default, 1);

    let beginning_numeric = eval_int(
        &mut ev,
        "(progn
           (erase-buffer)
           (insert \"abc\ndef\")
           (goto-char 2)
           (beginning-of-buffer 1)
           (point))",
    );
    assert_eq!(beginning_numeric, 5);

    let end_default = eval_int(
        &mut ev,
        "(progn
           (erase-buffer)
           (insert \"abc\ndef\")
           (goto-char 2)
           (end-of-buffer)
           (point))",
    );
    assert_eq!(end_default, 8);

    let beginning_err = eval_str(
        &mut ev,
        "(condition-case err (beginning-of-buffer nil nil) (error (car err)))",
    );
    assert_eq!(
        beginning_err.as_symbol_name(),
        Some("wrong-number-of-arguments")
    );

    let end_err = eval_str(
        &mut ev,
        "(condition-case err (end-of-buffer nil nil) (error (car err)))",
    );
    assert_eq!(end_err.as_symbol_name(), Some("wrong-number-of-arguments"));
}

#[test]
fn bootstrap_goto_line_matches_simple_el() {
    let mut ev = gnu_simple_line_eval();

    let ownership = eval_str(&mut ev, "(subrp (symbol-function 'goto-line))");
    assert_eq!(ownership, Value::NIL);

    let default_pos = eval_int(
        &mut ev,
        "(progn
           (erase-buffer)
           (insert \"aaa\nbbb\nccc\")
           (goto-line 3)
           (point))",
    );
    assert_eq!(default_pos, 9);

    let relative_pos = eval_int(
        &mut ev,
        "(progn
           (erase-buffer)
           (insert \"a\nb\nc\nd\")
           (narrow-to-region 3 7)
           (goto-line 2 nil t nil)
           (point))",
    );
    assert_eq!(relative_pos, 5);

    let arity_err = eval_str(
        &mut ev,
        "(condition-case err (goto-line 1 nil nil nil nil) (error (car err)))",
    );
    assert_eq!(
        arity_err.as_symbol_name(),
        Some("wrong-number-of-arguments")
    );
}

// -----------------------------------------------------------------------
// Character movement
// -----------------------------------------------------------------------

#[test]
fn test_forward_char() {
    let mut ev = eval_with_text("abcdef");
    eval_str(&mut ev, "(forward-char 3)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 4); // 1-based
}

#[test]
fn test_backward_char() {
    let mut ev = eval_with_text("abcdef");
    eval_str(&mut ev, "(goto-char 5)");
    eval_str(&mut ev, "(backward-char 2)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 3);
}

#[test]
fn test_forward_char_default() {
    let mut ev = eval_with_text("abcdef");
    eval_str(&mut ev, "(forward-char)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 2);
}

#[test]
fn test_skip_chars_forward() {
    let mut ev = eval_with_text("aaabbbccc");
    let moved = eval_int(&mut ev, "(skip-chars-forward \"a\")");
    assert_eq!(moved, 3);
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 4);
}

#[test]
fn test_skip_chars_forward_range() {
    let mut ev = eval_with_text("abcdef123");
    let moved = eval_int(&mut ev, "(skip-chars-forward \"a-f\")");
    assert_eq!(moved, 6);
}

#[test]
fn test_skip_chars_backward() {
    let mut ev = eval_with_text("aaabbbccc");
    eval_str(&mut ev, "(goto-char 10)"); // end
    let moved = eval_int(&mut ev, "(skip-chars-backward \"c\")");
    assert_eq!(moved, -3);
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 7);
}

#[test]
fn test_skip_chars_forward_negate() {
    let mut ev = eval_with_text("aaabbbccc");
    let moved = eval_int(&mut ev, "(skip-chars-forward \"^b\")");
    assert_eq!(moved, 3);
}

// -----------------------------------------------------------------------
// Mark and region
// -----------------------------------------------------------------------

#[test]
fn test_push_mark_and_mark() {
    let mut ev = bootstrap_eval_with_text("hello world");
    eval_str(&mut ev, "(push-mark 3)");
    let m = eval_int(&mut ev, "(mark t)");
    assert_eq!(m, 3);
}

#[test]
fn test_push_mark_default_pos() {
    let mut ev = bootstrap_eval_with_text("hello");
    eval_str(&mut ev, "(goto-char 3)");
    eval_str(&mut ev, "(push-mark)");
    let m = eval_int(&mut ev, "(mark t)");
    assert_eq!(m, 3);
}

#[test]
fn test_pop_mark() {
    let mut ev = bootstrap_eval_with_text("hello world");
    eval_str(&mut ev, "(push-mark 3)");
    eval_str(&mut ev, "(push-mark 5)");
    // Mark is now at 5, ring has [3]
    let m = eval_int(&mut ev, "(mark t)");
    assert_eq!(m, 5);
    eval_str(&mut ev, "(pop-mark)");
    let m2 = eval_int(&mut ev, "(mark t)");
    assert_eq!(m2, 3);
}

#[test]
fn test_region_beginning_and_end() {
    let mut ev = bootstrap_eval_with_text("hello world");
    eval_str(&mut ev, "(goto-char 8)");
    eval_str(&mut ev, "(push-mark 3 nil t)");
    let beg = eval_int(&mut ev, "(region-beginning)");
    let end = eval_int(&mut ev, "(region-end)");
    assert_eq!(beg, 3);
    assert_eq!(end, 8);
}

#[test]
fn test_use_region_p_is_available_after_bootstrap() {
    // use-region-p is a defun in simple.el, not autoloaded in GNU Emacs.
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let function = ev
        .obarray
        .symbol_function("use-region-p")
        .expect("missing use-region-p startup function cell");
    assert!(
        !crate::emacs_core::autoload::is_autoload_value(&function),
        "expected use-region-p to be a resolved function, not an autoload"
    );
}

#[test]
fn test_use_region_p() {
    let mut ev = bootstrap_eval_with_text("hello");
    let active = eval_str(
        &mut ev,
        "(let ((transient-mark-mode t))
           (push-mark 3 nil t)
           (use-region-p))",
    );
    assert!(active.is_truthy());
}

#[test]
fn test_use_region_p_inactive() {
    let mut ev = bootstrap_eval_with_text("hello");
    eval_str(&mut ev, "(push-mark 3)"); // not activated
    let active = eval_str(&mut ev, "(use-region-p)");
    assert!(active.is_nil());
}

#[test]
fn test_region_active_p_true_for_active_empty_region() {
    let mut ev = bootstrap_eval_with_text("hello");
    let active = eval_str(
        &mut ev,
        "(let ((transient-mark-mode t))
           (push-mark (point) nil t)
           (region-active-p))",
    );
    assert!(active.is_truthy());
}

#[test]
fn test_region_active_p_requires_mark() {
    let mut ev = bootstrap_eval_with_text("hello");
    let active = eval_str(
        &mut ev,
        "(condition-case err
             (let ((transient-mark-mode t)
                   (mark-active t))
               (region-active-p))
           (error (list (car err) (cdr err))))",
    );
    assert_eq!(
        active,
        eval_str(&mut ev, "'(cl-assertion-failed ((mark)))",)
    );
}

#[test]
fn test_region_active_p_over_arity() {
    let mut ev = bootstrap_eval_with_text("hello");
    let result = eval_str(
        &mut ev,
        "(condition-case err (region-active-p nil) (error (car err)))",
    );
    assert_eq!(result, Value::symbol("wrong-number-of-arguments"));
}

#[test]
fn test_deactivate_mark() {
    let mut ev = bootstrap_eval_with_text("hello");
    eval_str(&mut ev, "(push-mark 3 nil t)");
    eval_str(&mut ev, "(deactivate-mark)");
    let active = eval_str(&mut ev, "(use-region-p)");
    assert!(active.is_nil());
}

#[test]
fn test_exchange_point_and_mark() {
    let mut ev = bootstrap_eval_with_text("hello world");
    eval_str(&mut ev, "(goto-char 3)");
    eval_str(&mut ev, "(push-mark 8 nil t)");
    eval_str(&mut ev, "(exchange-point-and-mark)");
    let pt = eval_int(&mut ev, "(point)");
    let mk = eval_int(&mut ev, "(mark t)");
    assert_eq!(pt, 8);
    assert_eq!(mk, 3);
}

#[test]
fn test_transient_mark_mode() {
    let mut ev = eval_with_text("hello");
    let enabled = eval_str(&mut ev, "(transient-mark-mode)");
    assert!(enabled.is_truthy());

    let disabled = eval_str(&mut ev, "(transient-mark-mode -1)");
    assert!(disabled.is_nil());

    let reenabled_nil = eval_str(&mut ev, "(transient-mark-mode nil)");
    assert!(reenabled_nil.is_truthy());

    let zero = eval_str(&mut ev, "(transient-mark-mode 0)");
    assert!(zero.is_nil());

    let positive_float = eval_str(&mut ev, "(transient-mark-mode 1.5)");
    assert!(positive_float.is_truthy());

    let small_float = eval_str(&mut ev, "(transient-mark-mode 0.5)");
    assert!(small_float.is_nil());
}

#[test]
fn test_transient_mark_mode_over_arity() {
    let mut ev = eval_with_text("hello");
    let result = eval_str(
        &mut ev,
        "(condition-case err (transient-mark-mode nil nil) (error (car err)))",
    );
    assert_eq!(result, Value::symbol("wrong-number-of-arguments"));
}

#[test]
fn test_mark_marker() {
    let mut ev = bootstrap_eval_with_text("hello");
    eval_str(&mut ev, "(push-mark 4)");
    let pos = eval_int(&mut ev, "(marker-position (mark-marker))");
    assert_eq!(pos, 4);
}

#[test]
fn test_set_mark_activates() {
    let mut ev = bootstrap_eval_with_text("hello");
    let active = eval_str(
        &mut ev,
        "(let ((transient-mark-mode t))
           (set-mark 3)
           (use-region-p))",
    );
    assert!(active.is_truthy());
}

#[test]
fn test_use_region_p_honors_buffer_local_mark_active_when_global_is_nil() {
    let mut ev = bootstrap_eval_with_text("hello");
    let active = eval_str(
        &mut ev,
        "(with-temp-buffer
           (let ((transient-mark-mode t))
             (insert \"abc\")
             (goto-char (point-max))
             (set-mark (point-min))
             (setq mark-active t)
             (use-region-p)))",
    );
    assert!(active.is_truthy());
}

// -----------------------------------------------------------------------
// Edge cases
// -----------------------------------------------------------------------

#[test]
fn test_empty_buffer_predicates() {
    let mut ev = Context::new();
    let val = eval_str(&mut ev, "(bobp)");
    assert!(val.is_truthy());
    let val = eval_str(&mut ev, "(eobp)");
    assert!(val.is_truthy());
    let val = eval_str(&mut ev, "(bolp)");
    assert!(val.is_truthy());
    let val = eval_str(&mut ev, "(eolp)");
    assert!(val.is_truthy());
}

#[test]
fn test_forward_line_negative() {
    let mut ev = eval_with_text("abc\ndef\nghi");
    eval_str(&mut ev, "(goto-char 9)"); // on "ghi" line
    eval_str(&mut ev, "(forward-line -1)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 5); // beginning of "def"
}

#[test]
fn test_line_number_at_pos_last_line() {
    let mut ev = eval_with_text("abc\ndef\nghi");
    let n = eval_int(&mut ev, "(line-number-at-pos 10)");
    assert_eq!(n, 3);
}

#[test]
fn test_skip_chars_forward_with_limit() {
    let mut ev = eval_with_text("aaaaaaa");
    let moved = eval_int(&mut ev, "(skip-chars-forward \"a\" 4)");
    assert_eq!(moved, 3); // limited to position 4 (1-based = 3 chars from pos 1)
}

#[test]
fn test_forward_char_negative() {
    let mut ev = eval_with_text("abcdef");
    eval_str(&mut ev, "(goto-char 4)");
    eval_str(&mut ev, "(forward-char -2)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 2);
}
