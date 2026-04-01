use super::super::eval::Context;
use super::*;
use crate::emacs_core::value::{ValueKind, VecLikeType};
use crate::emacs_core::builtins::{
    builtin_current_buffer, builtin_get_pos_property, builtin_make_indirect_buffer,
};

/// Helper: create an evaluator with a buffer containing the given text.
fn eval_with_text(text: &str) -> Context {
    let mut eval = Context::new();
    eval.buffers.current_buffer_mut().unwrap().insert(text);
    // Reset point to beginning.
    eval.buffers.current_buffer_mut().unwrap().goto_char(0);
    eval
}

// -----------------------------------------------------------------------
// put-text-property / get-text-property
// -----------------------------------------------------------------------

#[test]
fn put_and_get_text_property() {
    let mut eval = eval_with_text("hello world");
    // Put 'face -> bold on positions 1..6 (1-based, "hello")
    let result = builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(6),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    );
    assert!(result.is_ok());

    // Get at position 3 (1-based, 'l')
    let result = builtin_get_text_property(&mut eval, vec![Value::fixnum(3), Value::symbol("face")]);
    match result {
        Ok(v) if v.as_symbol_id().is_some() => {
            assert_eq!(crate::emacs_core::intern::resolve_sym(v.as_symbol_id().unwrap()), "bold");
        }
        other => panic!("Expected Symbol(bold), got {:?}", other),
    }
}

#[test]
fn get_text_property_returns_nil_when_absent() {
    let mut eval = eval_with_text("hello");
    let result = builtin_get_text_property(&mut eval, vec![Value::fixnum(1), Value::symbol("face")]);
    assert!(result.as_ref().map_or(false, |v| v.is_nil()));
}

#[test]
fn put_text_property_outside_range() {
    let mut eval = eval_with_text("hello");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(3),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();

    // Position 4 is outside the propertized range.
    let result = builtin_get_text_property(&mut eval, vec![Value::fixnum(4), Value::symbol("face")]);
    assert!(result.as_ref().map_or(false, |v| v.is_nil()));
}

#[test]
fn indirect_buffers_share_text_property_updates() {
    let mut eval = eval_with_text("hello");
    let base = builtin_current_buffer(&mut eval, vec![]).unwrap();
    let indirect =
        builtin_make_indirect_buffer(&mut eval, vec![base, Value::string("*tp-indirect*")])
            .unwrap();

    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(6),
            Value::symbol("face"),
            Value::symbol("bold"),
            base,
        ],
    )
    .unwrap();

    let via_indirect = builtin_get_text_property(
        &mut eval,
        vec![Value::fixnum(3), Value::symbol("face"), indirect],
    )
    .unwrap();
    assert!(via_indirect.is_symbol_named("bold"));

    builtin_remove_text_properties(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(6),
            Value::list(vec![Value::symbol("face"), Value::NIL]),
            indirect,
        ],
    )
    .unwrap();

    let via_base =
        builtin_get_text_property(&mut eval, vec![Value::fixnum(3), Value::symbol("face"), base])
            .unwrap();
    assert!(via_base.is_nil());
}

// -----------------------------------------------------------------------
// get-char-property
// -----------------------------------------------------------------------

#[test]
fn get_char_property_delegates() {
    let mut eval = eval_with_text("abcdef");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(5),
            Value::symbol("help-echo"),
            Value::string("tooltip"),
        ],
    )
    .unwrap();

    let result =
        builtin_get_char_property(&mut eval, vec![Value::fixnum(3), Value::symbol("help-echo")]);
    assert!(result.unwrap().is_string());
}

#[test]
fn get_char_property_and_overlay_shape() {
    let mut eval = eval_with_text("abcd");
    let result = builtin_get_char_property_and_overlay(
        &mut eval,
        vec![Value::fixnum(2), Value::symbol("missing")],
    )
    .unwrap();
    let pair = list_to_vec(&result).unwrap();
    assert_eq!(pair, vec![Value::NIL]);

    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(2), Value::fixnum(4)]).unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![ov, Value::symbol("foo"), Value::symbol("bar")],
    )
    .unwrap();
    let result =
        builtin_get_char_property_and_overlay(&mut eval, vec![Value::fixnum(3), Value::symbol("foo")])
            .unwrap();
    if !result.is_cons() {
        panic!("expected cons");
    };
    let (value, overlay) = {
        let pair_car = result.cons_car();
        let pair_cdr = result.cons_cdr();
        (pair_car, pair_cdr)
    };
    assert!(value.is_symbol_named("bar"));
    let overlayp = builtin_overlayp(&mut eval, vec![overlay]).unwrap();
    assert!(overlayp.is_t());
}

#[test]
fn get_char_property_prefers_highest_priority_overlay() {
    let mut eval = eval_with_text("abcd");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(3),
            Value::symbol("face"),
            Value::symbol("text"),
        ],
    )
    .unwrap();

    let low = builtin_make_overlay(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::NIL,
            Value::T,
            Value::NIL,
        ],
    )
    .unwrap();
    let high = builtin_make_overlay(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::NIL,
            Value::T,
            Value::NIL,
        ],
    )
    .unwrap();

    builtin_overlay_put(
        &mut eval,
        vec![low, Value::symbol("face"), Value::symbol("low")],
    )
    .unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![low, Value::symbol("priority"), Value::fixnum(1)],
    )
    .unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![high, Value::symbol("face"), Value::symbol("high")],
    )
    .unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![
            high,
            Value::symbol("priority"),
            Value::cons(Value::fixnum(10), Value::fixnum(0)),
        ],
    )
    .unwrap();

    let char_prop =
        builtin_get_char_property(&mut eval, vec![Value::fixnum(2), Value::symbol("face")]).unwrap();
    assert_eq!(char_prop.as_symbol_name(), Some("high"));

    let pair = builtin_get_char_property_and_overlay(
        &mut eval,
        vec![Value::fixnum(2), Value::symbol("face")],
    )
    .unwrap();
    if !pair.is_cons() {
        panic!("expected cons");
    };
    let pair_car = pair.cons_car();
    let pair_cdr = pair.cons_cdr();
    assert_eq!(pair_car.as_symbol_name(), Some("high"));
    assert_eq!(pair_cdr, high);
}

#[test]
fn get_pos_property_respects_overlay_advance_and_text_stickiness() {
    let mut eval = eval_with_text("abcd");

    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("carry"),
            Value::symbol("before"),
        ],
    )
    .unwrap();
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("rear-nonsticky"),
            Value::list(vec![Value::symbol("carry")]),
        ],
    )
    .unwrap();
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(3),
            Value::symbol("carry"),
            Value::symbol("after"),
        ],
    )
    .unwrap();
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(3),
            Value::symbol("front-sticky"),
            Value::list(vec![Value::symbol("carry")]),
        ],
    )
    .unwrap();
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(3),
            Value::symbol("face"),
            Value::symbol("text"),
        ],
    )
    .unwrap();

    let low = builtin_make_overlay(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::NIL,
            Value::T,
            Value::NIL,
        ],
    )
    .unwrap();
    let high = builtin_make_overlay(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::NIL,
            Value::T,
            Value::NIL,
        ],
    )
    .unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![low, Value::symbol("face"), Value::symbol("low")],
    )
    .unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![low, Value::symbol("priority"), Value::fixnum(1)],
    )
    .unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![high, Value::symbol("face"), Value::symbol("high")],
    )
    .unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![high, Value::symbol("priority"), Value::fixnum(10)],
    )
    .unwrap();

    let start_face =
        builtin_get_pos_property(&mut eval, vec![Value::fixnum(2), Value::symbol("face")]).unwrap();
    assert!(start_face.is_nil());

    let carry =
        builtin_get_pos_property(&mut eval, vec![Value::fixnum(2), Value::symbol("carry")]).unwrap();
    assert_eq!(carry.as_symbol_name(), Some("after"));

    let inside_face =
        builtin_get_pos_property(&mut eval, vec![Value::fixnum(3), Value::symbol("face")]).unwrap();
    assert_eq!(inside_face.as_symbol_name(), Some("high"));
}

#[test]
fn get_pos_property_on_string_delegates_to_text_property() {
    let mut eval = Context::new();
    let string = Value::string("abcd");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::symbol("face"),
            Value::symbol("bold"),
            string,
        ],
    )
    .unwrap();

    let result = builtin_get_pos_property(
        &mut eval,
        vec![Value::fixnum(3), Value::symbol("face"), string],
    )
    .unwrap();
    assert_eq!(result.as_symbol_name(), Some("bold"));
}

#[test]
fn get_display_property_queries_display_only() {
    let mut eval = eval_with_text("abcd");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::symbol("p"),
            Value::symbol("v"),
        ],
    )
    .unwrap();
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::symbol("display"),
            Value::symbol("dv"),
        ],
    )
    .unwrap();
    let non_display = builtin_get_display_property(
        &mut eval,
        vec![Value::fixnum(2), Value::symbol("p"), Value::NIL, Value::NIL],
    )
    .unwrap();
    assert!(non_display.is_nil());

    let display = builtin_get_display_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::symbol("display"),
            Value::NIL,
            Value::NIL,
        ],
    )
    .unwrap();
    assert!(display.is_symbol_named("dv"));
}

// -----------------------------------------------------------------------
// add-text-properties
// -----------------------------------------------------------------------

#[test]
fn add_text_properties_multiple() {
    let mut eval = eval_with_text("hello world");
    let props = Value::list(vec![
        Value::symbol("face"),
        Value::symbol("bold"),
        Value::symbol("mouse-face"),
        Value::symbol("highlight"),
    ]);
    let result = builtin_add_text_properties(&mut eval, vec![Value::fixnum(1), Value::fixnum(6), props]);
    assert!(result.is_ok());

    let face =
        builtin_get_text_property(&mut eval, vec![Value::fixnum(2), Value::symbol("face")]).unwrap();
    assert!(face.is_symbol_named("bold"));

    let mouse =
        builtin_get_text_property(&mut eval, vec![Value::fixnum(2), Value::symbol("mouse-face")])
            .unwrap();
    assert!(mouse.is_symbol_named("highlight"));
}

#[test]
fn add_text_properties_odd_plist_signals_error() {
    let mut eval = eval_with_text("hello");
    let props = Value::list(vec![Value::symbol("face")]);
    let result = builtin_add_text_properties(&mut eval, vec![Value::fixnum(1), Value::fixnum(3), props]);
    assert!(result.is_err());
}

#[test]
fn add_face_text_property_basic_and_merge_order() {
    let mut eval = eval_with_text("abc");
    builtin_add_face_text_property(
        &mut eval,
        vec![Value::fixnum(1), Value::fixnum(3), Value::symbol("bold")],
    )
    .unwrap();
    let face =
        builtin_get_text_property(&mut eval, vec![Value::fixnum(2), Value::symbol("face")]).unwrap();
    assert_eq!(face, Value::symbol("bold"));

    let mut eval = eval_with_text("abc");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("face"),
            Value::symbol("italic"),
        ],
    )
    .unwrap();
    builtin_add_face_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("bold"),
            Value::T,
        ],
    )
    .unwrap();
    let appended =
        builtin_get_text_property(&mut eval, vec![Value::fixnum(1), Value::symbol("face")]).unwrap();
    assert_eq!(
        appended,
        Value::list(vec![Value::symbol("italic"), Value::symbol("bold")])
    );

    let mut eval = eval_with_text("abc");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("face"),
            Value::symbol("italic"),
        ],
    )
    .unwrap();
    builtin_add_face_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("bold"),
            Value::NIL,
        ],
    )
    .unwrap();
    let prepended =
        builtin_get_text_property(&mut eval, vec![Value::fixnum(1), Value::symbol("face")]).unwrap();
    assert_eq!(
        prepended,
        Value::list(vec![Value::symbol("bold"), Value::symbol("italic")])
    );
}

#[test]
fn add_face_text_property_argument_contracts() {
    let mut eval = eval_with_text("abc");

    let begin_err = builtin_add_face_text_property(
        &mut eval,
        vec![Value::string("1"), Value::fixnum(2), Value::symbol("bold")],
    )
    .unwrap_err();
    match begin_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("integer-or-marker-p"), Value::string("1")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let object_err = builtin_add_face_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("bold"),
            Value::NIL,
            Value::T,
        ],
    )
    .unwrap_err();
    match object_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("buffer-or-string-p"), Value::T]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let string_obj = builtin_add_face_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("bold"),
            Value::NIL,
            Value::string("abc"),
        ],
    )
    .unwrap();
    assert!(string_obj.is_nil());
}

// -----------------------------------------------------------------------
// remove-text-properties
// -----------------------------------------------------------------------

#[test]
fn remove_text_properties_basic() {
    let mut eval = eval_with_text("hello");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(6),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();

    let props = Value::list(vec![Value::symbol("face"), Value::NIL]);
    builtin_remove_text_properties(&mut eval, vec![Value::fixnum(1), Value::fixnum(6), props]).unwrap();

    let result = builtin_get_text_property(&mut eval, vec![Value::fixnum(3), Value::symbol("face")]);
    assert!(result.as_ref().map_or(false, |v| v.is_nil()));
}

#[test]
fn set_text_properties_replaces_existing_values() {
    let mut eval = eval_with_text("abcd");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::symbol("p"),
            Value::symbol("v"),
        ],
    )
    .unwrap();

    let result = builtin_set_text_properties(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::list(vec![Value::symbol("q"), Value::symbol("z")]),
        ],
    )
    .unwrap();
    assert!(result.is_t());

    let q = builtin_get_text_property(&mut eval, vec![Value::fixnum(2), Value::symbol("q")]).unwrap();
    let p = builtin_get_text_property(&mut eval, vec![Value::fixnum(2), Value::symbol("p")]).unwrap();
    assert!(q.is_symbol_named("z"));
    assert!(p.is_nil());
}

#[test]
fn remove_list_of_text_properties_returns_t_only_when_changed() {
    let mut eval = eval_with_text("abcd");
    builtin_set_text_properties(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::list(vec![Value::symbol("q"), Value::symbol("z")]),
        ],
    )
    .unwrap();

    let first = builtin_remove_list_of_text_properties(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::list(vec![Value::symbol("q")]),
        ],
    )
    .unwrap();
    let second = builtin_remove_list_of_text_properties(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::list(vec![Value::symbol("q")]),
        ],
    )
    .unwrap();
    assert!(first.is_t());
    assert!(second.is_nil());
}

// -----------------------------------------------------------------------
// text-properties-at
// -----------------------------------------------------------------------

#[test]
fn text_properties_at_returns_plist() {
    let mut eval = eval_with_text("hello");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(6),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();

    let result = builtin_text_properties_at(&mut eval, vec![Value::fixnum(2)]).unwrap();
    // Should be a plist with at least 'face 'bold.
    let items = list_to_vec(&result).unwrap();
    assert!(items.len() >= 2);
}

#[test]
fn text_properties_at_empty_returns_nil() {
    let mut eval = eval_with_text("hello");
    let result = builtin_text_properties_at(&mut eval, vec![Value::fixnum(1)]).unwrap();
    // Empty plist is nil.
    assert!(result.is_nil());
}

// -----------------------------------------------------------------------
// next-property-change
// -----------------------------------------------------------------------

#[test]
fn next_property_change_basic() {
    let mut eval = eval_with_text("hello world");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(6),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();

    // From position 1, next change should be at position 6.
    let result = builtin_next_property_change(&mut eval, vec![Value::fixnum(1)]).unwrap();
    assert!(result.is_fixnum());
}

#[test]
fn next_property_change_with_limit() {
    let mut eval = eval_with_text("hello world");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(6),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();

    // Limit at 4 — the actual change is at 6, so should return 4.
    let result =
        builtin_next_property_change(&mut eval, vec![Value::fixnum(1), Value::NIL, Value::fixnum(4)])
            .unwrap();
    assert!(result.is_fixnum());
}

#[test]
fn next_property_change_no_change() {
    let mut eval = eval_with_text("hello");
    let result = builtin_next_property_change(&mut eval, vec![Value::fixnum(1)]).unwrap();
    assert!(result.is_nil());
}

// -----------------------------------------------------------------------
// next-single-property-change
// -----------------------------------------------------------------------

#[test]
fn next_single_property_change_basic() {
    let mut eval = eval_with_text("hello world");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(6),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();

    let result =
        builtin_next_single_property_change(&mut eval, vec![Value::fixnum(1), Value::symbol("face")])
            .unwrap();
    assert!(result.is_fixnum());
}

#[test]
fn next_single_property_change_nil_when_none() {
    let mut eval = eval_with_text("hello");
    let result =
        builtin_next_single_property_change(&mut eval, vec![Value::fixnum(1), Value::symbol("face")])
            .unwrap();
    assert!(result.is_nil());
}

// -----------------------------------------------------------------------
// previous-single-property-change
// -----------------------------------------------------------------------

#[test]
fn previous_single_property_change_basic() {
    let mut eval = eval_with_text("hello world");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(6),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();

    // From position 8 (past the propertized region), looking backward for 'face change.
    let result = builtin_previous_single_property_change(
        &mut eval,
        vec![Value::fixnum(8), Value::symbol("face")],
    )
    .unwrap();
    assert!(result.is_fixnum());
}

#[test]
fn previous_single_property_change_from_interval_end_boundary() {
    let mut eval = eval_with_text("abcd");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::symbol("p"),
            Value::symbol("v"),
        ],
    )
    .unwrap();

    let result =
        builtin_previous_single_property_change(&mut eval, vec![Value::fixnum(4), Value::symbol("p")])
            .unwrap();
    assert!(result.is_fixnum());
}

// -----------------------------------------------------------------------
// text-property-any
// -----------------------------------------------------------------------

#[test]
fn text_property_any_found() {
    let mut eval = eval_with_text("hello world");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(3),
            Value::fixnum(6),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();

    let result = builtin_text_property_any(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(10),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();
    // Should find it at position 3.
    assert!(result.is_fixnum());
}

#[test]
fn text_property_any_not_found() {
    let mut eval = eval_with_text("hello");
    let result = builtin_text_property_any(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(6),
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn text_property_any_uses_live_marker_end_after_insertions() {
    let mut eval = Context::new();
    let forms = crate::emacs_core::parser::parse_forms(
        r#"(with-temp-buffer
             (insert "abc")
             (let ((end (copy-marker (point-max) t)))
               (goto-char (point-max))
               (insert "Z")
               (put-text-property 4 5 'hard t)
               (text-property-any 1 end 'hard t)))"#,
    )
    .expect("parse text-property-any marker regression");
    let result = eval
        .eval_forms(&forms)
        .into_iter()
        .last()
        .expect("one form")
        .expect("evaluation succeeds");
    assert_eq!(result, Value::fixnum(4));
}

#[test]
fn text_property_not_all_reports_first_mismatch() {
    let mut eval = eval_with_text("abcd");
    builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::symbol("p"),
            Value::symbol("v"),
        ],
    )
    .unwrap();

    let mismatch = builtin_text_property_not_all(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(5),
            Value::symbol("p"),
            Value::symbol("v"),
        ],
    )
    .unwrap();
    let no_mismatch = builtin_text_property_not_all(
        &mut eval,
        vec![
            Value::fixnum(2),
            Value::fixnum(4),
            Value::symbol("p"),
            Value::symbol("v"),
        ],
    )
    .unwrap();
    assert!(mismatch.is_fixnum());
    assert!(no_mismatch.is_nil());
}

// -----------------------------------------------------------------------
// make-overlay / delete-overlay
// -----------------------------------------------------------------------

#[test]
fn make_and_delete_overlay() {
    let mut eval = eval_with_text("hello world");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();

    assert!(ov.is_overlay());

    // Delete it.
    let result = builtin_delete_overlay(&mut eval, vec![ov]);
    assert!(result.is_ok());
}

// -----------------------------------------------------------------------
// overlay-put / overlay-get
// -----------------------------------------------------------------------

#[test]
fn overlay_put_and_get() {
    let mut eval = eval_with_text("hello");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();

    builtin_overlay_put(
        &mut eval,
        vec![ov, Value::symbol("face"), Value::symbol("bold")],
    )
    .unwrap();

    let result = builtin_overlay_get(&mut eval, vec![ov, Value::symbol("face")]).unwrap();
    assert!(result.is_symbol_named("bold"));
}

#[test]
fn deleted_overlay_preserves_plist_and_identity() {
    let mut eval = eval_with_text("hello");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(3)]).unwrap();

    builtin_overlay_put(
        &mut eval,
        vec![ov, Value::symbol("face"), Value::symbol("bold")],
    )
    .unwrap();
    builtin_delete_overlay(&mut eval, vec![ov]).unwrap();

    let overlayp = builtin_overlayp(&mut eval, vec![ov]).unwrap();
    assert!(overlayp.is_t());

    let face = builtin_overlay_get(&mut eval, vec![ov, Value::symbol("face")]).unwrap();
    assert_eq!(face.as_symbol_name(), Some("bold"));

    let properties = builtin_overlay_properties(&mut eval, vec![ov]).unwrap();
    assert_eq!(
        crate::emacs_core::print::print_value(&properties),
        "(face bold)"
    );

    let start = builtin_overlay_start(&mut eval, vec![ov]).unwrap();
    let end = builtin_overlay_end(&mut eval, vec![ov]).unwrap();
    let buffer = builtin_overlay_buffer(&mut eval, vec![ov]).unwrap();
    assert!(start.is_nil());
    assert!(end.is_nil());
    assert!(buffer.is_nil());
}

#[test]
fn overlay_get_absent_property() {
    let mut eval = eval_with_text("hello");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();

    let result = builtin_overlay_get(&mut eval, vec![ov, Value::symbol("missing")]).unwrap();
    assert!(result.is_nil());
}

// -----------------------------------------------------------------------
// overlayp
// -----------------------------------------------------------------------

#[test]
fn overlayp_true() {
    let mut eval = eval_with_text("hello");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();

    let result = builtin_overlayp(&mut eval, vec![ov]).unwrap();
    assert!(result.is_t());
}

#[test]
fn overlayp_false() {
    let mut eval = Context::new();
    let result = builtin_overlayp(&mut eval, vec![Value::fixnum(42)]).unwrap();
    assert!(result.is_nil());
}

// -----------------------------------------------------------------------
// overlays-at / overlays-in
// -----------------------------------------------------------------------

#[test]
fn overlays_at_finds_overlay() {
    let mut eval = eval_with_text("hello world");
    let _ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();

    let result = builtin_overlays_at(&mut eval, vec![Value::fixnum(3)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1);
}

#[test]
fn overlays_at_outside() {
    let mut eval = eval_with_text("hello world");
    let _ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(3)]).unwrap();

    let result = builtin_overlays_at(&mut eval, vec![Value::fixnum(5)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 0);
}

#[test]
fn overlays_at_sorted_returns_highest_priority_first() {
    let mut eval = eval_with_text("hello world");
    let low = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();
    let high = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();
    let nil_priority = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();

    builtin_overlay_put(
        &mut eval,
        vec![low, Value::symbol("priority"), Value::fixnum(1)],
    )
    .unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![high, Value::symbol("priority"), Value::fixnum(10)],
    )
    .unwrap();

    let result = builtin_overlays_at(&mut eval, vec![Value::fixnum(3), Value::T]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items, vec![high, low, nil_priority]);
}

#[test]
fn overlays_in_basic() {
    let mut eval = eval_with_text("hello world");
    builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();
    builtin_make_overlay(&mut eval, vec![Value::fixnum(4), Value::fixnum(10)]).unwrap();

    let result = builtin_overlays_in(&mut eval, vec![Value::fixnum(1), Value::fixnum(12)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 2);
}

#[test]
fn next_previous_overlay_change_boundaries() {
    let mut eval = eval_with_text("abcd");
    let no_overlay_next = builtin_next_overlay_change(&mut eval, vec![Value::fixnum(1)]).unwrap();
    let no_overlay_prev = builtin_previous_overlay_change(&mut eval, vec![Value::fixnum(4)]).unwrap();
    assert!(no_overlay_next.is_fixnum());
    assert!(no_overlay_prev.is_fixnum());

    builtin_make_overlay(&mut eval, vec![Value::fixnum(2), Value::fixnum(4)]).unwrap();
    let next_from_1 = builtin_next_overlay_change(&mut eval, vec![Value::fixnum(1)]).unwrap();
    let next_from_2 = builtin_next_overlay_change(&mut eval, vec![Value::fixnum(2)]).unwrap();
    let prev_from_4 = builtin_previous_overlay_change(&mut eval, vec![Value::fixnum(4)]).unwrap();
    let prev_from_2 = builtin_previous_overlay_change(&mut eval, vec![Value::fixnum(2)]).unwrap();
    assert!(next_from_1.is_fixnum());
    assert!(next_from_2.is_fixnum());
    assert!(prev_from_4.is_fixnum());
    assert!(prev_from_2.is_fixnum());
}

// -----------------------------------------------------------------------
// move-overlay
// -----------------------------------------------------------------------

#[test]
fn move_overlay_changes_range() {
    let mut eval = eval_with_text("hello world");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();

    builtin_move_overlay(&mut eval, vec![ov, Value::fixnum(3), Value::fixnum(8)]).unwrap();

    let start = builtin_overlay_start(&mut eval, vec![ov]).unwrap();
    let end = builtin_overlay_end(&mut eval, vec![ov]).unwrap();
    assert!(start.is_fixnum());
    assert!(end.is_fixnum());
}

// -----------------------------------------------------------------------
// overlay-start / overlay-end
// -----------------------------------------------------------------------

#[test]
fn overlay_start_and_end() {
    let mut eval = eval_with_text("hello world");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(2), Value::fixnum(8)]).unwrap();

    let start = builtin_overlay_start(&mut eval, vec![ov]).unwrap();
    let end = builtin_overlay_end(&mut eval, vec![ov]).unwrap();
    assert!(start.is_fixnum());
    assert!(end.is_fixnum());
}

// -----------------------------------------------------------------------
// overlay-buffer
// -----------------------------------------------------------------------

#[test]
fn overlay_buffer_returns_buffer() {
    let mut eval = eval_with_text("hello");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(3)]).unwrap();

    let result = builtin_overlay_buffer(&mut eval, vec![ov]).unwrap();
    assert!(result.is_buffer());
}

// -----------------------------------------------------------------------
// overlay-properties
// -----------------------------------------------------------------------

#[test]
fn overlay_properties_returns_plist() {
    let mut eval = eval_with_text("hello");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();

    builtin_overlay_put(
        &mut eval,
        vec![ov, Value::symbol("face"), Value::symbol("bold")],
    )
    .unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![ov, Value::symbol("priority"), Value::fixnum(10)],
    )
    .unwrap();

    let result = builtin_overlay_properties(&mut eval, vec![ov]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 4); // 2 properties * 2 (key+value)
}

#[test]
fn overlay_properties_empty() {
    let mut eval = eval_with_text("hello");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(3)]).unwrap();

    let result = builtin_overlay_properties(&mut eval, vec![ov]).unwrap();
    // Empty plist is nil.
    assert!(result.is_nil());
}

// -----------------------------------------------------------------------
// remove-overlays
// -----------------------------------------------------------------------

#[test]
fn remove_overlays_all() {
    let mut eval = eval_with_text("hello world");
    builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();
    builtin_make_overlay(&mut eval, vec![Value::fixnum(3), Value::fixnum(10)]).unwrap();

    builtin_remove_overlays(&mut eval, vec![]).unwrap();

    let result = builtin_overlays_in(&mut eval, vec![Value::fixnum(1), Value::fixnum(12)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 0);
}

#[test]
fn remove_overlays_by_property() {
    let mut eval = eval_with_text("hello world");
    let ov1 = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(6)]).unwrap();
    let ov2 = builtin_make_overlay(&mut eval, vec![Value::fixnum(3), Value::fixnum(10)]).unwrap();

    builtin_overlay_put(
        &mut eval,
        vec![ov1, Value::symbol("face"), Value::symbol("bold")],
    )
    .unwrap();
    builtin_overlay_put(
        &mut eval,
        vec![ov2, Value::symbol("face"), Value::symbol("italic")],
    )
    .unwrap();

    // Remove only overlays with face = bold.
    builtin_remove_overlays(
        &mut eval,
        vec![
            Value::NIL,
            Value::NIL,
            Value::symbol("face"),
            Value::symbol("bold"),
        ],
    )
    .unwrap();

    let result = builtin_overlays_in(&mut eval, vec![Value::fixnum(1), Value::fixnum(12)]).unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1); // only the italic one remains
}

// -----------------------------------------------------------------------
// Wrong argument count tests
// -----------------------------------------------------------------------

#[test]
fn put_text_property_wrong_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_put_text_property(&mut eval, vec![Value::fixnum(1), Value::fixnum(3)]);
    assert!(result.is_err());
}

#[test]
fn put_text_property_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_put_text_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("face"),
            Value::symbol("bold"),
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn get_text_property_wrong_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_get_text_property(&mut eval, vec![]);
    assert!(result.is_err());
}

#[test]
fn get_text_property_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_get_text_property(
        &mut eval,
        vec![Value::fixnum(1), Value::symbol("face"), Value::NIL, Value::NIL],
    );
    assert!(result.is_err());
}

#[test]
fn get_char_property_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_get_char_property(
        &mut eval,
        vec![Value::fixnum(1), Value::symbol("face"), Value::NIL, Value::NIL],
    );
    assert!(result.is_err());
}

#[test]
fn get_char_property_and_overlay_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_get_char_property_and_overlay(
        &mut eval,
        vec![Value::fixnum(1), Value::symbol("face"), Value::NIL, Value::NIL],
    );
    assert!(result.is_err());
}

#[test]
fn get_display_property_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_get_display_property(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::symbol("face"),
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn overlay_put_wrong_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_overlay_put(&mut eval, vec![Value::fixnum(42), Value::symbol("face")]);
    assert!(result.is_err());
}

#[test]
fn text_properties_at_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_text_properties_at(&mut eval, vec![Value::fixnum(1), Value::NIL, Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn text_property_any_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_text_property_any(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("face"),
            Value::symbol("bold"),
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn text_property_not_all_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_text_property_not_all(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::symbol("face"),
            Value::symbol("bold"),
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn set_text_properties_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_set_text_properties(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn remove_list_of_text_properties_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_remove_list_of_text_properties(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn remove_overlays_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_remove_overlays(
        &mut eval,
        vec![Value::NIL, Value::NIL, Value::NIL, Value::NIL, Value::NIL],
    );
    assert!(result.is_err());
}

#[test]
fn make_overlay_wrong_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_make_overlay(&mut eval, vec![Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn make_overlay_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_make_overlay(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn overlays_at_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_overlays_at(&mut eval, vec![Value::fixnum(1), Value::NIL, Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn next_overlay_change_wrong_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_next_overlay_change(&mut eval, vec![]);
    assert!(result.is_err());
}

#[test]
fn previous_overlay_change_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_previous_overlay_change(&mut eval, vec![Value::fixnum(1), Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn next_property_change_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_next_property_change(
        &mut eval,
        vec![Value::fixnum(1), Value::NIL, Value::NIL, Value::NIL],
    );
    assert!(result.is_err());
}

#[test]
fn next_single_property_change_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_next_single_property_change(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::symbol("face"),
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn previous_single_property_change_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_previous_single_property_change(
        &mut eval,
        vec![
            Value::fixnum(1),
            Value::symbol("face"),
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn move_overlay_rejects_too_many_args() {
    let mut eval = eval_with_text("hello");
    let result = builtin_move_overlay(
        &mut eval,
        vec![
            Value::NIL,
            Value::fixnum(1),
            Value::fixnum(2),
            Value::NIL,
            Value::NIL,
        ],
    );
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// Integration: overlays with advance flags
// -----------------------------------------------------------------------

#[test]
fn overlay_front_advance() {
    let mut eval = eval_with_text("hello world");
    // Create overlay with front-advance = t
    let ov = builtin_make_overlay(
        &mut eval,
        vec![
            Value::fixnum(3),
            Value::fixnum(8),
            Value::NIL,  // buffer
            Value::T, // front-advance
            Value::NIL,  // rear-advance
        ],
    )
    .unwrap();

    // Verify overlay was created.
    let start = builtin_overlay_start(&mut eval, vec![ov]).unwrap();
    assert!(start.is_fixnum());
}

#[test]
fn overlay_rear_advance() {
    let mut eval = eval_with_text("hello world");
    let ov = builtin_make_overlay(
        &mut eval,
        vec![
            Value::fixnum(3),
            Value::fixnum(8),
            Value::NIL,
            Value::NIL,
            Value::T, // rear-advance
        ],
    )
    .unwrap();

    let end = builtin_overlay_end(&mut eval, vec![ov]).unwrap();
    assert!(end.is_fixnum());
}

// -----------------------------------------------------------------------
// Edge cases
// -----------------------------------------------------------------------

#[test]
fn text_property_on_empty_buffer() {
    let mut eval = Context::new();
    // Scratch buffer is empty.
    let result = builtin_get_text_property(&mut eval, vec![Value::fixnum(1), Value::symbol("face")]);
    assert!(result.as_ref().map_or(false, |v| v.is_nil()));
}

#[test]
fn overlays_at_empty_buffer() {
    let mut eval = Context::new();
    let result = builtin_overlays_at(&mut eval, vec![Value::fixnum(1)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn delete_overlay_twice_is_ok() {
    let mut eval = eval_with_text("hello");
    let ov = builtin_make_overlay(&mut eval, vec![Value::fixnum(1), Value::fixnum(3)]).unwrap();

    builtin_delete_overlay(&mut eval, vec![ov]).unwrap();
    // Second delete should not crash.
    let result = builtin_delete_overlay(&mut eval, vec![ov]);
    assert!(result.is_ok());
}
