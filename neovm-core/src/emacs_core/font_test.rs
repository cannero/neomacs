use super::*;
use crate::emacs_core::eval::{
    DisplayHost, FontResolveRequest, GuiFrameHostRequest, ResolvedFontMatch,
};
use std::cell::RefCell;
use std::rc::Rc;

// -----------------------------------------------------------------------
// Font builtins
// -----------------------------------------------------------------------

#[derive(Default)]
struct FontAtDisplayHost {
    matched: Option<ResolvedFontMatch>,
}

impl DisplayHost for FontAtDisplayHost {
    fn realize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resolve_font_for_char(
        &mut self,
        request: FontResolveRequest,
    ) -> Result<Option<ResolvedFontMatch>, String> {
        if request.character == '好' {
            Ok(self.matched.clone())
        } else {
            Ok(None)
        }
    }
}

struct CapturingFontAtDisplayHost {
    last_request: Rc<RefCell<Option<FontResolveRequest>>>,
}

impl DisplayHost for CapturingFontAtDisplayHost {
    fn realize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resolve_font_for_char(
        &mut self,
        request: FontResolveRequest,
    ) -> Result<Option<ResolvedFontMatch>, String> {
        *self.last_request.borrow_mut() = Some(request);
        Ok(None)
    }
}

#[test]
fn fontp_on_non_font() {
    assert!(builtin_fontp(vec![Value::Int(42)]).unwrap().is_nil());
    assert!(
        builtin_fontp(vec![Value::string("hello")])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn font_spec_basic() {
    let spec = builtin_font_spec(vec![
        Value::Keyword(intern("family")),
        Value::string("Monospace"),
        Value::Keyword(intern("size")),
        Value::Int(12),
    ])
    .unwrap();
    assert!(is_font_spec(&spec));
    assert!(builtin_fontp(vec![spec]).unwrap().is_truthy());
}

#[test]
fn font_spec_odd_args_error() {
    let result = builtin_font_spec(vec![Value::Keyword(intern("family"))]);
    assert!(result.is_err());
}

#[test]
fn font_get_and_put() {
    let spec = builtin_font_spec(vec![
        Value::Keyword(intern("family")),
        Value::string("Monospace"),
    ])
    .unwrap();

    // Get existing property.
    let family = builtin_font_get(vec![spec, Value::Keyword(intern("family"))]).unwrap();
    assert_eq!(family.as_str(), Some("Monospace"));

    // Get missing property.
    let missing = builtin_font_get(vec![spec, Value::Keyword(intern("size"))]).unwrap();
    assert!(missing.is_nil());

    // Put returns VAL and mutates the original spec.
    let put_size =
        builtin_font_put(vec![spec, Value::Keyword(intern("size")), Value::Int(14)]).unwrap();
    assert_eq!(put_size.as_int(), Some(14));
    let size = builtin_font_get(vec![spec, Value::Keyword(intern("size"))]).unwrap();
    assert_eq!(size.as_int(), Some(14));

    // Overwrite existing property.
    let put_family = builtin_font_put(vec![
        spec,
        Value::Keyword(intern("family")),
        Value::string("Serif"),
    ])
    .unwrap();
    assert_eq!(put_family.as_str(), Some("Serif"));
    let family2 = builtin_font_get(vec![spec, Value::Keyword(intern("family"))]).unwrap();
    assert_eq!(family2.as_str(), Some("Serif"));
}

#[test]
fn font_get_symbol_key() {
    // Symbol key does not match keyword storage.
    let spec = builtin_font_spec(vec![
        Value::Keyword(intern("weight")),
        Value::symbol("bold"),
    ])
    .unwrap();
    let weight = builtin_font_get(vec![spec, Value::symbol("weight")]).unwrap();
    assert!(weight.is_nil());
}

#[test]
fn font_get_non_symbol_property_errors() {
    let spec = builtin_font_spec(vec![
        Value::Keyword(intern("weight")),
        Value::symbol("bold"),
    ])
    .unwrap();
    let result = builtin_font_get(vec![spec, Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn font_get_non_vector() {
    // font-get on a non-font value signals wrong-type-argument.
    let result = builtin_font_get(vec![Value::Int(42), Value::Keyword(intern("family"))]);
    assert!(result.is_err());
}

#[test]
fn list_fonts_returns_list_or_nil() {
    let result = builtin_list_fonts(vec![Value::vector(vec![Value::Keyword(intern(
        FONT_SPEC_TAG,
    ))])]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn list_fonts_rejects_non_font_spec() {
    let result = builtin_list_fonts(vec![Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn eval_list_fonts_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let result = builtin_list_fonts_eval(
        &mut eval,
        vec![
            Value::vector(vec![Value::Keyword(intern(FONT_SPEC_TAG))]),
            Value::Int(frame_id),
        ],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn find_font_returns_nil_for_font_spec() {
    let result = builtin_find_font(vec![Value::vector(vec![Value::Keyword(intern(
        FONT_SPEC_TAG,
    ))])]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn find_font_rejects_non_font_spec() {
    let result = builtin_find_font(vec![Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn eval_find_font_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let result = builtin_find_font_eval(
        &mut eval,
        vec![
            Value::vector(vec![Value::Keyword(intern(FONT_SPEC_TAG))]),
            Value::Int(frame_id),
        ],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn clear_font_cache_returns_nil() {
    assert!(builtin_clear_font_cache(vec![]).unwrap().is_nil());
}

#[test]
fn clear_font_cache_rejects_arity() {
    assert!(builtin_clear_font_cache(vec![Value::Nil]).is_err());
}

#[test]
fn clear_font_cache_resets_face_caches() {
    let face = Value::symbol("__neovm_clear_font_cache_unit_test");
    let _ = builtin_internal_make_lisp_face(vec![face]).unwrap();
    let _ = builtin_internal_set_lisp_face_attribute(vec![
        face,
        Value::Keyword(intern(":foreground")),
        Value::string("white"),
    ])
    .unwrap();

    CREATED_LISP_FACES.with(|slot| {
        assert!(slot.borrow().contains("__neovm_clear_font_cache_unit_test"));
    });
    FACE_ATTR_STATE.with(|slot| {
        assert!(!slot.borrow().selected_overrides.is_empty());
    });

    let result = builtin_clear_font_cache(vec![]).unwrap();
    assert!(result.is_nil());

    CREATED_LISP_FACES.with(|slot| assert!(slot.borrow().is_empty()));
    CREATED_FACE_IDS.with(|slot| assert!(slot.borrow().is_empty()));
    NEXT_CREATED_FACE_ID.with(|slot| {
        assert_eq!(*slot.borrow(), FIRST_DYNAMIC_FACE_ID);
    });
    FACE_ATTR_STATE.with(|slot| {
        let state = slot.borrow();
        assert!(state.selected_overrides.is_empty());
        assert!(state.defaults_overrides.is_empty());
        assert!(state.selected_created.is_empty());
    });
}

#[test]
fn font_family_list_batch_returns_nil() {
    let result = builtin_font_family_list(vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn font_family_list_rejects_non_nil_frame_designator() {
    let result = builtin_font_family_list(vec![Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn eval_font_family_list_accepts_live_frame_designator() {
    let mut eval = crate::emacs_core::Evaluator::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let result = builtin_font_family_list_eval(&mut eval, vec![Value::Int(frame_id)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn font_xlfd_name_returns_xlfd() {
    let result = builtin_font_xlfd_name(vec![Value::vector(vec![Value::Keyword(intern(
        FONT_SPEC_TAG,
    ))])])
    .unwrap();
    assert_eq!(result.as_str(), Some("-*-*-*-*-*-*-*-*-*-*-*-*-*-*"));
}

#[test]
fn font_xlfd_name_too_many_args() {
    let result = builtin_font_xlfd_name(vec![
        Value::vector(vec![Value::Keyword(intern(FONT_SPEC_TAG))]),
        Value::Nil,
        Value::Nil,
        Value::Nil,
    ]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn close_font_requires_font_object() {
    let wrong_nil = builtin_close_font(vec![Value::Nil]).unwrap_err();
    match wrong_nil {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("font-object"), Value::Nil]);
        }
        other => panic!("expected wrong-type-argument, got {other:?}"),
    }

    let wrong_spec = builtin_close_font(vec![builtin_font_spec(vec![]).unwrap()]).unwrap_err();
    match wrong_spec {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data[0], Value::symbol("font-object"));
        }
        other => panic!("expected wrong-type-argument, got {other:?}"),
    }
}

#[test]
fn close_font_accepts_tagged_font_object_and_checks_arity() {
    let font_obj = Value::vector(vec![Value::keyword("font-object"), Value::Int(1)]);
    assert!(builtin_close_font(vec![font_obj]).unwrap().is_nil());
    assert!(
        builtin_close_font(vec![font_obj, Value::Nil])
            .unwrap()
            .is_nil()
    );

    assert!(builtin_close_font(vec![]).is_err());
    assert!(builtin_close_font(vec![Value::Nil, Value::Nil, Value::Nil]).is_err());
}

#[test]
fn font_at_eval_returns_font_object_for_multibyte_buffer_face() {
    let mut eval = crate::emacs_core::Evaluator::new();
    crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);

    let face = Value::symbol("font-at-buffer-face");
    builtin_internal_make_lisp_face_eval(&mut eval, vec![face]).unwrap();
    builtin_internal_set_lisp_face_attribute_eval(
        &mut eval,
        vec![
            face,
            Value::Keyword(intern("family")),
            Value::string("Serif"),
        ],
    )
    .unwrap();

    let buffer = eval
        .buffers
        .current_buffer_mut()
        .expect("current buffer for font-at buffer test");
    buffer.insert("a好b");
    let start = buffer.text.char_to_byte(1);
    let end = buffer.text.char_to_byte(2);
    buffer.text_props.put_property(start, end, "face", face);

    let font = builtin_font_at_eval(&mut eval, vec![Value::Int(2)]).unwrap();
    assert!(
        builtin_fontp(vec![font, Value::symbol("font-object")])
            .unwrap()
            .is_truthy()
    );
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("family")])
            .unwrap()
            .as_str(),
        Some("Serif")
    );
}

#[test]
fn font_at_eval_returns_font_object_for_multibyte_string_face() {
    let mut eval = crate::emacs_core::Evaluator::new();
    crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);

    let face = Value::symbol("font-at-string-face");
    builtin_internal_make_lisp_face_eval(&mut eval, vec![face]).unwrap();
    builtin_internal_set_lisp_face_attribute_eval(
        &mut eval,
        vec![
            face,
            Value::Keyword(intern("family")),
            Value::string("Serif"),
        ],
    )
    .unwrap();

    let string = Value::string("a好b");
    let Value::Str(id) = string else {
        panic!("expected string value");
    };
    let mut table = crate::buffer::TextPropertyTable::new();
    let start = "a".len();
    let end = start + "好".len();
    table.put_property(start, end, "face", face);
    crate::emacs_core::value::set_string_text_properties_table(id, table);

    let font = builtin_font_at_eval(&mut eval, vec![Value::Int(1), Value::Nil, string]).unwrap();
    assert!(
        builtin_fontp(vec![font, Value::symbol("font-object")])
            .unwrap()
            .is_truthy()
    );
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("family")])
            .unwrap()
            .as_str(),
        Some("Serif")
    );
}

#[test]
fn font_at_eval_reads_source_style_inline_face_keywords() {
    let mut eval = crate::emacs_core::Evaluator::new();
    crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);

    let buffer = eval
        .buffers
        .current_buffer_mut()
        .expect("current buffer for inline face font-at test");
    buffer.insert("a好b");
    let inline_face = Value::list(vec![
        Value::symbol(":family"),
        Value::string("JetBrains Mono"),
        Value::symbol(":height"),
        Value::Float(1.2, next_float_id()),
        Value::symbol(":weight"),
        Value::symbol("normal"),
    ]);
    let start = buffer.text.char_to_byte(0);
    let end = buffer.text.char_to_byte(3);
    buffer
        .text_props
        .put_property(start, end, "face", inline_face);

    let font = builtin_font_at_eval(&mut eval, vec![Value::Int(1)]).unwrap();
    assert!(
        builtin_fontp(vec![font, Value::symbol("font-object")])
            .unwrap()
            .is_truthy()
    );
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("family")])
            .unwrap()
            .as_str(),
        Some("JetBrains Mono")
    );
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("height")]).unwrap(),
        Value::Int(120)
    );
}

#[test]
fn font_at_eval_passes_inline_face_weight_and_family_to_display_host() {
    let mut eval = crate::emacs_core::Evaluator::new();
    crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);

    let captured = Rc::new(RefCell::new(None));
    eval.set_display_host(Box::new(CapturingFontAtDisplayHost {
        last_request: captured.clone(),
    }));

    let buffer = eval
        .buffers
        .current_buffer_mut()
        .expect("current buffer for captured font-at test");
    buffer.insert("ab");
    let inline_face = Value::list(vec![
        Value::symbol(":family"),
        Value::string("Noto Sans Mono"),
        Value::symbol(":height"),
        Value::Float(0.9, next_float_id()),
        Value::symbol(":weight"),
        Value::symbol("semi-bold"),
    ]);
    let start = buffer.text.char_to_byte(0);
    let end = buffer.text.char_to_byte(2);
    buffer
        .text_props
        .put_property(start, end, "face", inline_face);

    let _ = builtin_font_at_eval(&mut eval, vec![Value::Int(1)]).unwrap();

    let request = captured
        .borrow()
        .clone()
        .expect("display host should capture font-at request");
    assert_eq!(request.character, 'a');
    assert_eq!(request.face.family.as_deref(), Some("Noto Sans Mono"));
    assert_eq!(request.face.weight, Some(FontWeight::SEMI_BOLD));
    assert_eq!(
        request.face.height,
        Some(crate::face::FaceHeight::Absolute(90))
    );
}

#[test]
fn font_at_eval_prefers_backend_selected_font_match_when_available() {
    let mut eval = crate::emacs_core::Evaluator::new();
    crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    eval.set_display_host(Box::new(FontAtDisplayHost {
        matched: Some(ResolvedFontMatch {
            family: "Noto Sans Mono CJK SC".to_string(),
            foundry: None,
            weight: FontWeight::NORMAL,
            slant: FontSlant::Normal,
            width: FontWidth::Normal,
            postscript_name: Some("NotoSansMonoCJKsc-Regular".to_string()),
        }),
    }));

    let buffer = eval
        .buffers
        .current_buffer_mut()
        .expect("current buffer for backend font-at test");
    buffer.insert("a好b");
    let inline_face = Value::list(vec![
        Value::symbol(":family"),
        Value::string("JetBrains Mono"),
        Value::symbol(":weight"),
        Value::symbol("normal"),
    ]);
    let start = buffer.text.char_to_byte(1);
    let end = buffer.text.char_to_byte(2);
    buffer
        .text_props
        .put_property(start, end, "face", inline_face);

    let font = builtin_font_at_eval(&mut eval, vec![Value::Int(2)]).unwrap();
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("family")])
            .unwrap()
            .as_str(),
        Some("Noto Sans Mono CJK SC")
    );
}

// -----------------------------------------------------------------------
// Face builtins
// -----------------------------------------------------------------------

#[test]
fn internal_lisp_face_p_symbol_returns_face_vector() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);

    let result = builtin_internal_lisp_face_p(vec![Value::symbol("default")]).unwrap();
    let values = match result {
        Value::Vector(v) => with_heap(|h| h.get_vector(v).clone()),
        _ => panic!("expected vector"),
    };
    assert_eq!(values.len(), LISP_FACE_VECTOR_LEN);
    assert_eq!(values[0].as_symbol_name(), Some("face"));
}

#[test]
fn internal_lisp_face_p_non_symbol() {
    let result = builtin_internal_lisp_face_p(vec![Value::Int(42)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_p_nil_returns_nil() {
    let result = builtin_internal_lisp_face_p(vec![Value::Nil]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_p_rejects_non_nil_frame_designator() {
    let result = builtin_internal_lisp_face_p(vec![Value::symbol("default"), Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn internal_lisp_face_p_with_frame_designator_returns_resolved_vector() {
    let mut heap = crate::gc::heap::LispHeap::new();
    crate::emacs_core::value::set_current_heap(&mut heap);
    clear_font_cache_state();

    let result =
        builtin_internal_lisp_face_p(vec![Value::symbol("default"), Value::Frame(FRAME_ID_BASE)])
            .unwrap();
    let values = match result {
        Value::Vector(v) => with_heap(|h| h.get_vector(v).clone()),
        _ => panic!("expected vector"),
    };
    assert_eq!(values[0].as_symbol_name(), Some("face"));
    assert_eq!(values[1].as_str(), Some("default"));
    assert_eq!(values[2].as_str(), Some("default"));
    assert_eq!(values[3].as_symbol_name(), Some("normal"));
    assert_eq!(values[4].as_int(), Some(1));
    assert_eq!(values[5].as_symbol_name(), Some("normal"));
    assert_eq!(values[8].as_symbol_name(), Some("nil"));
    assert!(values[9].as_str().is_some());
    assert!(values[10].as_str().is_some());
}

#[test]
fn internal_make_lisp_face_creates_symbol_visible_to_internal_lisp_face_p() {
    let name = Value::symbol("__neovm_make_face_unit_test");
    let made = builtin_internal_make_lisp_face(vec![name]).unwrap();
    assert!(matches!(made, Value::Vector(_)));
    let exists = builtin_internal_lisp_face_p(vec![name]).unwrap();
    assert!(matches!(exists, Value::Vector(_)));
}

#[test]
fn internal_make_lisp_face_rejects_non_symbol_and_non_nil_frame() {
    assert!(builtin_internal_make_lisp_face(vec![Value::string("foo")]).is_err());
    assert!(builtin_internal_make_lisp_face(vec![Value::symbol("foo"), Value::Int(1)]).is_err());
}

#[test]
fn internal_copy_lisp_face_returns_to_when_frame_t() {
    let result = builtin_internal_copy_lisp_face(vec![
        Value::symbol("bold"),
        Value::symbol("my-face"),
        Value::True,
        Value::Nil,
    ])
    .unwrap();
    assert_eq!(result.as_symbol_name(), Some("my-face"));
}

#[test]
fn internal_copy_lisp_face_eval_updates_face_table() {
    let mut eval = crate::emacs_core::Evaluator::new();
    builtin_internal_set_lisp_face_attribute_eval(
        &mut eval,
        vec![
            Value::symbol("bold"),
            Value::Keyword(intern("family")),
            Value::string("Serif"),
        ],
    )
    .unwrap();

    let copied = builtin_internal_copy_lisp_face_eval(
        &mut eval,
        vec![
            Value::symbol("bold"),
            Value::symbol("copied-face"),
            Value::True,
            Value::Nil,
        ],
    )
    .unwrap();
    assert_eq!(copied.as_symbol_name(), Some("copied-face"));
    assert_eq!(
        eval.face_table().resolve("copied-face").family.as_deref(),
        Some("Serif")
    );
}

#[test]
fn internal_copy_lisp_face_rejects_non_t_frame_designator() {
    let result = builtin_internal_copy_lisp_face(vec![
        Value::symbol("default"),
        Value::symbol("my-face"),
        Value::Nil,
        Value::Nil,
    ]);
    assert!(result.is_err());
}

#[test]
fn internal_copy_lisp_face_validates_new_frame_when_frame_designator_used() {
    let frame = Value::Frame(FRAME_ID_BASE);
    let err_t = builtin_internal_copy_lisp_face(vec![
        Value::symbol("default"),
        Value::symbol("my-face"),
        frame,
        Value::True,
    ]);
    assert!(err_t.is_err());

    let err_small_int = builtin_internal_copy_lisp_face(vec![
        Value::symbol("default"),
        Value::symbol("my-face"),
        frame,
        Value::Int(1),
    ]);
    assert!(err_small_int.is_err());

    let ok = builtin_internal_copy_lisp_face(vec![
        Value::symbol("default"),
        Value::symbol("my-face"),
        frame,
        frame,
    ])
    .unwrap();
    assert_eq!(ok.as_symbol_name(), Some("my-face"));
}

#[test]
fn internal_copy_lisp_face_uses_symbol_checks_before_frame_checks() {
    let result = builtin_internal_copy_lisp_face(vec![
        Value::Int(1),
        Value::symbol("my-face"),
        Value::Nil,
        Value::Nil,
    ]);
    assert!(result.is_err());
}

#[test]
fn internal_set_lisp_face_attribute_returns_value() {
    let face = Value::symbol("__neovm_set_attr_unit_test");
    let result = builtin_internal_set_lisp_face_attribute(vec![
        face,
        Value::Keyword(intern("foreground")),
        Value::string("white"),
    ])
    .unwrap();
    assert_eq!(result, face);
}

#[test]
fn internal_get_lisp_face_attribute_default_foreground() {
    let result = builtin_internal_get_lisp_face_attribute(vec![
        Value::symbol("default"),
        Value::Keyword(intern(":foreground")),
    ])
    .unwrap();
    assert_eq!(result.as_str(), Some("unspecified-fg"));
}

#[test]
fn internal_get_lisp_face_attribute_mode_line_returns_unspecified() {
    let result = builtin_internal_get_lisp_face_attribute(vec![
        Value::symbol("mode-line"),
        Value::Keyword(intern(":foreground")),
    ])
    .unwrap();
    assert_eq!(result.as_symbol_name(), Some("unspecified"));
}

#[test]
fn internal_get_lisp_face_attribute_defaults_frame_returns_unspecified() {
    let result = builtin_internal_get_lisp_face_attribute(vec![
        Value::symbol("default"),
        Value::Keyword(intern(":foreground")),
        Value::True,
    ])
    .unwrap();
    assert_eq!(result.as_symbol_name(), Some("unspecified"));
}

#[test]
fn internal_get_lisp_face_attribute_invalid_face_errors() {
    let result = builtin_internal_get_lisp_face_attribute(vec![
        Value::symbol("unknown-face"),
        Value::Keyword(intern(":foreground")),
    ]);
    assert!(result.is_err());
}

#[test]
fn internal_get_lisp_face_attribute_invalid_attr_errors() {
    let wrong_type =
        builtin_internal_get_lisp_face_attribute(vec![Value::symbol("default"), Value::Int(1)]);
    assert!(wrong_type.is_err());

    let invalid_name = builtin_internal_get_lisp_face_attribute(vec![
        Value::symbol("default"),
        Value::symbol("bogus"),
    ]);
    assert!(invalid_name.is_err());
}

#[test]
fn internal_lisp_face_attribute_values_discrete_boolean_attrs() {
    let result =
        builtin_internal_lisp_face_attribute_values(vec![Value::Keyword(intern(":underline"))])
            .unwrap();
    let vals = list_to_vec(&result).expect("list");
    assert_eq!(vals, vec![Value::True, Value::Nil]);
}

#[test]
fn internal_lisp_face_attribute_values_non_discrete_attr_is_nil() {
    let result =
        builtin_internal_lisp_face_attribute_values(vec![Value::Keyword(intern(":weight"))])
            .unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_attribute_values_rejects_non_symbol() {
    let result = builtin_internal_lisp_face_attribute_values(vec![Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn internal_lisp_face_empty_p_selected_frame_default_is_not_empty() {
    let result = builtin_internal_lisp_face_empty_p(vec![Value::symbol("default")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_empty_p_accepts_string_face_name() {
    let result = builtin_internal_lisp_face_empty_p(vec![Value::string("default")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_empty_p_defaults_frame_is_empty() {
    let result =
        builtin_internal_lisp_face_empty_p(vec![Value::symbol("default"), Value::True]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn internal_lisp_face_empty_p_rejects_non_nil_non_t_frame_designator() {
    let result = builtin_internal_lisp_face_empty_p(vec![Value::symbol("default"), Value::Int(1)]);
    assert!(result.is_err());
    let frame_result =
        builtin_internal_lisp_face_empty_p(vec![Value::symbol("default"), Value::Frame(1)]);
    assert!(frame_result.is_err());
}

#[test]
fn internal_lisp_face_comparators_accept_frame_handles() {
    let frame = Value::Frame(FRAME_ID_BASE);
    let empty_result =
        builtin_internal_lisp_face_empty_p(vec![Value::symbol("default"), frame]).unwrap();
    assert!(empty_result.is_nil());

    let equal_result = builtin_internal_lisp_face_equal_p(vec![
        Value::symbol("default"),
        Value::symbol("mode-line"),
        frame,
    ])
    .unwrap();
    assert!(equal_result.is_nil());
}

#[test]
fn internal_lisp_face_equal_p_selected_frame_distinguishes_faces() {
    let result = builtin_internal_lisp_face_equal_p(vec![
        Value::symbol("default"),
        Value::symbol("mode-line"),
    ])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_equal_p_defaults_frame_treats_faces_as_equal() {
    let result = builtin_internal_lisp_face_equal_p(vec![
        Value::symbol("default"),
        Value::symbol("mode-line"),
        Value::True,
    ])
    .unwrap();
    assert!(result.is_truthy());
}

#[test]
fn internal_lisp_face_equal_p_accepts_string_face_names() {
    let result = builtin_internal_lisp_face_equal_p(vec![
        Value::string("default"),
        Value::string("default"),
    ])
    .unwrap();
    assert!(result.is_truthy());
}

#[test]
fn internal_merge_in_global_face_rejects_non_frame_designator() {
    let result = builtin_internal_merge_in_global_face(vec![Value::symbol("default"), Value::Nil]);
    assert!(result.is_err());
    let frame_handle_result =
        builtin_internal_merge_in_global_face(vec![Value::symbol("default"), Value::Frame(1)]);
    assert!(frame_handle_result.is_err());
}

#[test]
fn internal_merge_in_global_face_copies_defaults_into_selected_face() {
    let face = Value::symbol("__neovm_merge_face_unit_test");
    let _ = builtin_internal_make_lisp_face(vec![face]).unwrap();
    let _ = builtin_internal_set_lisp_face_attribute(vec![
        face,
        Value::Keyword(intern("foreground")),
        Value::string("white"),
        Value::True,
    ])
    .unwrap();
    let merged =
        builtin_internal_merge_in_global_face(vec![face, Value::Frame(FRAME_ID_BASE)]).unwrap();
    assert!(merged.is_nil());
    let got =
        builtin_internal_get_lisp_face_attribute(vec![face, Value::Keyword(intern(":foreground"))])
            .unwrap();
    assert_eq!(got.as_str(), Some("white"));
}

#[test]
fn internal_lisp_face_helpers_accept_frame_handles() {
    let frame = Value::Frame(FRAME_ID_BASE);

    let descriptor = builtin_internal_lisp_face_p(vec![Value::symbol("default"), frame]).unwrap();
    assert!(descriptor.is_vector());

    let face = Value::symbol("__neovm_face_frame_handle_unit_test");
    let made = builtin_internal_make_lisp_face(vec![face, frame]).unwrap();
    assert!(made.is_vector());

    let copied =
        builtin_internal_copy_lisp_face(vec![Value::symbol("default"), face, frame, Value::Nil])
            .unwrap();
    assert_eq!(copied, face);

    let set = builtin_internal_set_lisp_face_attribute(vec![
        Value::symbol("default"),
        Value::Keyword(intern("foreground")),
        Value::string("red"),
        frame,
    ])
    .unwrap();
    assert_eq!(set, Value::symbol("default"));

    let got = builtin_internal_get_lisp_face_attribute(vec![
        Value::symbol("default"),
        Value::Keyword(intern(":foreground")),
        frame,
    ])
    .unwrap();
    assert_eq!(got.as_str(), Some("red"));
}

#[test]
fn face_attribute_relative_p_height_non_fixnum_is_relative() {
    let result =
        builtin_face_attribute_relative_p(vec![Value::Keyword(intern("height")), Value::Nil])
            .unwrap();
    assert!(result.is_truthy());
}

#[test]
fn face_attribute_relative_p_height_fixnum_is_not_relative() {
    let result =
        builtin_face_attribute_relative_p(vec![Value::Keyword(intern("height")), Value::Int(1)])
            .unwrap();
    assert!(result.is_nil());
}

#[test]
fn face_attribute_relative_p_non_height_attribute_is_nil() {
    let result = builtin_face_attribute_relative_p(vec![
        Value::Keyword(intern("weight")),
        Value::symbol("foo"),
    ])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn face_attribute_relative_p_unspecified_is_relative() {
    let result = builtin_face_attribute_relative_p(vec![
        Value::Keyword(intern("weight")),
        Value::symbol("unspecified"),
    ])
    .unwrap();
    assert!(result.is_truthy());
}

#[test]
fn merge_face_attribute_non_unspecified() {
    let result = builtin_merge_face_attribute(vec![
        Value::Keyword(intern("foreground")),
        Value::string("red"),
        Value::string("blue"),
    ])
    .unwrap();
    assert_eq!(result.as_str(), Some("red"));
}

#[test]
fn merge_face_attribute_unspecified() {
    let result = builtin_merge_face_attribute(vec![
        Value::Keyword(intern("foreground")),
        Value::symbol("unspecified"),
        Value::string("blue"),
    ])
    .unwrap();
    assert_eq!(result.as_str(), Some("blue"));
}

#[test]
fn merge_face_attribute_height_relative_over_absolute() {
    let result = builtin_merge_face_attribute(vec![
        Value::Keyword(intern("height")),
        Value::Float(1.5, next_float_id()),
        Value::Int(120),
    ])
    .unwrap();
    assert_eq!(result, Value::Int(180));
}

#[test]
fn merge_face_attribute_height_relative_over_relative() {
    let result = builtin_merge_face_attribute(vec![
        Value::Keyword(intern("height")),
        Value::Float(1.5, next_float_id()),
        Value::Float(1.2, next_float_id()),
    ])
    .unwrap();
    match result {
        Value::Float(value, _) => assert!((value - 1.8).abs() < 1e-9),
        other => panic!("expected float result, got {other:?}"),
    }
}

#[test]
fn face_list_returns_known_faces() {
    let result = builtin_face_list(vec![]).unwrap();
    let faces = list_to_vec(&result).unwrap();
    let names: Vec<&str> = faces.iter().filter_map(|v| v.as_symbol_name()).collect();
    assert!(names.contains(&"default"));
    assert!(names.contains(&"bold"));
    assert!(names.contains(&"cursor"));
    assert!(names.contains(&"mode-line"));
    assert!(names.contains(&"tool-bar"));
    assert!(names.contains(&"tab-bar"));
    assert!(names.contains(&"tab-line"));
}

#[test]
fn color_defined_p_known_and_unknown() {
    let result = builtin_color_defined_p(vec![Value::string("red")]).unwrap();
    assert!(result.is_truthy());

    let missing = builtin_color_defined_p(vec![Value::string("anything")]).unwrap();
    assert!(missing.is_nil());

    let invalid_hex = builtin_color_defined_p(vec![Value::string("#ggg")]).unwrap();
    assert!(invalid_hex.is_nil());

    let non_string = builtin_color_defined_p(vec![Value::Int(1)]).unwrap();
    assert!(non_string.is_nil());
}

#[test]
fn color_queries_validate_optional_device_arg() {
    assert!(builtin_color_defined_p(vec![Value::string("red"), Value::Int(1)]).is_err());
    assert!(builtin_color_values(vec![Value::string("red"), Value::Int(1)]).is_err());
    assert!(builtin_defined_colors(vec![Value::Int(1)]).is_err());
    assert!(builtin_color_defined_p(vec![Value::string("red"), Value::Frame(1)]).is_err());
    assert!(builtin_color_values(vec![Value::string("red"), Value::Frame(1)]).is_err());
    assert!(builtin_defined_colors(vec![Value::Frame(1)]).is_err());
    assert!(
        builtin_color_defined_p(vec![Value::string("red"), Value::Frame(FRAME_ID_BASE)]).is_ok()
    );
    assert!(builtin_color_values(vec![Value::string("red"), Value::Frame(FRAME_ID_BASE)]).is_ok());
    assert!(builtin_defined_colors(vec![Value::Frame(FRAME_ID_BASE)]).is_ok());
}

#[test]
fn color_values_named_black() {
    let result = builtin_color_values(vec![Value::string("black")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb.len(), 3);
    assert_eq!(rgb[0].as_int(), Some(0));
    assert_eq!(rgb[1].as_int(), Some(0));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_named_white() {
    let result = builtin_color_values(vec![Value::string("white")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(65535));
    assert_eq!(rgb[2].as_int(), Some(65535));
}

#[test]
fn color_values_hex_rrggbb() {
    // Hex colors are approximated to terminal palette colors in batch mode.
    let result = builtin_color_values(vec![Value::string("#FF0000")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(0));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_hex_short() {
    // #F00 resolves and approximates to red.
    let result = builtin_color_values(vec![Value::string("#F00")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(0));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_hex_12digit() {
    // 12-digit hex resolves and approximates to red.
    let result = builtin_color_values(vec![Value::string("#FFFF00000000")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(0));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_unknown_returns_nil() {
    let result = builtin_color_values(vec![Value::string("nonexistent-color")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn color_values_wrong_type_returns_nil() {
    let result = builtin_color_values(vec![Value::Int(42)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn defined_colors_returns_list() {
    let result = builtin_defined_colors(vec![]).unwrap();
    assert!(result.is_list());
    assert!(!result.is_nil());
    let colors = list_to_vec(&result).expect("defined-colors list");
    assert_eq!(colors.len(), 8);
    assert_eq!(colors[0].as_str(), Some("black"));
    assert_eq!(colors[7].as_str(), Some("white"));
}

#[test]
fn face_id_rejects_non_symbol_faces() {
    let result = builtin_face_id(vec![Value::symbol("default")]).unwrap();
    assert_eq!(result.as_int(), Some(0));
}

#[test]
fn face_id_known_faces_use_oracle_ids() {
    let bold = builtin_face_id(vec![Value::symbol("bold")]).unwrap();
    assert_eq!(bold.as_int(), Some(1));
    let mode_line = builtin_face_id(vec![Value::symbol("mode-line")]).unwrap();
    assert_eq!(mode_line.as_int(), Some(25));
}

#[test]
fn face_id_accepts_optional_frame_argument() {
    let result = builtin_face_id(vec![Value::symbol("default"), Value::Nil]).unwrap();
    assert_eq!(result.as_int(), Some(0));
}

#[test]
fn face_id_assigns_dynamic_id_for_created_faces() {
    let face = Value::symbol("__neovm_face_id_dynamic_unit_test");
    let _ = builtin_internal_make_lisp_face(vec![face]).unwrap();
    let first = builtin_face_id(vec![face]).unwrap();
    let second = builtin_face_id(vec![face]).unwrap();
    assert_eq!(first, second);
}

#[test]
fn face_id_rejects_invalid_face() {
    let result = builtin_face_id(vec![Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn face_font_returns_nil_for_known_faces() {
    let result = builtin_face_font(vec![Value::symbol("default")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn face_font_accepts_known_string_face() {
    let result = builtin_face_font(vec![Value::string("default")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn face_font_ignores_optional_arguments_for_known_face() {
    let result =
        builtin_face_font(vec![Value::symbol("default"), Value::Nil, Value::True]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn face_font_rejects_invalid_face() {
    let result = builtin_face_font(vec![Value::Int(1)]);
    assert!(result.is_err());
}

#[test]
fn internal_face_x_get_resource_returns_nil_for_string_args() {
    let result = builtin_internal_face_x_get_resource(vec![
        Value::string("font"),
        Value::string("Font"),
        Value::Nil,
    ])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_face_x_get_resource_validates_string_args_and_arity() {
    assert!(builtin_internal_face_x_get_resource(vec![]).is_err());
    assert!(builtin_internal_face_x_get_resource(vec![Value::Nil]).is_err());
    assert!(builtin_internal_face_x_get_resource(vec![Value::Nil, Value::string("Font")]).is_err());
    assert!(builtin_internal_face_x_get_resource(vec![Value::string("font"), Value::Nil]).is_err());
}

#[test]
fn internal_set_font_selection_order_accepts_valid_order() {
    let result = builtin_internal_set_font_selection_order(vec![Value::list(vec![
        Value::Keyword(intern(":width")),
        Value::Keyword(intern(":height")),
        Value::Keyword(intern(":weight")),
        Value::Keyword(intern(":slant")),
    ])])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_set_font_selection_order_rejects_invalid_order() {
    let result =
        builtin_internal_set_font_selection_order(vec![Value::list(vec![Value::symbol("x")])]);
    assert!(result.is_err());
}

#[test]
fn internal_set_alternative_font_family_alist_returns_converted_list() {
    let result = builtin_internal_set_alternative_font_family_alist(vec![Value::Nil]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_set_alternative_font_family_alist_converts_strings_to_symbols() {
    let input = Value::list(vec![Value::list(vec![
        Value::string("Foo"),
        Value::string("Bar"),
    ])]);
    let result = builtin_internal_set_alternative_font_family_alist(vec![input]).unwrap();
    let outer = list_to_vec(&result).expect("outer list");
    let inner = list_to_vec(&outer[0]).expect("inner list");
    assert_eq!(inner[0].as_symbol_name(), Some("Foo"));
    assert_eq!(inner[1].as_symbol_name(), Some("Bar"));
}

#[test]
fn internal_set_alternative_font_registry_alist_returns_nil_or_value() {
    let result = builtin_internal_set_alternative_font_registry_alist(vec![Value::Nil]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_set_alternative_font_registry_alist_preserves_values() {
    let input = Value::list(vec![Value::list(vec![Value::Int(1), Value::Int(2)])]);
    let result = builtin_internal_set_alternative_font_registry_alist(vec![input]).unwrap();
    assert_eq!(result, input);
}

// -----------------------------------------------------------------------
// Arity checks
// -----------------------------------------------------------------------

#[test]
fn fontp_too_many_args() {
    let result = builtin_fontp(vec![Value::Nil, Value::Nil, Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn fontp_no_args() {
    let result = builtin_fontp(vec![]);
    assert!(result.is_err());
}

#[test]
fn font_get_wrong_arity() {
    assert!(builtin_font_get(vec![Value::Nil]).is_err());
    assert!(builtin_font_get(vec![Value::Nil, Value::Nil, Value::Nil]).is_err());
}

#[test]
fn font_put_wrong_arity() {
    assert!(builtin_font_put(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn face_attribute_relative_p_wrong_arity() {
    assert!(builtin_face_attribute_relative_p(vec![Value::Nil]).is_err());
}

#[test]
fn merge_face_attribute_wrong_arity() {
    assert!(builtin_merge_face_attribute(vec![Value::Nil, Value::Nil]).is_err());
}

#[test]
fn color_values_case_insensitive() {
    let result = builtin_color_values(vec![Value::string("RED")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(0));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_hex_lowercase() {
    let result = builtin_color_values(vec![Value::string("#ff8000")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    // #ff8000 approximates to yellow in the terminal palette.
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(65535));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_invalid_hex_returns_nil() {
    let result = builtin_color_values(vec![Value::string("#ggg")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn color_values_from_color_spec_semantics() {
    let rgb_short =
        list_to_vec(&builtin_color_values_from_color_spec(vec![Value::string("#000")]).unwrap())
            .unwrap();
    assert_eq!(rgb_short, vec![Value::Int(0), Value::Int(0), Value::Int(0)]);

    let rgb_12 = list_to_vec(
        &builtin_color_values_from_color_spec(vec![Value::string("#111122223333")]).unwrap(),
    )
    .unwrap();
    assert_eq!(
        rgb_12,
        vec![Value::Int(4369), Value::Int(8738), Value::Int(13107)]
    );

    assert!(
        builtin_color_values_from_color_spec(vec![Value::string("#abcd")])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_color_values_from_color_spec(vec![Value::string("bogus")])
            .unwrap()
            .is_nil()
    );

    let type_err = builtin_color_values_from_color_spec(vec![Value::Int(1)])
        .expect_err("color-values-from-color-spec should enforce stringp");
    match type_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn color_gray_and_supported_semantics() {
    assert!(
        builtin_color_gray_p(vec![Value::string("#000000")])
            .unwrap()
            .is_truthy()
    );
    assert!(
        builtin_color_gray_p(vec![Value::string("#808080")])
            .unwrap()
            .is_truthy()
    );
    assert!(
        builtin_color_gray_p(vec![Value::string("#ff0000")])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_color_gray_p(vec![Value::string("#fff"), Value::Nil])
            .unwrap()
            .is_truthy()
    );

    let gray_color_type =
        builtin_color_gray_p(vec![Value::Int(1)]).expect_err("color-gray-p should enforce stringp");
    match gray_color_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let gray_frame_type = builtin_color_gray_p(vec![Value::string("#fff"), Value::Int(0)])
        .expect_err("color-gray-p should validate FRAME");
    match gray_frame_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("framep"), Value::Int(0)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    assert!(
        builtin_color_supported_p(vec![Value::string("#123456")])
            .unwrap()
            .is_truthy()
    );
    assert!(
        builtin_color_supported_p(vec![Value::string("#fff"), Value::Nil, Value::True])
            .unwrap()
            .is_truthy()
    );
    assert!(
        builtin_color_supported_p(vec![Value::string("bogus"), Value::Nil, Value::Nil])
            .unwrap()
            .is_nil()
    );

    let supported_type = builtin_color_supported_p(vec![Value::Int(1)])
        .expect_err("color-supported-p should enforce stringp");
    match supported_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let supported_frame_type =
        builtin_color_supported_p(vec![Value::string("#fff"), Value::Int(1)])
            .expect_err("color-supported-p should validate FRAME");
    match supported_frame_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("framep"), Value::Int(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn color_distance_semantics() {
    let black_white = builtin_color_distance(vec![Value::string("#000"), Value::string("#fff")])
        .expect("color-distance should evaluate");
    match black_white {
        Value::Int(n) => assert!(n > 0),
        other => panic!("expected integer distance, got {other:?}"),
    }

    assert_eq!(
        builtin_color_distance(vec![Value::string("#000"), Value::string("#000")]).unwrap(),
        Value::Int(0)
    );

    // Both colors collapse to black in tty-approx mode.
    assert_eq!(
        builtin_color_distance(vec![Value::string("#000"), Value::string("#111")]).unwrap(),
        Value::Int(0)
    );
}

#[test]
fn color_distance_errors_match_oracle_shape() {
    let invalid_left =
        builtin_color_distance(vec![Value::string("#00"), Value::string("#fff")]).unwrap_err();
    match invalid_left {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Invalid color"), Value::string("#00")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let invalid_type = builtin_color_distance(vec![Value::Int(1), Value::string("#fff")])
        .expect_err("color-distance should signal invalid color for non-string args");
    match invalid_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Invalid color"), Value::Int(1)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let frame_err = builtin_color_distance(vec![
        Value::string("#000"),
        Value::string("#fff"),
        Value::True,
    ])
    .expect_err("color-distance should validate optional FRAME");
    match frame_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::True]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn color_values_dark_gray_approximates_to_white() {
    let result = builtin_color_values(vec![Value::string("DarkGray")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(65535));
    assert_eq!(rgb[2].as_int(), Some(65535));
}
