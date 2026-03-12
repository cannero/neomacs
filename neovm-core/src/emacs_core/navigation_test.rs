use super::super::eval::Evaluator;
use super::super::value::Value;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};

/// Helper: create an evaluator, insert text, and position point.
fn eval_with_text(text: &str) -> Evaluator {
    let mut ev = Evaluator::new();
    {
        let buf = ev.buffers.current_buffer_mut().unwrap();
        buf.insert(text);
        // Point is now at the end. Reset to beginning.
        buf.goto_char(0);
    }
    ev
}

fn bootstrap_eval_with_text(text: &str) -> Evaluator {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    {
        let buf = ev.buffers.current_buffer_mut().unwrap();
        buf.insert(text);
        buf.goto_char(0);
    }
    ev
}

/// Evaluate an Elisp string and return the result Value.
fn eval_str(ev: &mut Evaluator, src: &str) -> Value {
    let forms = super::super::parser::parse_forms(src).unwrap();
    let results = ev.eval_forms(&forms);
    results.into_iter().last().unwrap().unwrap()
}

/// Evaluate and expect an integer result.
fn eval_int(ev: &mut Evaluator, src: &str) -> i64 {
    match eval_str(ev, src) {
        Value::Int(n) => n,
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
fn test_next_line_moves_to_next_line() {
    let mut ev = eval_with_text("abc\ndef");
    eval_str(&mut ev, "(next-line)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 5);
}

#[test]
fn test_next_line_signals_end_of_buffer() {
    let mut ev = eval_with_text("abc");
    let val = eval_str(
        &mut ev,
        "(condition-case err (next-line) (error (car err)))",
    );
    assert_eq!(val.as_symbol_name(), Some("end-of-buffer"));
}

#[test]
fn test_previous_line_moves_to_previous_line() {
    let mut ev = eval_with_text("abc\ndef");
    eval_str(&mut ev, "(goto-char 5)");
    eval_str(&mut ev, "(previous-line)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 1);
}

#[test]
fn test_previous_line_signals_beginning_of_buffer() {
    let mut ev = eval_with_text("abc");
    let val = eval_str(
        &mut ev,
        "(condition-case err (previous-line) (error (car err)))",
    );
    assert_eq!(val.as_symbol_name(), Some("beginning-of-buffer"));
}

#[test]
fn test_previous_line_signals_beginning_of_buffer_from_middle_of_line() {
    let mut ev = eval_with_text("abc");
    eval_str(&mut ev, "(goto-char 2)");
    let val = eval_str(
        &mut ev,
        "(condition-case err (previous-line) (error (car err)))",
    );
    assert_eq!(val.as_symbol_name(), Some("beginning-of-buffer"));
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
fn test_beginning_of_buffer_moves_to_start_by_default() {
    let mut ev = eval_with_text("abc\ndef");
    eval_str(&mut ev, "(goto-char 5)");
    eval_str(&mut ev, "(beginning-of-buffer)");
    assert_eq!(eval_int(&mut ev, "(point)"), 1);
}

#[test]
fn test_beginning_of_buffer_non_nil_arg_moves_to_end() {
    let mut ev = eval_with_text("abc\ndef");
    eval_str(&mut ev, "(goto-char 2)");
    eval_str(&mut ev, "(beginning-of-buffer 1)");
    assert_eq!(eval_int(&mut ev, "(point)"), 8);
}

#[test]
fn test_end_of_buffer_moves_to_end() {
    let mut ev = eval_with_text("abc\ndef");
    eval_str(&mut ev, "(goto-char 2)");
    eval_str(&mut ev, "(end-of-buffer)");
    assert_eq!(eval_int(&mut ev, "(point)"), 8);
}

#[test]
fn test_beginning_end_of_buffer_reject_too_many_args() {
    let mut ev = eval_with_text("abc");
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
fn test_goto_line() {
    let mut ev = eval_with_text("aaa\nbbb\nccc");
    eval_str(&mut ev, "(goto-line 3)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 9); // beginning of third line
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
fn test_use_region_p_startup_is_autoloaded() {
    let eval = super::super::eval::Evaluator::new();
    let function = eval
        .obarray
        .symbol_function("use-region-p")
        .expect("missing use-region-p startup function cell");
    assert!(crate::emacs_core::autoload::is_autoload_value(&function));
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
    let mut ev = eval_with_text("hello");
    let active = eval_str(
        &mut ev,
        "(let ((transient-mark-mode t)
               (mark-active t))
           (region-active-p))",
    );
    assert!(active.is_nil());
}

#[test]
fn test_region_active_p_over_arity() {
    let mut ev = eval_with_text("hello");
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
    let mut ev = Evaluator::new();
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
fn test_goto_line_first() {
    let mut ev = eval_with_text("abc\ndef\nghi");
    eval_str(&mut ev, "(goto-char 7)"); // somewhere in the middle
    eval_str(&mut ev, "(goto-line 1)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 1);
}

#[test]
fn test_forward_char_negative() {
    let mut ev = eval_with_text("abcdef");
    eval_str(&mut ev, "(goto-char 4)");
    eval_str(&mut ev, "(forward-char -2)");
    let pos = eval_int(&mut ev, "(point)");
    assert_eq!(pos, 2);
}
