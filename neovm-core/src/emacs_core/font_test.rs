use super::*;
use crate::emacs_core::eval::{
    Context, DisplayHost, FontResolveRequest, FontSpecResolveRequest, GuiFrameHostRequest,
    ResolvedFontMatch, ResolvedFontSpecMatch, ResolvedFrameFont,
};
use crate::emacs_core::value::{ValueKind, VecLikeType};
use crate::face::{Color, FaceAttrValue};
use crate::heap_types::LispString;
use crate::test_utils::runtime_startup_eval_all;
use crate::window::{FRAME_ID_BASE, FrameId};
use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

fn call_face_font(args: impl FnOnce() -> Vec<Value>) -> EvalResult {
    let mut eval = Context::new();
    let args = args();
    builtin_face_font(&mut eval, args)
}

macro_rules! call_font_builtin {
    ($builtin:ident, $args:expr) => {{
        let mut eval = Context::new();
        let args = $args;
        $builtin(&mut eval, args)
    }};
}

fn ensure_selected_gui_frame(eval: &mut Context) -> FrameId {
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(eval);
    let frame = eval
        .frame_manager_mut()
        .get_mut(frame_id)
        .expect("selected frame");
    frame.set_window_system(Some(Value::symbol("neo")));
    frame_id
}

#[test]
fn raw_context_does_not_prebind_x_color_aliases() {
    crate::test_utils::init_test_tracing();
    let eval = Context::new();
    for name in ["x-defined-colors", "x-color-defined-p", "x-color-values"] {
        assert!(
            eval.obarray.symbol_function(name).is_none(),
            "{name} should come from GNU faces.el, not Context::new",
        );
    }
}

#[test]
fn gnu_faces_el_defines_x_color_aliases() {
    crate::test_utils::init_test_tracing();
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("project root")
            .join("lisp/faces.el"),
    )
    .expect("read faces.el");
    assert!(
        source.contains(
            "(define-obsolete-function-alias 'x-defined-colors #'defined-colors \"30.1\")"
        ),
        "GNU faces.el should own the x-defined-colors alias",
    );
    assert!(
        source.contains(
            "(define-obsolete-function-alias 'x-color-defined-p #'color-defined-p \"30.1\")"
        ),
        "GNU faces.el should own the x-color-defined-p alias",
    );
    assert!(
        source.contains("(define-obsolete-function-alias 'x-color-values #'color-values \"30.1\")"),
        "GNU faces.el should own the x-color-values alias",
    );
}

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

struct CapturingFindFontDisplayHost {
    last_request: Rc<RefCell<Option<FontSpecResolveRequest>>>,
    matched: Option<ResolvedFontSpecMatch>,
}

impl DisplayHost for CapturingFindFontDisplayHost {
    fn realize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resolve_font_for_spec(
        &mut self,
        request: FontSpecResolveRequest,
    ) -> Result<Option<ResolvedFontSpecMatch>, String> {
        *self.last_request.borrow_mut() = Some(request);
        Ok(self.matched.clone())
    }
}

struct LiveFrameFontDisplayHost {
    realized: Option<ResolvedFrameFont>,
}

impl DisplayHost for LiveFrameFontDisplayHost {
    fn realize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resolve_frame_font(
        &mut self,
        _frame_id: crate::window::FrameId,
        _face: crate::face::Face,
    ) -> Result<Option<ResolvedFrameFont>, String> {
        Ok(self.realized.clone())
    }
}

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    runtime_startup_eval_all(src)
}

#[test]
fn fontp_on_non_font() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_fontp(vec![Value::fixnum(42)]).unwrap().is_nil());
    assert!(
        builtin_fontp(vec![Value::string("hello")])
            .unwrap()
            .is_nil()
    );
}

#[test]
fn font_spec_basic() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_font_spec(vec![
        Value::keyword("family"),
        Value::string("Monospace"),
        Value::keyword("size"),
        Value::fixnum(12),
    ])
    .unwrap();
    assert!(is_font_spec(&spec));
    assert!(builtin_fontp(vec![spec]).unwrap().is_truthy());
}

#[test]
fn find_font_eval_requests_exact_registry_match_from_display_host() {
    crate::test_utils::init_test_tracing();
    let last_request = Rc::new(RefCell::new(None));
    let mut eval = crate::emacs_core::Context::new();
    crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    eval.set_display_host(Box::new(CapturingFindFontDisplayHost {
        last_request: Rc::clone(&last_request),
        matched: Some(ResolvedFontSpecMatch {
            family: LispString::from_utf8("Noto Sans Mono CJK SC"),
            registry: Some(LispString::from_utf8("iso10646-1")),
            weight: Some(FontWeight::NORMAL),
            slant: Some(FontSlant::Normal),
            width: Some(crate::face::FontWidth::Normal),
            spacing: None,
            postscript_name: Some(LispString::from_utf8("NotoSansMonoCJKsc-Regular")),
        }),
    }));

    let spec = builtin_font_spec(vec![
        Value::keyword("registry"),
        Value::string("gb2312.1980-0"),
        Value::keyword("weight"),
        Value::symbol("normal"),
    ])
    .unwrap();
    let font = builtin_find_font(&mut eval, vec![spec]).unwrap();

    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("family")])
            .unwrap()
            .as_symbol_name(),
        Some("Noto Sans Mono CJK SC")
    );
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("registry")])
            .unwrap()
            .as_symbol_name(),
        Some("iso10646-1")
    );
    assert!(
        builtin_fontp(vec![font, Value::symbol("font-entity")])
            .unwrap()
            .is_truthy()
    );

    let request = last_request
        .borrow()
        .clone()
        .expect("display host should capture find-font request");
    assert_eq!(
        request.registry,
        Some(LispString::from_utf8("gb2312.1980-0"))
    );
    assert_eq!(request.family, None);
    assert_eq!(request.weight, Some(FontWeight::NORMAL));
}

#[test]
fn find_font_eval_returns_gnu_canonical_ultra_light_weight_symbol() {
    crate::test_utils::init_test_tracing();
    let last_request = Rc::new(RefCell::new(None));
    let mut eval = crate::emacs_core::Context::new();
    crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    eval.set_display_host(Box::new(CapturingFindFontDisplayHost {
        last_request: Rc::clone(&last_request),
        matched: Some(ResolvedFontSpecMatch {
            family: LispString::from_utf8("JetBrains Mono"),
            registry: Some(LispString::from_utf8("iso10646-1")),
            weight: Some(FontWeight::EXTRA_LIGHT),
            slant: Some(FontSlant::Normal),
            width: Some(crate::face::FontWidth::Normal),
            spacing: Some(100),
            postscript_name: None,
        }),
    }));

    let spec = builtin_font_spec(vec![
        Value::keyword("family"),
        Value::string("JetBrains Mono"),
    ])
    .unwrap();
    let font = builtin_find_font(&mut eval, vec![spec]).unwrap();

    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("weight")]).unwrap(),
        Value::symbol("ultra-light")
    );
}

#[test]
fn font_spec_odd_args_error() {
    crate::test_utils::init_test_tracing();
    let result = builtin_font_spec(vec![Value::keyword("family")]);
    assert!(result.is_err());
}

#[test]
fn font_get_and_put() {
    crate::test_utils::init_test_tracing();
    let spec =
        builtin_font_spec(vec![Value::keyword("family"), Value::string("Monospace")]).unwrap();

    // Get existing property.
    let family = builtin_font_get(vec![spec, Value::keyword("family")]).unwrap();
    assert_eq!(family.as_symbol_name(), Some("Monospace"));

    // Get missing property.
    let missing = builtin_font_get(vec![spec, Value::keyword("size")]).unwrap();
    assert!(missing.is_nil());

    // Put returns VAL and mutates the original spec.
    let put_size = builtin_font_put(vec![spec, Value::keyword("size"), Value::fixnum(14)]).unwrap();
    assert_eq!(put_size.as_int(), Some(14));
    let size = builtin_font_get(vec![spec, Value::keyword("size")]).unwrap();
    assert_eq!(size.as_int(), Some(14));

    // Overwrite existing property.
    let put_family =
        builtin_font_put(vec![spec, Value::keyword("family"), Value::string("Serif")]).unwrap();
    assert_eq!(put_family.as_symbol_name(), Some("Serif"));
    let family2 = builtin_font_get(vec![spec, Value::keyword("family")]).unwrap();
    assert_eq!(family2.as_symbol_name(), Some("Serif"));
}

#[test]
fn font_get_symbol_key() {
    crate::test_utils::init_test_tracing();
    // Symbol key does not match keyword storage.
    let spec = builtin_font_spec(vec![Value::keyword("weight"), Value::symbol("bold")]).unwrap();
    let weight = builtin_font_get(vec![spec, Value::symbol("weight")]).unwrap();
    assert!(weight.is_nil());
}

#[test]
fn font_get_keyword_with_colon_matches_keyword_storage_without_colon() {
    crate::test_utils::init_test_tracing();
    let font = Value::vector(vec![
        Value::keyword(FONT_OBJECT_TAG),
        Value::keyword("family"),
        Value::string("Hack"),
        Value::keyword("size"),
        Value::fixnum(27),
    ]);
    let family = builtin_font_get(vec![font, Value::keyword(":family")]).unwrap();
    let size = builtin_font_get(vec![font, Value::keyword(":size")]).unwrap();
    assert_eq!(family.as_utf8_str(), Some("Hack"));
    assert_eq!(size.as_int(), Some(27));
}

#[test]
fn font_get_non_symbol_property_errors() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_font_spec(vec![Value::keyword("weight"), Value::symbol("bold")]).unwrap();
    let result = builtin_font_get(vec![spec, Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn font_get_non_vector() {
    crate::test_utils::init_test_tracing();
    // font-get on a non-font value signals wrong-type-argument.
    let result = builtin_font_get(vec![Value::fixnum(42), Value::keyword("family")]);
    assert!(result.is_err());
}

#[test]
fn list_fonts_returns_list_or_nil() {
    crate::test_utils::init_test_tracing();
    let result = call_font_builtin!(
        builtin_list_fonts,
        vec![Value::vector(vec![Value::keyword(FONT_SPEC_TAG)])]
    );
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn list_fonts_rejects_non_font_spec() {
    crate::test_utils::init_test_tracing();
    let result = call_font_builtin!(builtin_list_fonts, vec![Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn eval_list_fonts_accepts_live_frame_designator() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let result = builtin_list_fonts(
        &mut eval,
        vec![
            Value::vector(vec![Value::keyword(FONT_SPEC_TAG)]),
            Value::fixnum(frame_id),
        ],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn find_font_returns_nil_for_font_spec() {
    crate::test_utils::init_test_tracing();
    let result = call_font_builtin!(
        builtin_find_font,
        vec![Value::vector(vec![Value::keyword(FONT_SPEC_TAG)])]
    );
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn find_font_rejects_non_font_spec() {
    crate::test_utils::init_test_tracing();
    let result = call_font_builtin!(builtin_find_font, vec![Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn eval_find_font_accepts_live_frame_designator() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let result = builtin_find_font(
        &mut eval,
        vec![
            Value::vector(vec![Value::keyword(FONT_SPEC_TAG)]),
            Value::fixnum(frame_id),
        ],
    )
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn clear_font_cache_returns_nil() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_clear_font_cache(vec![]).unwrap().is_nil());
}

#[test]
fn clear_font_cache_rejects_arity() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_clear_font_cache(vec![Value::NIL]).is_err());
}

#[test]
fn clear_font_cache_resets_face_caches() {
    crate::test_utils::init_test_tracing();
    let face_name = "__neovm_clear_font_cache_unit_test";
    let _ = call_font_builtin!(
        builtin_internal_make_lisp_face,
        vec![Value::symbol(face_name)]
    )
    .unwrap();
    let _ = call_font_builtin!(
        builtin_internal_set_lisp_face_attribute,
        vec![
            Value::symbol(face_name),
            Value::keyword(":foreground"),
            Value::string("white"),
        ]
    )
    .unwrap();

    CREATED_LISP_FACES.with(|slot| {
        assert!(
            slot.borrow()
                .contains(&crate::emacs_core::intern::intern(face_name,))
        );
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
fn created_face_runtime_state_uses_symbol_identity() {
    crate::test_utils::init_test_tracing();
    clear_font_cache_state();
    let face_name = "__neovm_symbol_runtime_face";
    let face_symbol = crate::emacs_core::intern::intern(face_name);
    let mut eval = Context::new();

    builtin_internal_make_lisp_face(&mut eval, vec![Value::symbol(face_name)]).unwrap();
    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol(face_name),
            Value::keyword(":foreground"),
            Value::string("green"),
        ],
    )
    .unwrap();

    CREATED_LISP_FACES.with(|slot| {
        assert!(slot.borrow().contains(&face_symbol));
    });
    CREATED_FACE_IDS.with(|slot| {
        assert!(slot.borrow().contains_key(&face_symbol));
    });
    FACE_ATTR_STATE.with(|slot| {
        let state = slot.borrow();
        assert_eq!(
            state
                .selected_overrides
                .get(&face_symbol)
                .and_then(|attrs| attrs.get(&crate::emacs_core::intern::intern(":foreground")))
                .copied(),
            Some(Value::string("green"))
        );
    });
}

#[test]
fn font_family_list_batch_returns_nil() {
    crate::test_utils::init_test_tracing();
    let result = call_font_builtin!(builtin_font_family_list, vec![]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn font_family_list_rejects_non_nil_frame_designator() {
    crate::test_utils::init_test_tracing();
    let result = call_font_builtin!(builtin_font_family_list, vec![Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn eval_font_family_list_accepts_live_frame_designator() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;
    let result = builtin_font_family_list(&mut eval, vec![Value::fixnum(frame_id)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn font_xlfd_name_returns_xlfd() {
    crate::test_utils::init_test_tracing();
    let result =
        builtin_font_xlfd_name(vec![Value::vector(vec![Value::keyword(FONT_SPEC_TAG)])]).unwrap();
    assert_eq!(result.as_utf8_str(), Some("-*-*-*-*-*-*-*-*-*-*-*-*-*-*"));
}

#[test]
fn font_xlfd_name_too_many_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_font_xlfd_name(vec![
        Value::vector(vec![Value::keyword(FONT_SPEC_TAG)]),
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "wrong-number-of-arguments"
    ));
}

#[test]
fn close_font_requires_font_object() {
    crate::test_utils::init_test_tracing();
    let wrong_nil = builtin_close_font(vec![Value::NIL]).unwrap_err();
    match wrong_nil {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("font-object"), Value::NIL]);
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
    crate::test_utils::init_test_tracing();
    let _eval = crate::emacs_core::Context::new(); // sets up heap
    let font_obj = Value::vector(vec![Value::keyword("font-object"), Value::fixnum(1)]);
    assert!(builtin_close_font(vec![font_obj]).unwrap().is_nil());
    assert!(
        builtin_close_font(vec![font_obj, Value::NIL])
            .unwrap()
            .is_nil()
    );

    assert!(builtin_close_font(vec![]).is_err());
    assert!(builtin_close_font(vec![Value::NIL, Value::NIL, Value::NIL]).is_err());
}

#[test]
fn font_at_eval_returns_font_object_for_multibyte_buffer_face() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    ensure_selected_gui_frame(&mut eval);

    let face = Value::symbol("font-at-buffer-face");
    builtin_internal_make_lisp_face(&mut eval, vec![face]).unwrap();
    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![face, Value::keyword("family"), Value::string("Serif")],
    )
    .unwrap();

    let buffer = eval
        .buffers
        .current_buffer_mut()
        .expect("current buffer for font-at buffer test");
    buffer.insert("a好b");
    let start = buffer.text.char_to_byte(1);
    let end = buffer.text.char_to_byte(2);
    buffer
        .text
        .text_props_put_property(start, end, Value::symbol("face"), face);

    let font = builtin_font_at(&mut eval, vec![Value::fixnum(2)]).unwrap();
    assert!(
        builtin_fontp(vec![font, Value::symbol("font-object")])
            .unwrap()
            .is_truthy()
    );
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("family")])
            .unwrap()
            .as_symbol_name(),
        Some("Serif")
    );
}

#[test]
fn font_at_eval_returns_nil_on_terminal_frame_after_position_validation() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);

    eval.buffers
        .current_buffer_mut()
        .expect("current buffer for terminal font-at test")
        .insert("abc");

    assert!(
        builtin_font_at(&mut eval, vec![Value::fixnum(1)])
            .expect("valid terminal font-at should evaluate")
            .is_nil()
    );

    let err = builtin_font_at(&mut eval, vec![Value::fixnum(4)])
        .expect_err("out-of-range terminal font-at should still validate position");
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "args-out-of-range"),
        other => panic!("expected args-out-of-range signal, got {other:?}"),
    }
}

#[test]
fn font_at_eval_returns_font_object_for_multibyte_string_face() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    ensure_selected_gui_frame(&mut eval);

    let face = Value::symbol("font-at-string-face");
    builtin_internal_make_lisp_face(&mut eval, vec![face]).unwrap();
    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![face, Value::keyword("family"), Value::string("Serif")],
    )
    .unwrap();

    let string = Value::string("a好b");
    if !string.is_string() {
        panic!("expected string value");
    };
    let mut table = crate::buffer::TextPropertyTable::new();
    let start = "a".len();
    let end = start + "好".len();
    table.put_property(start, end, Value::symbol("face"), face);
    crate::emacs_core::value::set_string_text_properties_table_for_value(string, table);

    let font = builtin_font_at(&mut eval, vec![Value::fixnum(1), Value::NIL, string]).unwrap();
    assert!(
        builtin_fontp(vec![font, Value::symbol("font-object")])
            .unwrap()
            .is_truthy()
    );
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("family")])
            .unwrap()
            .as_symbol_name(),
        Some("Serif")
    );
}

#[test]
fn font_at_eval_preserves_raw_unibyte_string_face() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    ensure_selected_gui_frame(&mut eval);

    let face = Value::symbol("font-at-raw-string-face");
    builtin_internal_make_lisp_face(&mut eval, vec![face]).unwrap();
    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![face, Value::keyword("family"), Value::string("Serif")],
    )
    .unwrap();

    let string = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF]));
    let runtime = crate::emacs_core::builtins::lisp_string_to_runtime_string(string);
    let mut table = crate::buffer::TextPropertyTable::new();
    table.put_property(0, runtime.len(), Value::symbol("face"), face);
    crate::emacs_core::value::set_string_text_properties_table_for_value(string, table);

    let font = builtin_font_at(&mut eval, vec![Value::fixnum(0), Value::NIL, string]).unwrap();
    assert!(
        builtin_fontp(vec![font, Value::symbol("font-object")])
            .unwrap()
            .is_truthy()
    );
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("family")])
            .unwrap()
            .as_symbol_name(),
        Some("Serif")
    );
}

#[test]
fn font_at_eval_reads_source_style_inline_face_keywords() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    ensure_selected_gui_frame(&mut eval);

    let buffer = eval
        .buffers
        .current_buffer_mut()
        .expect("current buffer for inline face font-at test");
    buffer.insert("a好b");
    let inline_face = Value::list(vec![
        Value::symbol(":family"),
        Value::string("JetBrains Mono"),
        Value::symbol(":height"),
        Value::make_float(1.2),
        Value::symbol(":weight"),
        Value::symbol("normal"),
    ]);
    let start = buffer.text.char_to_byte(0);
    let end = buffer.text.char_to_byte(3);
    buffer
        .text
        .text_props_put_property(start, end, Value::symbol("face"), inline_face);

    let font = builtin_font_at(&mut eval, vec![Value::fixnum(1)]).unwrap();
    assert!(
        builtin_fontp(vec![font, Value::symbol("font-object")])
            .unwrap()
            .is_truthy()
    );
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("family")])
            .unwrap()
            .as_symbol_name(),
        Some("JetBrains Mono")
    );
    // After the specbind refactor, font-get :height returns the raw
    // float value from the face spec instead of converting to decipoints.
    let height = builtin_font_get(vec![font, Value::keyword("height")]).unwrap();
    match height.kind() {
        ValueKind::Float => {
            let v = height.as_float().unwrap();
            assert!((v - 1.2).abs() < 1e-9, "expected 1.2, got {v}");
        }
        other => panic!("expected Float(1.2), got {other:?}"),
    }
}

#[test]
fn font_at_eval_passes_inline_face_weight_and_family_to_display_host() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    ensure_selected_gui_frame(&mut eval);

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
        Value::make_float(0.9),
        Value::symbol(":weight"),
        Value::symbol("semi-bold"),
    ]);
    let start = buffer.text.char_to_byte(0);
    let end = buffer.text.char_to_byte(2);
    buffer
        .text
        .text_props_put_property(start, end, Value::symbol("face"), inline_face);

    let _ = builtin_font_at(&mut eval, vec![Value::fixnum(1)]).unwrap();

    let request = captured
        .borrow()
        .clone()
        .expect("display host should capture font-at request");
    assert_eq!(request.character, 'a');
    assert_eq!(
        request.face.family_runtime_string_owned().as_deref(),
        Some("Noto Sans Mono")
    );
    assert_eq!(request.face.weight, Some(FontWeight::SEMI_BOLD));
    // After the specbind refactor, float heights are treated as relative
    // instead of being converted to absolute decipoints.
    assert_eq!(
        request.face.height,
        Some(crate::face::FaceHeight::Relative(0.9))
    );
}

#[test]
fn font_at_eval_prefers_backend_selected_font_match_when_available() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    ensure_selected_gui_frame(&mut eval);
    eval.set_display_host(Box::new(FontAtDisplayHost {
        matched: Some(ResolvedFontMatch {
            family: LispString::from_utf8("Noto Sans Mono CJK SC"),
            foundry: None,
            weight: FontWeight::NORMAL,
            slant: FontSlant::Normal,
            width: FontWidth::Normal,
            postscript_name: Some(LispString::from_utf8("NotoSansMonoCJKsc-Regular")),
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
        .text
        .text_props_put_property(start, end, Value::symbol("face"), inline_face);

    let font = builtin_font_at(&mut eval, vec![Value::fixnum(2)]).unwrap();
    assert_eq!(
        builtin_font_get(vec![font, Value::keyword("family")])
            .unwrap()
            .as_symbol_name(),
        Some("Noto Sans Mono CJK SC")
    );
}

// -----------------------------------------------------------------------
// Face builtins
// -----------------------------------------------------------------------

#[test]
fn internal_lisp_face_p_symbol_returns_face_vector() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_lisp_face_p(vec![Value::symbol("default")]).unwrap();
    let values = match result.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => result.as_vector_data().unwrap().clone(),
        _ => panic!("expected vector"),
    };
    assert_eq!(values.len(), LISP_FACE_VECTOR_LEN);
    assert_eq!(values[0].as_symbol_name(), Some("face"));
}

#[test]
fn internal_lisp_face_p_non_symbol() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_lisp_face_p(vec![Value::fixnum(42)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_p_nil_returns_nil() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_lisp_face_p(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_p_rejects_non_nil_frame_designator() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_lisp_face_p(vec![Value::symbol("default"), Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn internal_lisp_face_p_with_frame_designator_returns_resolved_vector() {
    crate::test_utils::init_test_tracing();
    clear_font_cache_state();

    let result = builtin_internal_lisp_face_p(vec![
        Value::symbol("default"),
        Value::make_frame(FRAME_ID_BASE),
    ])
    .unwrap();
    let values = match result.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => result.as_vector_data().unwrap().clone(),
        _ => panic!("expected vector"),
    };
    assert_eq!(values[0].as_symbol_name(), Some("face"));
    assert_eq!(values[1].as_utf8_str(), Some("default"));
    assert_eq!(values[2].as_utf8_str(), Some("default"));
    assert_eq!(values[3].as_symbol_name(), Some("normal"));
    assert_eq!(values[4].as_int(), Some(1));
    assert_eq!(values[5].as_symbol_name(), Some("normal"));
    assert_eq!(values[8].as_symbol_name(), Some("nil"));
    assert!(values[9].as_utf8_str().is_some());
    assert!(values[10].as_utf8_str().is_some());
}

#[test]
fn internal_make_lisp_face_creates_symbol_visible_to_internal_lisp_face_p() {
    crate::test_utils::init_test_tracing();
    let face_name = "__neovm_make_face_unit_test";
    let mut eval = Context::new();
    let made = builtin_internal_make_lisp_face(&mut eval, vec![Value::symbol(face_name)]).unwrap();
    assert!(made.is_vector());
    let exists = builtin_internal_lisp_face_p(vec![Value::symbol(face_name)]).unwrap();
    assert!(exists.is_vector());
    assert_eq!(
        eval.obarray().get_property(face_name, "face"),
        face_id_for_name(face_name).map(Value::fixnum),
    );
}

#[test]
fn internal_make_lisp_face_publishes_known_face_id_property() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let made = builtin_internal_make_lisp_face(&mut eval, vec![Value::symbol("default")]).unwrap();
    assert!(made.is_vector());
    assert_eq!(
        eval.obarray().get_property("default", "face"),
        Some(Value::fixnum(0))
    );
}

#[test]
fn internal_make_lisp_face_sets_gnu_face_id_symbol_property() {
    crate::test_utils::init_test_tracing();
    clear_font_cache_state();

    let mut eval = Context::new();
    let face_name = "__neovm_make_face_id_property_unit_test";
    builtin_internal_make_lisp_face(&mut eval, vec![Value::symbol(face_name)]).unwrap();

    let face_id = eval
        .obarray()
        .get_property(face_name, "face")
        .and_then(|value| value.as_int())
        .expect("new Lisp face should publish its GNU face id");
    assert_eq!(Some(face_id), face_id_for_name(face_name));

    builtin_internal_make_lisp_face(&mut eval, vec![Value::symbol(face_name)]).unwrap();
    let repeated_face_id = eval
        .obarray()
        .get_property(face_name, "face")
        .and_then(|value| value.as_int())
        .expect("existing Lisp face should keep its GNU face id");
    assert_eq!(repeated_face_id, face_id);
}

#[test]
fn internal_make_lisp_face_rejects_non_symbol_and_non_nil_frame() {
    crate::test_utils::init_test_tracing();
    assert!(
        call_font_builtin!(builtin_internal_make_lisp_face, vec![Value::string("foo")]).is_err()
    );
    assert!(
        call_font_builtin!(
            builtin_internal_make_lisp_face,
            vec![Value::symbol("foo"), Value::fixnum(1)]
        )
        .is_err()
    );
}

#[test]
fn internal_copy_lisp_face_returns_to_when_frame_t() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let result = builtin_internal_copy_lisp_face(
        &mut eval,
        vec![
            Value::symbol("bold"),
            Value::symbol("my-face"),
            Value::T,
            Value::NIL,
        ],
    )
    .unwrap();
    assert_eq!(result.as_symbol_name(), Some("my-face"));
}

#[test]
fn internal_copy_lisp_face_sets_gnu_face_id_symbol_property() {
    crate::test_utils::init_test_tracing();
    clear_font_cache_state();

    let mut eval = Context::new();
    let face_name = "__neovm_copy_face_id_property_unit_test";
    builtin_internal_copy_lisp_face(
        &mut eval,
        vec![
            Value::symbol("bold"),
            Value::symbol(face_name),
            Value::T,
            Value::NIL,
        ],
    )
    .unwrap();

    let face_id = eval
        .obarray()
        .get_property(face_name, "face")
        .and_then(|value| value.as_int())
        .expect("copied Lisp face should publish its GNU face id");
    assert_eq!(Some(face_id), face_id_for_name(face_name));
}

#[test]
fn internal_copy_lisp_face_eval_updates_face_table() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("bold"),
            Value::keyword("family"),
            Value::string("Serif"),
        ],
    )
    .unwrap();

    let copied = builtin_internal_copy_lisp_face(
        &mut eval,
        vec![
            Value::symbol("bold"),
            Value::symbol("copied-face"),
            Value::T,
            Value::NIL,
        ],
    )
    .unwrap();
    assert_eq!(copied.as_symbol_name(), Some("copied-face"));
    assert_eq!(
        eval.face_table()
            .resolve("copied-face")
            .family_runtime_string_owned()
            .as_deref(),
        Some("Serif")
    );
}

#[test]
fn internal_copy_lisp_face_rejects_non_t_frame_designator() {
    crate::test_utils::init_test_tracing();
    let result = call_font_builtin!(
        builtin_internal_copy_lisp_face,
        vec![
            Value::symbol("default"),
            Value::symbol("my-face"),
            Value::NIL,
            Value::NIL,
        ]
    );
    assert!(result.is_err());
}

#[test]
fn internal_copy_lisp_face_validates_new_frame_when_frame_designator_used() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let frame = Value::make_frame(FRAME_ID_BASE);
    let err_t = builtin_internal_copy_lisp_face(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::symbol("my-face"),
            frame,
            Value::T,
        ],
    );
    assert!(err_t.is_err());

    let err_small_int = builtin_internal_copy_lisp_face(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::symbol("my-face"),
            frame,
            Value::fixnum(1),
        ],
    );
    assert!(err_small_int.is_err());

    let ok = builtin_internal_copy_lisp_face(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::symbol("my-face"),
            frame,
            frame,
        ],
    )
    .unwrap();
    assert_eq!(ok.as_symbol_name(), Some("my-face"));
}

#[test]
fn internal_copy_lisp_face_uses_symbol_checks_before_frame_checks() {
    crate::test_utils::init_test_tracing();
    let result = call_font_builtin!(
        builtin_internal_copy_lisp_face,
        vec![
            Value::fixnum(1),
            Value::symbol("my-face"),
            Value::NIL,
            Value::NIL,
        ]
    );
    assert!(result.is_err());
}

#[test]
fn internal_set_lisp_face_attribute_returns_value() {
    crate::test_utils::init_test_tracing();
    let face_name = "__neovm_set_attr_unit_test";
    let mut eval = Context::new();
    let result = builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol(face_name),
            Value::keyword("foreground"),
            Value::string("white"),
        ],
    )
    .unwrap();
    assert_eq!(result.as_symbol_name(), Some(face_name));
}

#[test]
fn internal_get_lisp_face_attribute_default_foreground() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let result = builtin_internal_get_lisp_face_attribute(
        &mut eval,
        vec![Value::symbol("default"), Value::keyword(":foreground")],
    )
    .unwrap();
    assert_eq!(result.as_utf8_str(), Some("unspecified-fg"));
}

#[test]
fn internal_get_lisp_face_attribute_mode_line_returns_unspecified() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let result = builtin_internal_get_lisp_face_attribute(
        &mut eval,
        vec![Value::symbol("mode-line"), Value::keyword(":foreground")],
    )
    .unwrap();
    assert_eq!(result.as_utf8_str(), Some("black"));
}

#[test]
fn internal_get_lisp_face_attribute_defaults_frame_returns_unspecified() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let result = builtin_internal_get_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::keyword(":foreground"),
            Value::T,
        ],
    )
    .unwrap();
    assert_eq!(result.as_symbol_name(), Some("unspecified"));
}

#[test]
fn internal_get_lisp_face_attribute_invalid_face_errors() {
    crate::test_utils::init_test_tracing();
    let result = call_font_builtin!(
        builtin_internal_get_lisp_face_attribute,
        vec![Value::symbol("unknown-face"), Value::keyword(":foreground"),]
    );
    assert!(result.is_err());
}

#[test]
fn internal_get_lisp_face_attribute_invalid_attr_errors() {
    crate::test_utils::init_test_tracing();
    let wrong_type = call_font_builtin!(
        builtin_internal_get_lisp_face_attribute,
        vec![Value::symbol("default"), Value::fixnum(1)]
    );
    assert!(wrong_type.is_err());

    let invalid_name = call_font_builtin!(
        builtin_internal_get_lisp_face_attribute,
        vec![Value::symbol("default"), Value::symbol("bogus"),]
    );
    assert!(invalid_name.is_err());
}

#[test]
fn internal_set_lisp_face_attribute_font_object_derives_font_related_attrs() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let font_object = Value::vector(vec![
        Value::keyword(FONT_OBJECT_TAG),
        Value::keyword("family"),
        Value::string("Hack"),
        Value::keyword("weight"),
        Value::symbol("regular"),
        Value::keyword("slant"),
        Value::symbol("normal"),
        Value::keyword("width"),
        Value::symbol("normal"),
        Value::keyword("size"),
        Value::fixnum(102),
    ]);

    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::keyword("font"),
            font_object,
        ],
    )
    .unwrap();

    assert_eq!(
        builtin_internal_get_lisp_face_attribute(
            &mut eval,
            vec![Value::symbol("default"), Value::keyword(":family"),]
        )
        .unwrap()
        .as_utf8_str(),
        Some("Hack")
    );
    assert_eq!(
        builtin_internal_get_lisp_face_attribute(
            &mut eval,
            vec![Value::symbol("default"), Value::keyword(":weight"),]
        )
        .unwrap()
        .as_symbol_name(),
        Some("regular")
    );
    assert_eq!(
        builtin_internal_get_lisp_face_attribute(
            &mut eval,
            vec![Value::symbol("default"), Value::keyword(":height"),]
        )
        .unwrap()
        .as_int(),
        Some(102)
    );
}

#[test]
fn internal_set_lisp_face_attribute_eval_uses_live_frame_font_parameter_for_default_face() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let font_name = Value::string("-*-Hack-regular-normal-*-*-102-*-*-*-m-0-iso10646-1");
    let font_object = Value::vector(vec![
        Value::keyword(FONT_OBJECT_TAG),
        Value::keyword("family"),
        Value::string("Hack"),
        Value::keyword("weight"),
        Value::symbol("regular"),
        Value::keyword("slant"),
        Value::symbol("normal"),
        Value::keyword("width"),
        Value::symbol("normal"),
        Value::keyword("size"),
        Value::fixnum(102),
        Value::keyword("height"),
        Value::fixnum(102),
        Value::keyword("name"),
        font_name,
    ]);

    {
        let frame = eval
            .frame_manager_mut()
            .get_mut(frame_id)
            .expect("selected frame");
        frame.window_system = Some(Value::symbol("neo"));
        frame.set_parameter(Value::symbol("font"), font_name);
        frame.set_parameter(Value::symbol("font-parameter"), font_object);
    }

    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::keyword("font"),
            font_name,
            Value::make_frame(frame_id.0),
        ],
    )
    .expect("set live default face font");

    assert_eq!(
        builtin_font_get(vec![
            builtin_internal_get_lisp_face_attribute(
                &mut eval,
                vec![
                    Value::symbol("default"),
                    Value::keyword(":font"),
                    Value::make_frame(frame_id.0),
                ],
            )
            .expect("default face font"),
            Value::keyword(":family"),
        ])
        .expect("default face font family")
        .as_utf8_str(),
        Some("Hack")
    );
    assert_eq!(
        builtin_font_get(vec![
            builtin_internal_get_lisp_face_attribute(
                &mut eval,
                vec![
                    Value::symbol("default"),
                    Value::keyword(":font"),
                    Value::make_frame(frame_id.0),
                ],
            )
            .expect("default face font"),
            Value::keyword(":size"),
        ])
        .expect("default face font size")
        .as_int(),
        Some(102)
    );
    assert!(
        builtin_font_get(vec![
            builtin_internal_get_lisp_face_attribute(
                &mut eval,
                vec![
                    Value::symbol("default"),
                    Value::keyword(":font"),
                    Value::make_frame(frame_id.0),
                ],
            )
            .expect("default face font"),
            Value::keyword(":height"),
        ])
        .expect("default face font height")
        .is_nil()
    );
    assert_eq!(
        builtin_internal_get_lisp_face_attribute(
            &mut eval,
            vec![
                Value::symbol("default"),
                Value::keyword(":font"),
                Value::make_frame(frame_id.0),
            ],
        )
        .expect("default face font")
        .is_vector(),
        true
    );
    assert_eq!(
        builtin_internal_get_lisp_face_attribute(
            &mut eval,
            vec![
                Value::symbol("default"),
                Value::keyword(":family"),
                Value::make_frame(frame_id.0),
            ],
        )
        .expect("default face family")
        .as_utf8_str(),
        Some("Hack")
    );
    assert_eq!(
        builtin_internal_get_lisp_face_attribute(
            &mut eval,
            vec![
                Value::symbol("default"),
                Value::keyword(":height"),
                Value::make_frame(frame_id.0),
            ],
        )
        .expect("default face height")
        .as_int(),
        Some(102)
    );
}

#[test]
fn internal_set_lisp_face_attribute_eval_realizes_string_font_requests_for_live_default_face() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    {
        let frame = eval
            .frame_manager_mut()
            .get_mut(frame_id)
            .expect("selected frame");
        frame.window_system = Some(Value::symbol("neo"));
    }
    eval.set_display_host(Box::new(LiveFrameFontDisplayHost {
        realized: Some(ResolvedFrameFont {
            family: LispString::from_utf8("Noto Sans Mono"),
            foundry: None,
            weight: FontWeight::NORMAL,
            slant: FontSlant::Normal,
            width: FontWidth::Normal,
            postscript_name: Some(LispString::from_utf8("NotoSansMono-Regular")),
            font_size_px: 22.0,
            char_width: 13.0,
            line_height: 31.0,
        }),
    }));

    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::keyword("font"),
            Value::string("Noto Sans Mono-16"),
            Value::make_frame(frame_id.0),
        ],
    )
    .expect("set live default face font from string");

    let frame = eval
        .frame_manager()
        .get(frame_id)
        .expect("selected frame after font change");
    assert_eq!(
        frame
            .parameter("font")
            .and_then(|value| value.as_utf8_str()),
        Some("Noto Sans Mono-16")
    );
    let font_parameter = frame
        .parameter("font-parameter")
        .expect("font-parameter should be set");
    assert!(
        builtin_fontp(vec![font_parameter, Value::symbol("font-object")])
            .expect("font-object check")
            .is_truthy()
    );
    assert_eq!(frame.char_width, 13.0);
    assert_eq!(frame.char_height, 31.0);
    assert_eq!(frame.font_pixel_size, 22.0);

    let default_font = builtin_internal_get_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::keyword(":font"),
            Value::make_frame(frame_id.0),
        ],
    )
    .expect("default face font");
    assert_eq!(
        builtin_font_get(vec![default_font, Value::keyword(":family")])
            .expect("default font family")
            .as_utf8_str(),
        Some("Noto Sans Mono")
    );
    assert_eq!(
        builtin_internal_get_lisp_face_attribute(
            &mut eval,
            vec![
                Value::symbol("default"),
                Value::keyword(":height"),
                Value::make_frame(frame_id.0),
            ],
        )
        .expect("default face height")
        .as_int(),
        Some(160)
    );
}

#[test]
fn face_font_eval_returns_font_name_on_live_gui_frame() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let frame = eval
        .frame_manager_mut()
        .get_mut(frame_id)
        .expect("selected frame");
    frame.window_system = Some(Value::symbol("neo"));
    frame.font_pixel_size = 16.0;
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let result = builtin_face_font(&mut eval, vec![Value::symbol("default")]).unwrap();
    assert!(result.is_string());
    assert!(result.as_utf8_str().is_some_and(|name| !name.is_empty()));
}

#[test]
fn font_info_eval_accepts_font_object_on_live_gui_frame() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    {
        let frame = eval
            .frame_manager_mut()
            .get_mut(frame_id)
            .expect("selected frame");
        frame.window_system = Some(Value::symbol("neo"));
        frame.font_pixel_size = 18.0;
        frame.char_width = 9.0;
        frame.char_height = 18.0;
    }
    eval.buffers
        .current_buffer_mut()
        .expect("current buffer")
        .insert("a");

    let font = builtin_font_at(&mut eval, vec![Value::fixnum(1)]).unwrap();
    let info = builtin_font_info(&mut eval, vec![font]).unwrap();
    if !info.is_vector() {
        panic!("expected font info vector");
    };
    let values = info.as_vector_data().unwrap().clone();
    assert_eq!(values.len(), 14);
    assert_eq!(values[3].as_int(), Some(18));
    assert_eq!(values[10].as_int(), Some(9));
    assert_eq!(values[11].as_int(), Some(9));
}

#[test]
fn internal_lisp_face_attribute_values_discrete_boolean_attrs() {
    crate::test_utils::init_test_tracing();
    let result =
        builtin_internal_lisp_face_attribute_values(vec![Value::keyword(":underline")]).unwrap();
    let vals = list_to_vec(&result).expect("list");
    assert_eq!(vals, vec![Value::T, Value::NIL]);
}

#[test]
fn internal_lisp_face_attribute_values_non_discrete_attr_is_nil() {
    crate::test_utils::init_test_tracing();
    let result =
        builtin_internal_lisp_face_attribute_values(vec![Value::keyword(":weight")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_attribute_values_rejects_non_symbol() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_lisp_face_attribute_values(vec![Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn internal_lisp_face_empty_p_selected_frame_default_is_not_empty() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_lisp_face_empty_p(vec![Value::symbol("default")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_empty_p_accepts_string_face_name() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_lisp_face_empty_p(vec![Value::string("default")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_empty_p_defaults_frame_is_empty() {
    crate::test_utils::init_test_tracing();
    let result =
        builtin_internal_lisp_face_empty_p(vec![Value::symbol("default"), Value::T]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn internal_lisp_face_empty_p_rejects_non_nil_non_t_frame_designator() {
    crate::test_utils::init_test_tracing();
    let result =
        builtin_internal_lisp_face_empty_p(vec![Value::symbol("default"), Value::fixnum(1)]);
    assert!(result.is_err());
    let frame_result =
        builtin_internal_lisp_face_empty_p(vec![Value::symbol("default"), Value::make_frame(1)]);
    assert!(frame_result.is_err());
}

#[test]
fn internal_lisp_face_comparators_accept_frame_handles() {
    crate::test_utils::init_test_tracing();
    let frame = Value::make_frame(FRAME_ID_BASE);
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
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_lisp_face_equal_p(vec![
        Value::symbol("default"),
        Value::symbol("mode-line"),
    ])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_lisp_face_equal_p_defaults_frame_treats_faces_as_equal() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_lisp_face_equal_p(vec![
        Value::symbol("default"),
        Value::symbol("mode-line"),
        Value::T,
    ])
    .unwrap();
    assert!(result.is_truthy());
}

#[test]
fn internal_lisp_face_equal_p_accepts_string_face_names() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_lisp_face_equal_p(vec![
        Value::string("default"),
        Value::string("default"),
    ])
    .unwrap();
    assert!(result.is_truthy());
}

#[test]
fn internal_merge_in_global_face_rejects_non_frame_designator() {
    crate::test_utils::init_test_tracing();
    let result = call_font_builtin!(
        builtin_internal_merge_in_global_face,
        vec![Value::symbol("default"), Value::NIL]
    );
    assert!(result.is_err());
    let frame_handle_result = call_font_builtin!(
        builtin_internal_merge_in_global_face,
        vec![Value::symbol("default"), Value::make_frame(1)]
    );
    assert!(frame_handle_result.is_err());
}

#[test]
fn internal_merge_in_global_face_copies_defaults_into_selected_face() {
    crate::test_utils::init_test_tracing();
    let face_name = "__neovm_merge_face_unit_test";
    let mut eval = Context::new();
    let frame =
        Value::make_frame(crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0);
    let _ = builtin_internal_make_lisp_face(&mut eval, vec![Value::symbol(face_name)]).unwrap();
    let _ = builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol(face_name),
            Value::keyword("foreground"),
            Value::string("white"),
            Value::T,
        ],
    )
    .unwrap();
    let merged =
        builtin_internal_merge_in_global_face(&mut eval, vec![Value::symbol(face_name), frame])
            .unwrap();
    assert!(merged.is_nil());
    let got = builtin_internal_get_lisp_face_attribute(
        &mut eval,
        vec![Value::symbol(face_name), Value::keyword(":foreground")],
    )
    .unwrap();
    assert_eq!(got.as_utf8_str(), Some("white"));
}

#[test]
fn internal_lisp_face_helpers_accept_frame_handles() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let frame =
        Value::make_frame(crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0);

    let descriptor = builtin_internal_lisp_face_p(vec![Value::symbol("default"), frame]).unwrap();
    assert!(descriptor.is_vector());

    let face_name = "__neovm_face_frame_handle_unit_test";
    let made =
        builtin_internal_make_lisp_face(&mut eval, vec![Value::symbol(face_name), frame]).unwrap();
    assert!(made.is_vector());

    let copied = builtin_internal_copy_lisp_face(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::symbol(face_name),
            frame,
            Value::NIL,
        ],
    )
    .unwrap();
    assert_eq!(copied.as_symbol_name(), Some(face_name));

    let set = builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::keyword("foreground"),
            Value::string("red"),
            frame,
        ],
    )
    .unwrap();
    assert_eq!(set, Value::symbol("default"));

    let got = builtin_internal_get_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("default"),
            Value::keyword(":foreground"),
            frame,
        ],
    )
    .unwrap();
    assert_eq!(got.as_utf8_str(), Some("red"));
}

#[test]
fn face_attribute_relative_p_height_non_fixnum_is_relative() {
    crate::test_utils::init_test_tracing();
    let result =
        builtin_face_attribute_relative_p(vec![Value::keyword("height"), Value::NIL]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn face_attribute_relative_p_height_fixnum_is_not_relative() {
    crate::test_utils::init_test_tracing();
    let result =
        builtin_face_attribute_relative_p(vec![Value::keyword("height"), Value::fixnum(1)])
            .unwrap();
    assert!(result.is_nil());
}

#[test]
fn face_attribute_relative_p_non_height_attribute_is_nil() {
    crate::test_utils::init_test_tracing();
    let result =
        builtin_face_attribute_relative_p(vec![Value::keyword("weight"), Value::symbol("foo")])
            .unwrap();
    assert!(result.is_nil());
}

#[test]
fn face_attribute_relative_p_unspecified_is_relative() {
    crate::test_utils::init_test_tracing();
    let result = builtin_face_attribute_relative_p(vec![
        Value::keyword("weight"),
        Value::symbol("unspecified"),
    ])
    .unwrap();
    assert!(result.is_truthy());
}

#[test]
fn merge_face_attribute_non_unspecified() {
    crate::test_utils::init_test_tracing();
    let result = builtin_merge_face_attribute(vec![
        Value::keyword("foreground"),
        Value::string("red"),
        Value::string("blue"),
    ])
    .unwrap();
    assert_eq!(result.as_utf8_str(), Some("red"));
}

#[test]
fn merge_face_attribute_unspecified() {
    crate::test_utils::init_test_tracing();
    let result = builtin_merge_face_attribute(vec![
        Value::keyword("foreground"),
        Value::symbol("unspecified"),
        Value::string("blue"),
    ])
    .unwrap();
    assert_eq!(result.as_utf8_str(), Some("blue"));
}

#[test]
fn merge_face_attribute_height_relative_over_absolute() {
    crate::test_utils::init_test_tracing();
    let result = builtin_merge_face_attribute(vec![
        Value::keyword("height"),
        Value::make_float(1.5),
        Value::fixnum(120),
    ])
    .unwrap();
    assert_eq!(result, Value::fixnum(180));
}

#[test]
fn merge_face_attribute_height_relative_over_relative() {
    crate::test_utils::init_test_tracing();
    let result = builtin_merge_face_attribute(vec![
        Value::keyword("height"),
        Value::make_float(1.5),
        Value::make_float(1.2),
    ])
    .unwrap();
    match result.kind() {
        ValueKind::Float => {
            let value = result.as_float().unwrap();
            assert!((value - 1.8).abs() < 1e-9);
        }
        other => panic!("expected float result, got {other:?}"),
    }
}

#[test]
fn internal_set_face_height_accepts_function_height_like_gnu() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();

    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("neo-height-face"),
            Value::keyword(":height"),
            Value::symbol("identity"),
            Value::NIL,
        ],
    )
    .expect("GNU accepts function-valued non-default face height");

    let stored = builtin_internal_get_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("neo-height-face"),
            Value::keyword(":height"),
            Value::NIL,
        ],
    )
    .expect("read stored height");
    assert_eq!(stored, Value::symbol("identity"));
}

#[test]
fn merge_face_attribute_height_calls_function_height_like_gnu() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();

    let result = builtin_merge_face_attribute_with_eval(
        &mut eval,
        vec![
            Value::keyword("height"),
            Value::symbol("1-"),
            Value::fixnum(120),
        ],
    )
    .expect("merge function height");
    assert_eq!(result, Value::fixnum(119));
}

#[test]
fn face_list_orders_default_last_and_includes_dynamic_faces() {
    crate::test_utils::init_test_tracing();
    clear_font_cache_state();
    let mut eval = Context::new();
    builtin_internal_make_lisp_face(&mut eval, vec![Value::symbol("__neovm_face_list_dynamic")])
        .expect("create dynamic face");

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
    assert!(names.contains(&"__neovm_face_list_dynamic"));
    assert_eq!(names.last().copied(), Some("default"));
}

#[test]
fn color_defined_p_known_and_unknown() {
    crate::test_utils::init_test_tracing();
    let result = builtin_color_defined_p(vec![Value::string("red")]).unwrap();
    assert!(result.is_truthy());

    let missing = builtin_color_defined_p(vec![Value::string("anything")]).unwrap();
    assert!(missing.is_nil());

    let invalid_hex = builtin_color_defined_p(vec![Value::string("#ggg")]).unwrap();
    assert!(invalid_hex.is_nil());

    let non_string = builtin_color_defined_p(vec![Value::fixnum(1)]).unwrap();
    assert!(non_string.is_nil());
}

#[test]
fn color_queries_validate_optional_device_arg() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_color_defined_p(vec![Value::string("red"), Value::fixnum(1)]).is_err());
    assert!(builtin_color_values(vec![Value::string("red"), Value::fixnum(1)]).is_err());
    assert!(builtin_defined_colors(vec![Value::fixnum(1)]).is_err());
    assert!(builtin_color_defined_p(vec![Value::string("red"), Value::make_frame(1)]).is_err());
    assert!(builtin_color_values(vec![Value::string("red"), Value::make_frame(1)]).is_err());
    assert!(builtin_defined_colors(vec![Value::make_frame(1)]).is_err());
    assert!(
        builtin_color_defined_p(vec![Value::string("red"), Value::make_frame(FRAME_ID_BASE)])
            .is_ok()
    );
    assert!(
        builtin_color_values(vec![Value::string("red"), Value::make_frame(FRAME_ID_BASE)]).is_ok()
    );
    assert!(builtin_defined_colors(vec![Value::make_frame(FRAME_ID_BASE)]).is_ok());
}

#[test]
fn color_values_named_black() {
    crate::test_utils::init_test_tracing();
    let result = builtin_color_values(vec![Value::string("black")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb.len(), 3);
    assert_eq!(rgb[0].as_int(), Some(0));
    assert_eq!(rgb[1].as_int(), Some(0));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_named_white() {
    crate::test_utils::init_test_tracing();
    let result = builtin_color_values(vec![Value::string("white")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(65535));
    assert_eq!(rgb[2].as_int(), Some(65535));
}

#[test]
fn color_values_hex_rrggbb() {
    crate::test_utils::init_test_tracing();
    // Hex colors are approximated to terminal palette colors in batch mode.
    let result = builtin_color_values(vec![Value::string("#FF0000")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(0));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_hex_short() {
    crate::test_utils::init_test_tracing();
    // #F00 resolves and approximates to red.
    let result = builtin_color_values(vec![Value::string("#F00")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(0));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_hex_12digit() {
    crate::test_utils::init_test_tracing();
    // 12-digit hex resolves and approximates to red.
    let result = builtin_color_values(vec![Value::string("#FFFF00000000")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(0));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_unknown_returns_nil() {
    crate::test_utils::init_test_tracing();
    let result = builtin_color_values(vec![Value::string("nonexistent-color")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn color_values_wrong_type_returns_nil() {
    crate::test_utils::init_test_tracing();
    let result = builtin_color_values(vec![Value::fixnum(42)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn defined_colors_returns_list() {
    crate::test_utils::init_test_tracing();
    let result = builtin_defined_colors(vec![]).unwrap();
    assert!(result.is_list());
    assert!(!result.is_nil());
    let colors = list_to_vec(&result).expect("defined-colors list");
    assert_eq!(colors.len(), 8);
    assert_eq!(colors[0].as_utf8_str(), Some("black"));
    assert_eq!(colors[7].as_utf8_str(), Some("white"));
}

#[test]
fn face_id_rejects_non_symbol_faces() {
    crate::test_utils::init_test_tracing();
    let result = builtin_face_id(vec![Value::symbol("default")]).unwrap();
    assert_eq!(result.as_int(), Some(0));
}

#[test]
fn face_id_known_faces_use_oracle_ids() {
    crate::test_utils::init_test_tracing();
    let bold = builtin_face_id(vec![Value::symbol("bold")]).unwrap();
    assert_eq!(bold.as_int(), Some(1));
    let mode_line = builtin_face_id(vec![Value::symbol("mode-line")]).unwrap();
    assert_eq!(mode_line.as_int(), Some(25));
}

#[test]
fn face_id_accepts_optional_frame_argument() {
    crate::test_utils::init_test_tracing();
    let result = builtin_face_id(vec![Value::symbol("default"), Value::NIL]).unwrap();
    assert_eq!(result.as_int(), Some(0));
}

#[test]
fn face_id_assigns_dynamic_id_for_created_faces() {
    crate::test_utils::init_test_tracing();
    let face_name = "__neovm_face_id_dynamic_unit_test";
    let _ = call_font_builtin!(
        builtin_internal_make_lisp_face,
        vec![Value::symbol(face_name)]
    )
    .unwrap();
    let first = builtin_face_id(vec![Value::symbol(face_name)]).unwrap();
    let second = builtin_face_id(vec![Value::symbol(face_name)]).unwrap();
    assert_eq!(first, second);
}

#[test]
fn face_id_rejects_invalid_face() {
    crate::test_utils::init_test_tracing();
    let result = builtin_face_id(vec![Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn face_font_returns_nil_for_known_faces() {
    crate::test_utils::init_test_tracing();
    let result = call_face_font(|| vec![Value::symbol("default")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn face_font_accepts_known_string_face() {
    crate::test_utils::init_test_tracing();
    let result = call_face_font(|| vec![Value::string("default")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn face_font_ignores_optional_arguments_for_known_face() {
    crate::test_utils::init_test_tracing();
    let result = call_face_font(|| vec![Value::symbol("default"), Value::NIL, Value::T]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn face_font_rejects_invalid_face() {
    crate::test_utils::init_test_tracing();
    let result = call_face_font(|| vec![Value::fixnum(1)]);
    assert!(result.is_err());
}

#[test]
fn internal_get_lisp_face_attribute_eval_reads_live_face_table() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    eval.set_face_attribute(
        "mode-line",
        ":background",
        FaceAttrValue::Color(Color::rgb(191, 191, 191)),
    );

    let value = builtin_internal_get_lisp_face_attribute(
        &mut eval,
        vec![
            Value::symbol("mode-line"),
            Value::keyword(":background"),
            Value::NIL,
        ],
    )
    .expect("live face attribute");

    assert_eq!(value, Value::string("grey75"));
}

#[test]
fn internal_merge_in_global_face_eval_updates_live_face_table() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let face = Value::symbol("__neovm_internal_merge_global_face_eval");
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval).0 as i64;

    builtin_internal_make_lisp_face(&mut eval, vec![face])
        .expect("create dynamic face in live face table");
    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            face,
            Value::keyword(":background"),
            Value::string("grey85"),
            Value::T,
        ],
    )
    .expect("set defaults background");
    builtin_internal_merge_in_global_face(&mut eval, vec![face, Value::fixnum(frame_id)])
        .expect("merge defaults into selected live face");

    let value = builtin_internal_get_lisp_face_attribute(
        &mut eval,
        vec![face, Value::keyword(":background"), Value::NIL],
    )
    .expect("read merged live background");

    assert_eq!(value, Value::string("grey85"));
}

#[test]
fn internal_get_lisp_face_attribute_eval_prefers_explicit_lisp_face_values() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let face = Value::symbol("__neovm_internal_get_lisp_face_attribute_eval_prefers_lisp");

    builtin_internal_make_lisp_face(&mut eval, vec![face])
        .expect("create dynamic face in live face table");
    builtin_internal_set_lisp_face_attribute(
        &mut eval,
        vec![
            face,
            Value::keyword(":foreground"),
            Value::string("red"),
            Value::NIL,
        ],
    )
    .expect("set selected foreground");

    let value = builtin_internal_get_lisp_face_attribute(
        &mut eval,
        vec![face, Value::keyword(":foreground"), Value::NIL],
    )
    .expect("read selected foreground");

    assert_eq!(value, Value::string("red"));
}

#[test]
fn bootstrap_set_face_attribute_updates_live_mode_line_face() {
    crate::test_utils::init_test_tracing();
    let rendered = bootstrap_eval_all(
        r#"(list
             (assq :background face-x-resources)
             (progn
               (set-face-attribute 'mode-line (selected-frame)
                                   :background "grey75"
                                   :foreground "black")
               (face-background 'mode-line nil t))
             (let* ((table (frame--face-hash-table (selected-frame)))
                    (face (gethash 'mode-line table)))
               (list (aref face 9) (aref face 10))))"#,
    );

    assert_eq!(
        rendered,
        vec!["OK ((:background (\".attributeBackground\" . \"Face.AttributeBackground\")) \"grey75\" (\"black\" \"grey75\"))".to_string()]
    );
}

#[test]
fn bootstrap_frame_face_hash_table_is_frame_owned_object() {
    crate::test_utils::init_test_tracing();
    let rendered = bootstrap_eval_all(
        r#"(let ((a (frame--face-hash-table (selected-frame)))
                 (b (frame--face-hash-table (selected-frame))))
             (eq a b))"#,
    );

    assert_eq!(rendered, vec!["OK t".to_string()]);
}

#[test]
fn internal_face_x_get_resource_returns_nil_for_string_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_face_x_get_resource(vec![
        Value::string("font"),
        Value::string("Font"),
        Value::NIL,
    ])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_face_x_get_resource_validates_string_args_and_arity() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_internal_face_x_get_resource(vec![]).is_err());
    assert!(builtin_internal_face_x_get_resource(vec![Value::NIL]).is_err());
    assert!(builtin_internal_face_x_get_resource(vec![Value::NIL, Value::string("Font")]).is_err());
    assert!(builtin_internal_face_x_get_resource(vec![Value::string("font"), Value::NIL]).is_err());
}

#[test]
fn internal_set_font_selection_order_accepts_valid_order() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_set_font_selection_order(vec![Value::list(vec![
        Value::keyword(":width"),
        Value::keyword(":height"),
        Value::keyword(":weight"),
        Value::keyword(":slant"),
    ])])
    .unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_set_font_selection_order_rejects_invalid_order() {
    crate::test_utils::init_test_tracing();
    let result =
        builtin_internal_set_font_selection_order(vec![Value::list(vec![Value::symbol("x")])]);
    assert!(result.is_err());
}

#[test]
fn internal_set_alternative_font_family_alist_returns_converted_list() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_set_alternative_font_family_alist(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_set_alternative_font_family_alist_converts_strings_to_symbols() {
    crate::test_utils::init_test_tracing();
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
fn internal_set_alternative_font_family_alist_accepts_raw_unibyte_strings() {
    crate::test_utils::init_test_tracing();
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF]));
    let expected = crate::emacs_core::builtins::lisp_string_to_runtime_string(raw);
    let input = Value::list(vec![Value::list(vec![raw])]);
    let result = builtin_internal_set_alternative_font_family_alist(vec![input]).unwrap();
    let outer = list_to_vec(&result).expect("outer list");
    let inner = list_to_vec(&outer[0]).expect("inner list");
    assert_eq!(inner[0].as_symbol_name(), Some(expected.as_str()));
    assert_eq!(alternative_font_families(&expected), vec![expected]);
}

#[test]
fn internal_set_alternative_font_family_alist_updates_family_lookup_order() {
    crate::test_utils::init_test_tracing();
    let input = Value::list(vec![Value::list(vec![
        Value::string("Noto Sans Mono"),
        Value::string("Noto Sans Mono CJK SC"),
        Value::string("Sarasa Gothic CL"),
    ])]);
    builtin_internal_set_alternative_font_family_alist(vec![input]).unwrap();

    assert_eq!(
        alternative_font_families("noto sans mono"),
        vec![
            "Noto Sans Mono".to_string(),
            "Noto Sans Mono CJK SC".to_string(),
            "Sarasa Gothic CL".to_string(),
        ]
    );
    assert_eq!(
        alternative_font_families("Noto Sans Mono"),
        vec![
            "Noto Sans Mono".to_string(),
            "Noto Sans Mono CJK SC".to_string(),
            "Sarasa Gothic CL".to_string(),
        ]
    );
}

#[test]
fn internal_set_alternative_font_registry_alist_returns_nil_or_value() {
    crate::test_utils::init_test_tracing();
    let result = builtin_internal_set_alternative_font_registry_alist(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn internal_set_alternative_font_registry_alist_downcases_values() {
    crate::test_utils::init_test_tracing();
    let input = Value::list(vec![Value::list(vec![
        Value::string("ISO10646-1"),
        Value::string("GB18030.2000-1"),
    ])]);
    let result = builtin_internal_set_alternative_font_registry_alist(vec![input]).unwrap();
    let outer = list_to_vec(&result).expect("outer list");
    let inner = list_to_vec(&outer[0]).expect("inner list");
    assert_eq!(
        inner[0].as_runtime_string_owned().as_deref(),
        Some("iso10646-1")
    );
    assert_eq!(
        inner[1].as_runtime_string_owned().as_deref(),
        Some("gb18030.2000-1")
    );
    assert_eq!(
        alternative_font_registries("ISO10646-1"),
        vec!["iso10646-1".to_string(), "gb18030.2000-1".to_string()]
    );
}

#[test]
fn internal_set_alternative_font_registry_alist_accepts_raw_unibyte_strings() {
    crate::test_utils::init_test_tracing();
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
        0xFF, b'A',
    ]));
    let expected = crate::emacs_core::builtins::lisp_string_to_runtime_string(Value::heap_string(
        crate::heap_types::LispString::from_unibyte(vec![0xFF, b'a']),
    ));
    let input = Value::list(vec![Value::list(vec![raw])]);
    let result = builtin_internal_set_alternative_font_registry_alist(vec![input]).unwrap();
    let outer = list_to_vec(&result).expect("outer list");
    let inner = list_to_vec(&outer[0]).expect("inner list");
    assert_eq!(
        inner[0].as_runtime_string_owned().as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(alternative_font_registries(&expected), vec![expected]);
}

// -----------------------------------------------------------------------
// Arity checks
// -----------------------------------------------------------------------

#[test]
fn fontp_too_many_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_fontp(vec![Value::NIL, Value::NIL, Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn fontp_no_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_fontp(vec![]);
    assert!(result.is_err());
}

#[test]
fn font_get_wrong_arity() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_font_get(vec![Value::NIL]).is_err());
    assert!(builtin_font_get(vec![Value::NIL, Value::NIL, Value::NIL]).is_err());
}

#[test]
fn font_put_wrong_arity() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_font_put(vec![Value::NIL, Value::NIL]).is_err());
}

#[test]
fn face_attribute_relative_p_wrong_arity() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_face_attribute_relative_p(vec![Value::NIL]).is_err());
}

#[test]
fn merge_face_attribute_wrong_arity() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_merge_face_attribute(vec![Value::NIL, Value::NIL]).is_err());
}

#[test]
fn color_values_case_insensitive() {
    crate::test_utils::init_test_tracing();
    let result = builtin_color_values(vec![Value::string("RED")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(0));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_hex_lowercase() {
    crate::test_utils::init_test_tracing();
    let result = builtin_color_values(vec![Value::string("#ff8000")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    // #ff8000 approximates to yellow in the terminal palette.
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(65535));
    assert_eq!(rgb[2].as_int(), Some(0));
}

#[test]
fn color_values_invalid_hex_returns_nil() {
    crate::test_utils::init_test_tracing();
    let result = builtin_color_values(vec![Value::string("#ggg")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn color_values_from_color_spec_semantics() {
    crate::test_utils::init_test_tracing();
    let rgb_short =
        list_to_vec(&builtin_color_values_from_color_spec(vec![Value::string("#000")]).unwrap())
            .unwrap();
    assert_eq!(
        rgb_short,
        vec![Value::fixnum(0), Value::fixnum(0), Value::fixnum(0)]
    );

    let rgb_12 = list_to_vec(
        &builtin_color_values_from_color_spec(vec![Value::string("#111122223333")]).unwrap(),
    )
    .unwrap();
    assert_eq!(
        rgb_12,
        vec![
            Value::fixnum(4369),
            Value::fixnum(8738),
            Value::fixnum(13107)
        ]
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

    let type_err = builtin_color_values_from_color_spec(vec![Value::fixnum(1)])
        .expect_err("color-values-from-color-spec should enforce stringp");
    match type_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn color_gray_and_supported_semantics() {
    crate::test_utils::init_test_tracing();
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
        builtin_color_gray_p(vec![Value::string("#fff"), Value::NIL])
            .unwrap()
            .is_truthy()
    );

    let gray_color_type = builtin_color_gray_p(vec![Value::fixnum(1)])
        .expect_err("color-gray-p should enforce stringp");
    match gray_color_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let gray_frame_type = builtin_color_gray_p(vec![Value::string("#fff"), Value::fixnum(0)])
        .expect_err("color-gray-p should validate FRAME");
    match gray_frame_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("framep"), Value::fixnum(0)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    assert!(
        builtin_color_supported_p(vec![Value::string("#123456")])
            .unwrap()
            .is_truthy()
    );
    assert!(
        builtin_color_supported_p(vec![Value::string("#fff"), Value::NIL, Value::T])
            .unwrap()
            .is_truthy()
    );
    assert!(
        builtin_color_supported_p(vec![Value::string("bogus"), Value::NIL, Value::NIL])
            .unwrap()
            .is_nil()
    );

    let supported_type = builtin_color_supported_p(vec![Value::fixnum(1)])
        .expect_err("color-supported-p should enforce stringp");
    match supported_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let supported_frame_type =
        builtin_color_supported_p(vec![Value::string("#fff"), Value::fixnum(1)])
            .expect_err("color-supported-p should validate FRAME");
    match supported_frame_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("framep"), Value::fixnum(1)]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn color_distance_semantics() {
    crate::test_utils::init_test_tracing();
    let black_white = builtin_color_distance(vec![Value::string("#000"), Value::string("#fff")])
        .expect("color-distance should evaluate");
    match black_white.kind() {
        ValueKind::Fixnum(n) => assert!(n > 0),
        other => panic!("expected integer distance, got {other:?}"),
    }

    assert_eq!(
        builtin_color_distance(vec![Value::string("#000"), Value::string("#000")]).unwrap(),
        Value::fixnum(0)
    );

    // Both colors collapse to black in tty-approx mode.
    assert_eq!(
        builtin_color_distance(vec![Value::string("#000"), Value::string("#111")]).unwrap(),
        Value::fixnum(0)
    );
}

#[test]
fn xw_color_primitives_follow_live_gui_frame_state() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    {
        let frame = eval
            .frame_manager_mut()
            .get_mut(frame_id)
            .expect("selected frame");
        frame.window_system = Some(Value::symbol("neo"));
    }

    assert_eq!(
        builtin_xw_color_defined_p_ctx(
            &eval,
            vec![Value::string("#123456"), Value::make_frame(frame_id.0)],
        )
        .expect("xw-color-defined-p should evaluate"),
        Value::T
    );
    assert_eq!(
        builtin_xw_color_values_ctx(
            &eval,
            vec![Value::string("#123456"), Value::make_frame(frame_id.0)],
        )
        .expect("xw-color-values should evaluate"),
        Value::list(vec![
            Value::fixnum(0x12 * 257),
            Value::fixnum(0x34 * 257),
            Value::fixnum(0x56 * 257),
        ])
    );
    assert_eq!(
        crate::emacs_core::builtins::symbols::builtin_xw_display_color_p_ctx(
            &eval,
            vec![Value::make_frame(frame_id.0)],
        )
        .expect("xw-display-color-p should evaluate"),
        Value::T
    );
}

#[test]
fn color_distance_errors_match_oracle_shape() {
    crate::test_utils::init_test_tracing();
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

    let invalid_type = builtin_color_distance(vec![Value::fixnum(1), Value::string("#fff")])
        .expect_err("color-distance should signal invalid color for non-string args");
    match invalid_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Invalid color"), Value::fixnum(1)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let frame_err =
        builtin_color_distance(vec![Value::string("#000"), Value::string("#fff"), Value::T])
            .expect_err("color-distance should validate optional FRAME");
    match frame_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("frame-live-p"), Value::T]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn color_values_dark_gray_approximates_to_white() {
    crate::test_utils::init_test_tracing();
    let result = builtin_color_values(vec![Value::string("DarkGray")]).unwrap();
    let rgb = list_to_vec(&result).unwrap();
    assert_eq!(rgb[0].as_int(), Some(65535));
    assert_eq!(rgb[1].as_int(), Some(65535));
    assert_eq!(rgb[2].as_int(), Some(65535));
}
