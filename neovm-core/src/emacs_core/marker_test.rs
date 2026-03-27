use super::*;

fn call_marker_position(args: Vec<Value>) -> EvalResult {
    let mut eval = super::super::eval::Context::new();
    builtin_marker_position(&mut eval, args)
}

fn call_marker_buffer(args: Vec<Value>) -> EvalResult {
    let mut eval = super::super::eval::Context::new();
    builtin_marker_buffer(&mut eval, args)
}

fn call_set_marker_insertion_type(args: Vec<Value>) -> EvalResult {
    let mut eval = super::super::eval::Context::new();
    builtin_set_marker_insertion_type(&mut eval, args)
}

fn call_copy_marker(args: Vec<Value>) -> EvalResult {
    let mut eval = super::super::eval::Context::new();
    builtin_copy_marker(&mut eval, args)
}

#[test]
fn make_marker_creates_tagged_vector() {
    let m = make_marker_value(Some("*scratch*"), Some(42), false);
    assert!(is_marker(&m));
}

#[test]
fn make_marker_empty() {
    let m = make_marker_value(None, None, false);
    assert!(is_marker(&m));
    assert!(marker_position_value(&m).is_nil());
    assert!(marker_buffer_value(&m).is_nil());
}

#[test]
fn is_marker_rejects_non_markers() {
    assert!(!is_marker(&Value::Nil));
    assert!(!is_marker(&Value::Int(42)));
    assert!(!is_marker(&Value::vector(vec![Value::Int(1)])));
    // Wrong tag
    assert!(!is_marker(&Value::vector(vec![
        Value::Keyword(intern(":not-marker")),
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ])));
}

#[test]
fn builtin_markerp_works() {
    let m = make_marker_value(None, None, false);
    assert!(builtin_markerp(vec![m]).unwrap().is_truthy());
    assert!(builtin_markerp(vec![Value::Int(5)]).unwrap().is_nil());
}

#[test]
fn builtin_marker_position_returns_position() {
    let m = make_marker_value(Some("buf"), Some(10), false);
    let pos = call_marker_position(vec![m]).unwrap();
    assert!(matches!(pos, Value::Int(10)));
}

#[test]
fn builtin_marker_position_returns_nil_when_unset() {
    let m = make_marker_value(None, None, false);
    let pos = call_marker_position(vec![m]).unwrap();
    assert!(pos.is_nil());
}

#[test]
fn builtin_marker_buffer_returns_name() {
    let m = make_marker_value(Some("*scratch*"), Some(1), false);
    let buf = call_marker_buffer(vec![m]).unwrap();
    assert_eq!(buf.as_str(), Some("*scratch*"));
}

#[test]
fn builtin_marker_insertion_type_roundtrip() {
    let m = make_marker_value(None, None, false);
    assert!(builtin_marker_insertion_type(vec![m]).unwrap().is_nil());

    call_set_marker_insertion_type(vec![m, Value::True]).unwrap();
    assert!(builtin_marker_insertion_type(vec![m]).unwrap().is_truthy());
}

#[test]
fn builtin_copy_marker_from_marker() {
    let m = make_marker_value(Some("buf"), Some(5), true);
    let copy = call_copy_marker(vec![m]).unwrap();
    assert!(is_marker(&copy));
    assert!(matches!(marker_position_value(&copy), Value::Int(5)));
}

#[test]
fn builtin_copy_marker_from_integer() {
    let copy = call_copy_marker(vec![Value::Int(99)]).unwrap();
    assert!(is_marker(&copy));
    assert!(matches!(marker_position_value(&copy), Value::Int(99)));
    assert!(marker_buffer_value(&copy).is_nil());
}

#[test]
fn builtin_move_marker_matches_set_marker_behavior() {
    let mut eval = super::super::eval::Context::new();
    // Insert content so the buffer is long enough for position 3.
    if let Some(buf) = eval.buffers.current_buffer_mut() {
        buf.insert("abcdef");
    }
    let marker = builtin_make_marker(vec![]).expect("make marker");
    let moved = builtin_move_marker(
        &mut eval,
        vec![marker, Value::Int(3), Value::string("*scratch*")],
    )
    .expect("move marker");
    assert!(is_marker(&moved));
    assert_eq!(call_marker_position(vec![moved]).unwrap(), Value::Int(3));
    assert_eq!(
        call_marker_buffer(vec![moved]).unwrap().as_str(),
        Some("*scratch*")
    );
}

#[test]
fn builtin_make_marker_returns_empty() {
    let m = builtin_make_marker(vec![]).unwrap();
    assert!(is_marker(&m));
    assert!(marker_position_value(&m).is_nil());
    assert!(marker_buffer_value(&m).is_nil());
    assert!(marker_insertion_type_value(&m).is_nil());
}

#[test]
fn wrong_type_signals_error() {
    let result = call_marker_position(vec![Value::Int(5)]);
    assert!(result.is_err());
}

#[test]
fn marker_accessors_require_zero_arguments() {
    let mut eval = super::super::eval::Context::new();

    assert!(builtin_point_marker(&mut eval, vec![Value::Nil]).is_err());
    assert!(builtin_point_min_marker(&mut eval, vec![Value::Nil]).is_err());
    assert!(builtin_point_max_marker(&mut eval, vec![Value::Nil]).is_err());
    assert!(builtin_mark_marker(&mut eval, vec![Value::Nil]).is_err());
}

#[test]
fn numeric_comparisons_use_live_marker_positions() {
    let mut eval = super::super::eval::Context::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "abcdef\n123456\n")
             (goto-char 9)
             (let ((m (copy-marker (line-end-position))))
               (delete-region 1 2)
               (delete-region 7 8)
               (list (marker-position m)
                     (<= (point-max) m)
                     (<= (1- (point-max)) m))))"#,
    )
    .expect("parse marker comparison regression");
    let result = eval
        .eval_forms(&forms)
        .into_iter()
        .last()
        .expect("one form")
        .expect("evaluation succeeds");
    assert_eq!(
        crate::emacs_core::error::format_eval_result(&Ok(result)),
        "OK (12 nil t)"
    );
}

#[test]
fn point_min_and_max_markers_follow_narrowing() {
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer_id().expect("current buffer");
    let _ = eval.buffers.insert_into_buffer(buf_id, "ééz");
    let _ = eval
        .buffers
        .narrow_buffer_to_region(buf_id, 'é'.len_utf8(), "ééz".len());

    let min_marker = builtin_point_min_marker(&mut eval, vec![]).expect("point-min-marker");
    let max_marker = builtin_point_max_marker(&mut eval, vec![]).expect("point-max-marker");

    assert!(matches!(
        call_marker_position(vec![min_marker]),
        Ok(Value::Int(2))
    ));
    assert!(matches!(
        call_marker_position(vec![max_marker]),
        Ok(Value::Int(4))
    ));
}

#[test]
fn mark_marker_follows_cached_mark_char_position() {
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer_id().expect("current buffer");
    let _ = eval.buffers.insert_into_buffer(buf_id, "ééz");
    let _ = eval.buffers.set_buffer_mark(buf_id, 'é'.len_utf8());

    let marker = builtin_mark_marker(&mut eval, vec![]).expect("mark-marker");
    assert!(matches!(
        call_marker_position(vec![marker]),
        Ok(Value::Int(2))
    ));
}

#[test]
fn copy_marker_from_integer_tracks_insertions_before_it() {
    let mut eval = super::super::eval::Context::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "abc")
             (let ((m (copy-marker (point-max) t)))
               (goto-char 2)
               (insert "X")
               (list (marker-position m)
                     (buffer-string))))"#,
    )
    .expect("parse copy-marker insertion regression");
    let result = eval
        .eval_forms(&forms)
        .into_iter()
        .last()
        .expect("one form")
        .expect("evaluation succeeds");
    assert_eq!(
        crate::emacs_core::error::format_eval_result(&Ok(result)),
        r#"OK (5 "aXbc")"#
    );
}

#[test]
fn set_marker_uses_live_source_marker_position_after_insertions() {
    let mut eval = super::super::eval::Context::new();
    let forms = super::super::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "abc")
             (let ((src (copy-marker (point-max) t))
                   (dst (make-marker)))
               (goto-char 2)
               (insert "X")
               (set-marker dst src)
               (marker-position dst)))"#,
    )
    .expect("parse set-marker source marker regression");
    let result = eval
        .eval_forms(&forms)
        .into_iter()
        .last()
        .expect("one form")
        .expect("evaluation succeeds");
    assert_eq!(
        crate::emacs_core::error::format_eval_result(&Ok(result)),
        "OK 5"
    );
}
