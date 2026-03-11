use super::*;

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
    let pos = builtin_marker_position(vec![m]).unwrap();
    assert!(matches!(pos, Value::Int(10)));
}

#[test]
fn builtin_marker_position_returns_nil_when_unset() {
    let m = make_marker_value(None, None, false);
    let pos = builtin_marker_position(vec![m]).unwrap();
    assert!(pos.is_nil());
}

#[test]
fn builtin_marker_buffer_returns_name() {
    let m = make_marker_value(Some("*scratch*"), Some(1), false);
    let buf = builtin_marker_buffer(vec![m]).unwrap();
    assert_eq!(buf.as_str(), Some("*scratch*"));
}

#[test]
fn builtin_marker_insertion_type_roundtrip() {
    let m = make_marker_value(None, None, false);
    assert!(builtin_marker_insertion_type(vec![m]).unwrap().is_nil());

    builtin_set_marker_insertion_type(vec![m, Value::True]).unwrap();
    assert!(builtin_marker_insertion_type(vec![m]).unwrap().is_truthy());
}

#[test]
fn builtin_copy_marker_from_marker() {
    let m = make_marker_value(Some("buf"), Some(5), true);
    let copy = builtin_copy_marker(vec![m]).unwrap();
    assert!(is_marker(&copy));
    assert!(matches!(marker_position_value(&copy), Value::Int(5)));
}

#[test]
fn builtin_copy_marker_from_integer() {
    let copy = builtin_copy_marker(vec![Value::Int(99)]).unwrap();
    assert!(is_marker(&copy));
    assert!(matches!(marker_position_value(&copy), Value::Int(99)));
    assert!(marker_buffer_value(&copy).is_nil());
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
    let result = builtin_marker_position(vec![Value::Int(5)]);
    assert!(result.is_err());
}

#[test]
fn marker_accessors_require_zero_arguments() {
    let mut eval = super::super::eval::Evaluator::new();

    assert!(builtin_point_marker(&mut eval, vec![Value::Nil]).is_err());
    assert!(builtin_point_min_marker(&mut eval, vec![Value::Nil]).is_err());
    assert!(builtin_point_max_marker(&mut eval, vec![Value::Nil]).is_err());
    assert!(builtin_mark_marker(&mut eval, vec![Value::Nil]).is_err());
}

#[test]
fn numeric_comparisons_use_live_marker_positions() {
    let mut eval = super::super::eval::Evaluator::new();
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
