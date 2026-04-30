use super::*;
use crate::buffer::buffer::BUFFER_SLOT_BUFFER_FILE_CODING_SYSTEM;
use crate::emacs_core::Context;
use crate::emacs_core::intern::intern;
use crate::emacs_core::value::{
    StringTextPropertyRun, ValueKind, get_string_text_properties_table_for_value,
    set_string_text_properties_for_value,
};

#[test]
fn test_register_bootstrap_vars_include_tab_bar_display_vars() {
    crate::test_utils::init_test_tracing();
    let mut obarray = crate::emacs_core::symbol::Obarray::new();
    register_bootstrap_vars(&mut obarray);

    assert_eq!(obarray.symbol_value("inhibit-redisplay"), Some(&Value::NIL));
    assert_eq!(
        obarray.symbol_value("auto-resize-tab-bars"),
        Some(&Value::T)
    );
    assert_eq!(
        obarray.symbol_value("auto-raise-tab-bar-buttons"),
        Some(&Value::T)
    );
    assert_eq!(
        obarray.symbol_value("tab-bar-border"),
        Some(&Value::symbol("internal-border-width"))
    );
    assert_eq!(
        obarray.symbol_value("tab-bar-button-margin"),
        Some(&Value::fixnum(1))
    );
    assert_eq!(
        obarray.symbol_value("fontification-functions"),
        Some(&Value::NIL)
    );
    assert!(obarray.is_special("fontification-functions"));
    let fontification_functions = intern("fontification-functions");
    assert!(
        obarray
            .blv(fontification_functions)
            .is_some_and(|blv| blv.local_if_set)
    );
}

#[test]
fn test_format_mode_line() {
    crate::test_utils::init_test_tracing();
    let result =
        builtin_format_mode_line(vec![Value::string("test"), Value::symbol("default")]).unwrap();
    assert_eq!(result, Value::string(""));

    let result = builtin_format_mode_line(vec![
        Value::string("test"),
        Value::symbol("default"),
        Value::NIL,
    ])
    .unwrap();
    assert_eq!(result, Value::string(""));

    let err = builtin_format_mode_line(vec![
        Value::string("test"),
        Value::symbol("default"),
        Value::symbol("window"),
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let err = builtin_format_mode_line(vec![
        Value::string("test"),
        Value::symbol("default"),
        Value::NIL,
        Value::symbol("buffer"),
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    assert!(builtin_format_mode_line(vec![]).is_err());
}

#[test]
fn test_format_mode_line_eval_optional_designators() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-format", 80, 24, buffer_id);
    let window_id = eval.frames.get(frame_id).expect("frame").selected_window.0 as i64;

    let ok = builtin_format_mode_line_ctx(
        &mut eval,
        vec![
            Value::string("%b"),
            Value::NIL,
            Value::fixnum(window_id),
            Value::make_buffer(buffer_id),
        ],
    )
    .unwrap();
    // %b expands to the current buffer name
    let buf_name = eval
        .buffers
        .current_buffer()
        .map(|b| b.name_runtime_string_owned())
        .unwrap_or_default();
    assert_eq!(ok, Value::string(buf_name));

    let err = builtin_format_mode_line_ctx(
        &mut eval,
        vec![Value::string("%b"), Value::NIL, Value::string("x")],
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let err = builtin_format_mode_line_ctx(
        &mut eval,
        vec![
            Value::string("%b"),
            Value::NIL,
            Value::NIL,
            Value::string("x"),
        ],
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_resolve_live_window_display_context_uses_selected_window_buffer_point() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let selected_buffer_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval
        .frames
        .create_frame("xdisp-selected-point", 800, 600, selected_buffer_id);
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window;
    {
        let buffer = eval
            .buffers
            .get_mut(selected_buffer_id)
            .expect("selected window buffer");
        buffer.insert("abc\ndef\n");
        buffer.goto_byte(5);
    }
    let other_id = eval.buffers.create_buffer("*other*");
    eval.buffers.set_current(other_id);

    let ctx = resolve_live_window_display_context(
        &eval.frames,
        &eval.buffers,
        Some(&Value::make_window(selected_window.0)),
    )
    .expect("display context")
    .expect("selected window context");

    assert_eq!(ctx.window_point, 6);
    assert_eq!(eval.buffers.current_buffer_id(), Some(other_id));
}

#[test]
fn test_format_mode_line_eval_uses_explicit_buffer_instead_of_current_buffer() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let saved_current = eval.buffers.current_buffer_id().expect("current buffer");
    let other_id = eval.buffers.create_buffer("*other*");

    let ok = builtin_format_mode_line_ctx(
        &mut eval,
        vec![
            Value::string("%b"),
            Value::NIL,
            Value::NIL,
            Value::make_buffer(other_id),
        ],
    )
    .unwrap();

    assert_eq!(ok, Value::string("*other*"));
    assert_eq!(eval.buffers.current_buffer_id(), Some(saved_current));
}

#[test]
fn test_format_mode_line_eval_uses_window_buffer_instead_of_current_buffer() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let saved_current = eval.buffers.current_buffer_id().expect("current buffer");
    let frame_id = eval
        .frames
        .create_frame("xdisp-window", 80, 24, saved_current);
    let other_id = eval.buffers.create_buffer("*window*");
    let window_id = {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        let selected = frame.selected_window;
        let window = frame
            .find_window_mut(selected)
            .expect("selected window on frame");
        match window {
            crate::window::Window::Leaf { buffer_id, .. } => *buffer_id = other_id,
            other => panic!("expected leaf window, got {:?}", other),
        }
        selected.0 as i64
    };

    let ok = builtin_format_mode_line_ctx(
        &mut eval,
        vec![
            Value::string("%b"),
            Value::NIL,
            Value::make_window(window_id as u64),
            Value::NIL,
        ],
    )
    .unwrap();

    assert_eq!(ok, Value::string("*window*"));
    assert_eq!(eval.buffers.current_buffer_id(), Some(saved_current));
}

#[test]
fn test_format_mode_line_in_state_uses_buffer_local_symbols_and_restores_buffer() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let saved_current = eval.buffers.current_buffer_id().expect("current buffer");
    let other_id = eval.buffers.create_buffer("*mode-line*");
    eval.buffers
        .set_buffer_local_property(other_id, "mode-name", Value::string("Neo"))
        .expect("mode-name local should set");

    let rendered = format_mode_line_from_state(
        &eval.obarray,
        &[],
        &eval.frames,
        &mut eval.buffers,
        &eval.processes,
        vec![
            Value::list(vec![
                Value::string("%b "),
                Value::symbol("mode-name"),
                Value::string(" "),
                Value::symbol("mode-line-front-space"),
            ]),
            Value::NIL,
            Value::NIL,
            Value::make_buffer(other_id),
        ],
    )
    .expect("format-mode-line shared state")
    .expect("non-:eval format should stay on shared state");

    // Expected shape:
    //   "%b "                 -> "*mode-line* "        (buffer name + literal space)
    //   'mode-name            -> "Neo"                 (buffer-local value via
    //                                                   set_buffer_local_property)
    //   " "                                             (literal)
    //   'mode-line-front-space-> ""                    (unbound in bare
    //                                                   Context::new(): bindings.el
    //                                                   hasn't run, so the symbol
    //                                                   resolves to nil and GNU
    //                                                   xdisp.c:28438-28468 emits
    //                                                   nothing)
    //
    // This used to be asserted as "*mode-line* Neo  " (with two trailing spaces)
    // because the walker hardcoded mode-line-front-space to a space. That
    // short-circuit diverged from GNU and silently dropped the real symbol
    // value (e.g. bindings.el's `(:eval (if (display-graphic-p) " " "-"))`),
    // so it was removed.
    assert_eq!(rendered, Value::string("*mode-line* Neo "));
    assert_eq!(eval.buffers.current_buffer_id(), Some(saved_current));
}

#[test]
fn test_format_mode_line_eval_keeps_shared_buffer_context_around_eval_forms() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let saved_current = eval.buffers.current_buffer_id().expect("current buffer");
    let other_id = eval.buffers.create_buffer("*mode-line-eval*");
    eval.buffers
        .set_buffer_local_property(other_id, "mode-name", Value::string("Neo"))
        .expect("mode-name local should set");

    let rendered = builtin_format_mode_line_ctx(
        &mut eval,
        vec![
            Value::list(vec![
                Value::string("%b "),
                Value::list(vec![Value::symbol(":eval"), Value::symbol("mode-name")]),
            ]),
            Value::NIL,
            Value::NIL,
            Value::make_buffer(other_id),
        ],
    )
    .expect("format-mode-line eval");

    assert_eq!(rendered, Value::string("*mode-line-eval* Neo"));
    assert_eq!(eval.buffers.current_buffer_id(), Some(saved_current));
}

#[test]
fn test_format_mode_line_in_state_with_eval_keeps_shared_buffer_context_around_eval_forms() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let saved_current = eval.buffers.current_buffer_id().expect("current buffer");
    let other_id = eval.buffers.create_buffer("*mode-line-shared-eval*");
    eval.buffers
        .set_buffer_local_property(other_id, "mode-name", Value::string("Neo"))
        .expect("mode-name local should set");

    let rendered = finish_format_mode_line_in_state_with_eval(
        &eval.obarray,
        &[],
        &eval.frames,
        &mut eval.buffers,
        &eval.processes,
        &[
            Value::list(vec![
                Value::string("%b "),
                Value::list(vec![Value::symbol(":eval"), Value::symbol("mode-name")]),
            ]),
            Value::NIL,
            Value::NIL,
            Value::make_buffer(other_id),
        ],
        |form, buffers| {
            assert_eq!(*form, Value::symbol("mode-name"));
            let buffer = buffers.current_buffer().expect("mode-line buffer");
            Ok(buffer
                .get_buffer_local("mode-name")
                .expect("buffer-local mode-name"))
        },
    )
    .expect("format-mode-line shared eval");

    assert_eq!(rendered, Value::string("*mode-line-shared-eval* Neo"));
    assert_eq!(eval.buffers.current_buffer_id(), Some(saved_current));
}

#[test]
fn test_format_mode_line_preserves_raw_unibyte_literal_segments() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF]));

    let rendered = builtin_format_mode_line_ctx(&mut eval, vec![Value::list(vec![raw])])
        .expect("format-mode-line raw literal");
    let text = rendered
        .as_lisp_string()
        .expect("format-mode-line should return a LispString");
    assert!(!text.is_multibyte());
    assert_eq!(text.as_bytes(), &[0xFF]);
}

#[test]
fn test_format_mode_line_symbol_conditional_uses_only_selected_branch() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    eval.obarray.set_symbol_value("mode-line-flag", Value::T);

    let then_rendered = builtin_format_mode_line_ctx(
        &mut eval,
        vec![Value::list(vec![
            Value::symbol("mode-line-flag"),
            Value::string("then"),
            Value::list(vec![
                Value::symbol(":eval"),
                Value::list(vec![Value::symbol("error"), Value::string("boom")]),
            ]),
        ])],
    )
    .expect("format-mode-line should use then branch");

    eval.obarray.set_symbol_value("mode-line-flag", Value::NIL);
    let else_rendered = builtin_format_mode_line_ctx(
        &mut eval,
        vec![Value::list(vec![
            Value::symbol("mode-line-flag"),
            Value::list(vec![
                Value::symbol(":eval"),
                Value::list(vec![Value::symbol("error"), Value::string("boom")]),
            ]),
            Value::string("else"),
        ])],
    )
    .expect("format-mode-line should use else branch");

    assert_eq!(then_rendered, Value::string("then"));
    assert_eq!(else_rendered, Value::string("else"));
}

#[test]
fn test_format_mode_line_string_valued_symbols_render_literally() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let other_id = eval.buffers.create_buffer("*mode-line-literal*");
    eval.buffers
        .set_buffer_local_property(other_id, "mode-name", Value::string("%b"))
        .expect("mode-name local should set");

    let rendered = format_mode_line_from_state(
        &eval.obarray,
        &[],
        &eval.frames,
        &mut eval.buffers,
        &eval.processes,
        vec![
            Value::list(vec![Value::string("%b "), Value::symbol("mode-name")]),
            Value::NIL,
            Value::NIL,
            Value::make_buffer(other_id),
        ],
    )
    .expect("format-mode-line shared state")
    .expect("string-valued symbols should not require eval");

    assert_eq!(rendered, Value::string("*mode-line-literal* %b"));
}

#[test]
fn test_format_mode_line_fixnum_elements_pad_and_truncate_tail() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let other_id = eval.buffers.create_buffer("xy");

    let rendered = format_mode_line_from_state(
        &eval.obarray,
        &[],
        &eval.frames,
        &mut eval.buffers,
        &eval.processes,
        vec![
            Value::list(vec![
                Value::list(vec![Value::fixnum(5), Value::string("%b")]),
                Value::string("!"),
                Value::list(vec![Value::fixnum(-1), Value::string("%b")]),
            ]),
            Value::NIL,
            Value::NIL,
            Value::make_buffer(other_id),
        ],
    )
    .expect("format-mode-line shared state")
    .expect("fixnum elements should not require eval");

    assert_eq!(rendered, Value::string("xy   !x"));
}

#[test]
fn test_format_mode_line_percent_specs_keep_gnu_field_width_and_dash_semantics() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let other_id = eval.buffers.create_buffer("xy");

    let rendered = format_mode_line_from_state(
        &eval.obarray,
        &[],
        &eval.frames,
        &mut eval.buffers,
        &eval.processes,
        vec![
            Value::string("%5b|%-|%2*"),
            Value::NIL,
            Value::NIL,
            Value::make_buffer(other_id),
        ],
    )
    .expect("format-mode-line shared state")
    .expect("percent specs should not require eval");

    assert_eq!(rendered, Value::string("xy   |--|- "));
}

#[test]
fn test_format_mode_line_respects_risky_local_variable_for_eval_forms() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    eval.obarray.set_symbol_value(
        "unsafe-mode-line",
        Value::list(vec![
            Value::symbol(":eval"),
            Value::list(vec![Value::symbol("error"), Value::string("boom")]),
        ]),
    );
    eval.obarray.set_symbol_value(
        "trusted-mode-line",
        Value::list(vec![Value::symbol(":eval"), Value::string("ok")]),
    );
    eval.obarray
        .put_property("trusted-mode-line", "risky-local-variable", Value::T)
        .unwrap();

    let suppressed =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::symbol("unsafe-mode-line")])
            .expect("unsafe mode-line variable should be suppressed");
    let allowed = builtin_format_mode_line_ctx(&mut eval, vec![Value::symbol("trusted-mode-line")])
        .expect("trusted mode-line variable should evaluate");

    assert_eq!(suppressed, Value::string(""));
    assert_eq!(allowed, Value::string("ok"));
}

#[test]
fn test_format_mode_line_propertize_preserves_text_properties() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let rendered = builtin_format_mode_line_ctx(
        &mut eval,
        vec![Value::list(vec![
            Value::symbol(":propertize"),
            Value::string("abc"),
            Value::symbol("face"),
            Value::symbol("bold"),
            Value::symbol("help-echo"),
            Value::string("h"),
        ])],
    )
    .expect("format-mode-line propertize");

    assert_eq!(rendered.as_utf8_str(), Some("abc"));
    assert!(rendered.is_string(), "expected string result");
    let props =
        get_string_text_properties_table_for_value(rendered).expect("mode-line text properties");
    assert_eq!(
        props.get_property(0, Value::symbol("face")).copied(),
        Some(Value::symbol("bold"))
    );
    assert_eq!(
        props.get_property(0, Value::symbol("help-echo")).copied(),
        Some(Value::string("h"))
    );
}

#[test]
fn test_format_mode_line_percent_specs_preserve_source_string_text_properties() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("fmt-prop-buffer");
    eval.buffers.set_current(buffer_id);

    let format = Value::string("%b!");
    assert!(format.is_string(), "expected string format");
    set_string_text_properties_for_value(
        format,
        vec![StringTextPropertyRun {
            start: 0,
            end: 3,
            plist: Value::list(vec![
                Value::symbol("face"),
                Value::symbol("bold"),
                Value::symbol("help-echo"),
                Value::string("h"),
            ]),
        }],
    );

    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![format]).expect("format-mode-line props");

    assert_eq!(rendered.as_utf8_str(), Some("fmt-prop-buffer!"));
    if !rendered.is_string() {
        panic!("expected string result");
    };
    let props =
        get_string_text_properties_table_for_value(rendered).expect("mode-line text properties");
    assert_eq!(
        props.get_property(0, Value::symbol("face")).copied(),
        Some(Value::symbol("bold"))
    );
    assert_eq!(
        props.get_property(0, Value::symbol("help-echo")).copied(),
        Some(Value::string("h"))
    );
    let last_byte = "fmt-prop-buffer".len();
    assert_eq!(
        props
            .get_property(last_byte, Value::symbol("face"))
            .copied(),
        Some(Value::symbol("bold"))
    );
    assert_eq!(
        props
            .get_property(last_byte, Value::symbol("help-echo"))
            .copied(),
        Some(Value::string("h"))
    );
}

#[test]
fn test_format_mode_line_status_specs_match_gnu_buffer_state() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("status-buffer");
    eval.buffers.set_current(buffer_id);
    {
        let buffer = eval.buffers.get_mut(buffer_id).expect("buffer");
        buffer.insert("abc");
        buffer.set_modified(true);
        buffer.set_buffer_local("buffer-read-only", Value::T);
    }

    let status =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%*|%+|%&")]).expect("status");
    assert_eq!(status, Value::string("%|*|*"));

    {
        let buffer = eval.buffers.get_mut(buffer_id).expect("buffer");
        buffer.set_buffer_local("buffer-read-only", Value::NIL);
        buffer.set_modified(false);
        buffer.narrow_to_region(1, 2);
    }

    let narrowed =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%n")]).expect("narrow");
    assert_eq!(narrowed, Value::string(" Narrow"));
}

#[test]
fn test_format_mode_line_face_argument_adds_default_face_and_merges_explicit_face() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let rendered = builtin_format_mode_line_ctx(
        &mut eval,
        vec![
            Value::list(vec![
                Value::list(vec![
                    Value::symbol(":propertize"),
                    Value::string("a"),
                    Value::symbol("face"),
                    Value::symbol("italic"),
                ]),
                Value::string("b"),
            ]),
            Value::symbol("bold"),
        ],
    )
    .expect("format-mode-line face arg");

    assert_eq!(rendered.as_utf8_str(), Some("ab"));
    assert!(rendered.is_string(), "expected string result");
    let props =
        get_string_text_properties_table_for_value(rendered).expect("mode-line text properties");
    assert_eq!(
        props.get_property(0, Value::symbol("face")).copied(),
        Some(Value::list(vec![
            Value::symbol("italic"),
            Value::symbol("bold")
        ]))
    );
    assert_eq!(
        props.get_property(1, Value::symbol("face")).copied(),
        Some(Value::symbol("bold"))
    );
}

#[test]
fn test_format_mode_line_integer_face_argument_discards_text_properties() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let rendered = builtin_format_mode_line_ctx(
        &mut eval,
        vec![
            Value::list(vec![
                Value::symbol(":propertize"),
                Value::string("abc"),
                Value::symbol("face"),
                Value::symbol("bold"),
                Value::symbol("help-echo"),
                Value::string("h"),
            ]),
            Value::fixnum(0),
        ],
    )
    .expect("format-mode-line face int");

    assert_eq!(rendered, Value::string("abc"));
    assert!(rendered.is_string(), "expected string result");
    assert!(
        get_string_text_properties_table_for_value(rendered).is_none(),
        "integer FACE arg should drop text properties"
    );
}

#[test]
fn test_format_mode_line_fixnum_padding_does_not_inherit_inner_properties() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let rendered = builtin_format_mode_line_ctx(
        &mut eval,
        vec![Value::list(vec![
            Value::fixnum(5),
            Value::list(vec![
                Value::symbol(":propertize"),
                Value::string("x"),
                Value::symbol("face"),
                Value::symbol("bold"),
            ]),
        ])],
    )
    .expect("format-mode-line fixnum padding");

    assert_eq!(rendered.as_utf8_str(), Some("x    "));
    assert!(rendered.is_string(), "expected string result");
    let props =
        get_string_text_properties_table_for_value(rendered).expect("mode-line text properties");
    assert_eq!(
        props.get_property(0, Value::symbol("face")).copied(),
        Some(Value::symbol("bold"))
    );
    assert_eq!(props.get_property(1, Value::symbol("face")).copied(), None);
}

#[test]
fn test_format_mode_line_recursive_depth_specs_match_gnu() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();

    eval.command_loop.recursive_depth = 4;
    let shallow =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%[|%]")]).expect("depth 3");
    assert_eq!(shallow, Value::string("[[[|]]]"));

    eval.command_loop.recursive_depth = 7;
    let deep =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%[|%]")]).expect("depth 6");
    assert_eq!(deep, Value::string("[[[... | ...]]]"));
}

#[test]
fn test_format_mode_line_size_and_process_specs_match_gnu() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("mode-line-metadata");
    eval.buffers.set_current(buffer_id);
    {
        let buffer = eval.buffers.get_mut(buffer_id).expect("buffer");
        buffer.insert(&"x".repeat(1536));
    }

    let no_process =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%i|%I|%s")]).expect("specs");
    assert_eq!(no_process, Value::string("1536|1.5k|no process"));

    eval.processes.create_process(
        "mode-line-proc".into(),
        Value::make_buffer(buffer_id),
        "cat".into(),
        vec![],
    );
    let with_process =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%i|%I|%s")]).expect("specs");
    assert_eq!(with_process, Value::string("1536|1.5k|run"));
}

#[test]
fn test_format_mode_line_column_c_and_big_c_specs_match_gnu() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("col-test");
    eval.buffers.set_current(buffer_id);
    {
        let buffer = eval.buffers.get_mut(buffer_id).expect("buffer");
        buffer.insert("abcdef");
        buffer.goto_byte(3); // point at column 3 (0-indexed)
    }

    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%c|%C")]).expect("col specs");
    // %c = 0-indexed column (3), %C = 1-indexed column (4)
    assert_eq!(rendered, Value::string("3|4"));
}

#[test]
fn test_format_mode_line_major_mode_name_spec_matches_gnu() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("mode-test");
    eval.buffers.set_current(buffer_id);
    eval.buffers
        .set_buffer_local_property(buffer_id, "mode-name", Value::string("Emacs-Lisp"))
        .expect("set mode-name");

    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%m")]).expect("mode spec");
    assert_eq!(rendered, Value::string("Emacs-Lisp"));

    // Default mode-name is "Fundamental"
    let other_id = eval.buffers.create_buffer("default-mode");
    eval.buffers.set_current(other_id);
    let default =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%m")]).expect("default mode");
    assert_eq!(default, Value::string("Fundamental"));
}

#[test]
fn test_format_mode_line_major_mode_name_preserves_raw_unibyte_value() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("raw-mode-test");
    eval.buffers.set_current(buffer_id);
    let raw_mode = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
        0xFF, b'-', b'M',
    ]));
    eval.buffers
        .set_buffer_local_property(buffer_id, "mode-name", raw_mode)
        .expect("set raw mode-name");

    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%m")]).expect("mode spec");
    let text = rendered
        .as_lisp_string()
        .expect("format-mode-line should return a LispString");
    assert!(!text.is_multibyte());
    assert_eq!(text.as_bytes(), &[0xFF, b'-', b'M']);
}

#[test]
fn test_format_mode_line_frame_name_f_spec_prefers_title_and_preserves_raw_unibyte_value() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("frame-title-test");
    eval.buffers.set_current(buffer_id);

    let frame_id = eval.frames.create_frame("frame-name", 80, 24, buffer_id);
    let raw_title = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
        0xFF, b'-', b'F',
    ]));
    {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        frame.title = raw_title;
    }

    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%F")]).expect("frame title");
    let text = rendered
        .as_lisp_string()
        .expect("format-mode-line should return a LispString");
    assert!(!text.is_multibyte());
    assert_eq!(text.as_bytes(), &[0xFF, b'-', b'F']);
}

#[test]
fn test_format_mode_line_frame_name_f_spec_defaults_to_emacs_without_frame() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%F")]).expect("frame name");
    assert_eq!(rendered, Value::string("Emacs"));
}

#[test]
fn test_format_mode_line_frame_name_f_spec_uses_emacs_for_gui_frame_without_explicit_name() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("frame-name-default-test");
    eval.buffers.set_current(buffer_id);

    let frame_id = eval.frames.create_frame("F1", 80, 24, buffer_id);
    {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        frame.set_window_system(Some(Value::symbol("x")));
    }

    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%F")]).expect("frame name");
    assert_eq!(rendered, Value::string("Emacs"));
}

#[test]
fn test_format_mode_line_remote_at_spec_matches_gnu() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("remote-test");
    eval.buffers.set_current(buffer_id);

    // Local directory → "-"
    eval.buffers
        .set_buffer_local_property(buffer_id, "default-directory", Value::string("/home/user"))
        .expect("set local default-directory");
    let local =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%@")]).expect("local @");
    assert_eq!(local, Value::string("-"));

    // Remote (Tramp-style) directory → "@"
    eval.buffers
        .set_buffer_local_property(
            buffer_id,
            "default-directory",
            Value::string("/ssh:host:/home/user"),
        )
        .expect("set remote default-directory");
    let remote =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%@")]).expect("remote @");
    assert_eq!(remote, Value::string("@"));
}

#[test]
fn test_format_mode_line_remote_at_spec_accepts_raw_unibyte_default_directory() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("remote-raw-test");
    eval.buffers.set_current(buffer_id);
    let mut remote_dir = b"/ssh:host:/home/user".to_vec();
    remote_dir.push(0xFF);
    eval.buffers
        .set_buffer_local_property(
            buffer_id,
            "default-directory",
            Value::heap_string(crate::heap_types::LispString::from_unibyte(remote_dir)),
        )
        .expect("set raw remote default-directory");

    let remote =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%@")]).expect("remote @");
    assert_eq!(remote, Value::string("@"));
}

#[test]
fn test_format_mode_line_coding_system_z_and_big_z_specs_match_gnu() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("coding-test");
    eval.buffers.set_current(buffer_id);

    // utf-8-unix → mnemonic 'U', EOL ':'
    eval.buffers
        .set_buffer_local_property(
            buffer_id,
            "buffer-file-coding-system",
            Value::symbol("utf-8-unix"),
        )
        .expect("set coding");
    let z =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%z|%Z")]).expect("coding z");
    assert_eq!(z, Value::string("U|U:"));

    // undecided-dos → mnemonic '-', EOL '\'
    eval.buffers
        .set_buffer_local_property(
            buffer_id,
            "buffer-file-coding-system",
            Value::symbol("undecided-dos"),
        )
        .expect("set coding dos");
    let dos =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%z|%Z")]).expect("coding dos");
    assert_eq!(dos, Value::string("-|-\\"));
}

#[test]
fn test_format_mode_line_big_z_preserves_raw_unibyte_eol_indicator() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("coding-raw-eol-test");
    eval.buffers.set_current(buffer_id);

    eval.buffers
        .set_buffer_local_property(
            buffer_id,
            "buffer-file-coding-system",
            Value::symbol("undecided-dos"),
        )
        .expect("set coding");
    eval.obarray.set_symbol_value(
        "eol-mnemonic-dos",
        Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF])),
    );

    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%Z")]).expect("coding Z");
    let text = rendered
        .as_lisp_string()
        .expect("format-mode-line should return a LispString");
    assert!(!text.is_multibyte());
    assert_eq!(text.as_bytes(), &[b'-', 0xFF]);
}

#[test]
fn test_format_mode_line_tty_z_uses_live_coding_manager_state() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("tty-coding-test");
    eval.buffers.set_current(buffer_id);
    eval.frames
        .create_frame("tty-coding-frame", 80, 24, buffer_id);

    eval.buffers
        .set_buffer_local_property(
            buffer_id,
            "buffer-file-coding-system",
            Value::symbol("utf-8-unix"),
        )
        .expect("set coding");
    eval.obarray
        .set_symbol_value("terminal-coding-system", Value::NIL);
    eval.obarray
        .set_symbol_value("keyboard-coding-system", Value::NIL);

    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%z")]).expect("tty coding z");
    assert_eq!(rendered, Value::string("UUU"));
}

#[test]
fn test_format_mode_line_tty_z_reads_visible_buffer_file_coding_value_without_local_flag() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("tty-coding-visible-slot-test");
    eval.buffers.set_current(buffer_id);
    eval.frames
        .create_frame("tty-coding-frame", 80, 24, buffer_id);

    eval.buffers
        .set_buffer_local_property(
            buffer_id,
            "buffer-file-coding-system",
            Value::symbol("utf-8-unix"),
        )
        .expect("set coding");
    eval.buffers
        .get_mut(buffer_id)
        .expect("buffer")
        .set_slot_local_flag(BUFFER_SLOT_BUFFER_FILE_CODING_SYSTEM, false);
    eval.obarray
        .set_symbol_value("terminal-coding-system", Value::NIL);
    eval.obarray
        .set_symbol_value("keyboard-coding-system", Value::NIL);

    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%z")]).expect("tty coding z");
    assert_eq!(rendered, Value::string("UUU"));
}

#[test]
fn test_format_mode_line_tty_big_z_uses_live_coding_manager_state_and_eol_indicator() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("tty-coding-big-z-test");
    eval.buffers.set_current(buffer_id);
    eval.frames
        .create_frame("tty-coding-frame", 80, 24, buffer_id);

    eval.buffers
        .set_buffer_local_property(
            buffer_id,
            "buffer-file-coding-system",
            Value::symbol("utf-8-unix"),
        )
        .expect("set coding");
    eval.obarray
        .set_symbol_value("terminal-coding-system", Value::NIL);
    eval.obarray
        .set_symbol_value("keyboard-coding-system", Value::NIL);

    let rendered =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%Z")]).expect("tty coding Z");
    assert_eq!(rendered, Value::string("UUU:"));
}

#[test]
fn test_format_mode_line_propertize_display_min_width_matches_gnu_spacing() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let display = Value::list(vec![
        Value::symbol("min-width"),
        Value::list(vec![Value::make_float(6.0)]),
    ]);
    let format = Value::list(vec![
        Value::symbol(":propertize"),
        Value::string("All"),
        Value::symbol("display"),
        display,
    ]);

    let rendered = builtin_format_mode_line_ctx(&mut eval, vec![format]).expect("mode-line");
    assert_eq!(rendered, Value::string("All   "));
}

#[test]
fn test_format_mode_line_position_o_and_q_specs() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buffer_id = eval.buffers.create_buffer("pos-test");
    eval.buffers.set_current(buffer_id);

    // Empty buffer → "All" for %o, "All   " (with trailing spaces) for %q (GNU compat)
    let empty =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%o|%q")]).expect("empty");
    assert_eq!(empty, Value::string("All|All   "));

    // With content and no window set, fallback covers full buffer → "All"
    {
        let buffer = eval.buffers.get_mut(buffer_id).expect("buffer");
        buffer.insert(&"x".repeat(100));
    }
    let all_visible =
        builtin_format_mode_line_ctx(&mut eval, vec![Value::string("%o|%p")]).expect("all");
    assert_eq!(all_visible, Value::string("All|All"));

    // Set up frame/window to test partial visibility.
    let frame_id = eval.frames.create_frame("pos-frame", 80, 24, buffer_id);
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window;
    // Window showing middle portion: start=20, simulated visible range [20..80].
    {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        match window {
            crate::window::Window::Leaf { window_start, .. } => {
                *window_start = 20;
            }
            other => panic!("expected leaf window, got {:?}", other),
        }
    }

    let mid = builtin_format_mode_line_ctx(
        &mut eval,
        vec![
            Value::string("%o|%p|%P"),
            Value::NIL,
            Value::make_window(selected_window.0),
        ],
    )
    .expect("mid pos");
    // %o: toppos=20 > begv=0 → not "Top"; botpos=100 >= zv=100 → "Bottom"
    // %p: botpos >= zv → pos(20) > begv(0) → "Bottom"
    // %P: botpos >= zv → toppos(20) > begv(0) → "Bottom"
    assert_eq!(mid, Value::string("Bottom|Bottom|Bottom"));

    // Window at the very start
    {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        match window {
            crate::window::Window::Leaf { window_start, .. } => {
                *window_start = 0;
            }
            other => panic!("expected leaf window, got {:?}", other),
        }
    }
    let at_top = builtin_format_mode_line_ctx(
        &mut eval,
        vec![
            Value::string("%o|%p"),
            Value::NIL,
            Value::make_window(selected_window.0),
        ],
    )
    .expect("top pos");
    // window_start=0 and window_end(=zv)=100 >= zv → All
    assert_eq!(at_top, Value::string("All|All"));
}

#[test]
fn test_format_mode_line_percent_specs_use_window_buffer_and_completed_window_end() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let target_id = eval.buffers.create_buffer("window-target");
    {
        let buffer = eval.buffers.get_mut(target_id).expect("target buffer");
        buffer.insert(&"x".repeat(100));
    }
    let other_id = eval.buffers.create_buffer("other-buffer");
    {
        let buffer = eval.buffers.get_mut(other_id).expect("other buffer");
        buffer.insert(&"y".repeat(1000));
    }
    let frame_id = eval.frames.create_frame("pos-frame", 80, 24, target_id);
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window;
    {
        let target = eval.buffers.get(target_id).expect("target buffer");
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        match window {
            crate::window::Window::Leaf { window_start, .. } => {
                *window_start = 20;
                window.set_window_end_from_positions(
                    target.point_max_char().saturating_add(1),
                    target.point_max_byte(),
                    target.point_max_char(),
                    target.point_max_byte(),
                    0,
                );
            }
            other => panic!("expected leaf window, got {:?}", other),
        }
    }
    eval.buffers.set_current(other_id);

    let rendered = builtin_format_mode_line_ctx(
        &mut eval,
        vec![
            Value::string("%o|%p|%P"),
            Value::NIL,
            Value::make_window(selected_window.0),
        ],
    )
    .expect("mode-line percent specs");

    assert_eq!(rendered, Value::string("Bottom|Bottom|Bottom"));
}

#[test]
fn test_invisible_p() {
    crate::test_utils::init_test_tracing();
    let err = builtin_invisible_p(vec![Value::fixnum(0)]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "args-out-of-range"),
        other => panic!("expected args-out-of-range, got {:?}", other),
    }
    let result = builtin_invisible_p(vec![Value::fixnum(1)]).unwrap();
    assert!(result.is_nil());

    let result = builtin_invisible_p(vec![Value::symbol("invisible")]).unwrap();
    assert!(result.is_truthy());

    let result = builtin_invisible_p(vec![Value::fixnum(-1)]).unwrap();
    assert!(result.is_truthy());

    let result = builtin_invisible_p(vec![Value::NIL]).unwrap();
    assert!(result.is_nil());

    let result = builtin_invisible_p(vec![Value::string("x")]).unwrap();
    assert!(result.is_truthy());

    let result = builtin_invisible_p(vec![Value::make_float(1.5)]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn test_line_pixel_height() {
    crate::test_utils::init_test_tracing();
    let result = builtin_line_pixel_height(vec![]).unwrap();
    assert_eq!(result, Value::fixnum(1));
}

#[test]
fn test_window_text_pixel_size() {
    crate::test_utils::init_test_tracing();
    let result = builtin_window_text_pixel_size(vec![]).unwrap();
    if result.is_cons() {
        let pair_car = result.cons_car();
        let pair_cdr = result.cons_cdr();
        assert_eq!(pair_car, Value::fixnum(0));
        assert_eq!(pair_cdr, Value::fixnum(0));
    } else {
        panic!("expected cons");
    }
}

#[test]
fn test_window_text_pixel_size_arg_validation() {
    crate::test_utils::init_test_tracing();
    let err = builtin_window_text_pixel_size(vec![Value::fixnum(1)]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let err = builtin_window_text_pixel_size(vec![Value::NIL, Value::symbol("x")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let err = builtin_window_text_pixel_size(vec![Value::NIL, Value::NIL, Value::symbol("x")])
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    // X-LIMIT / Y-LIMIT / MODE / PIXELWISE are accepted without strict type checks.
    assert!(
        builtin_window_text_pixel_size(vec![
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::symbol("x"),
            Value::symbol("y"),
            Value::symbol("z"),
            Value::symbol("m"),
        ])
        .is_ok()
    );
}

#[test]
fn test_window_text_pixel_size_eval_window_validation() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-test", 80, 24, buf_id);
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window.0 as i64;

    let ok = builtin_window_text_pixel_size_ctx(&mut eval, vec![Value::fixnum(selected_window)])
        .unwrap();
    match ok.kind() {
        ValueKind::Cons => {}
        other => panic!("expected cons return, got {other:?}"),
    }

    let err =
        builtin_window_text_pixel_size_ctx(&mut eval, vec![Value::fixnum(999_999)]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_window_text_pixel_size_tty_frame_uses_char_cell_metrics() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-test", 80, 24, buf_id);
    {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        frame.char_width = 1.0;
        frame.char_height = 1.0;
        frame.font_pixel_size = 1.0;
        frame.set_window_system(None);
    }
    {
        let buffer = eval.buffers.get_mut(buf_id).expect("buffer");
        buffer.insert("tiny\n");
    }
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window.0 as i64;

    let result = builtin_window_text_pixel_size_ctx(
        &mut eval,
        vec![
            Value::fixnum(selected_window),
            Value::NIL,
            Value::T,
            Value::NIL,
            Value::fixnum(24),
            Value::T,
        ],
    )
    .unwrap();
    assert!(result.is_cons(), "expected cons, got {:?}", result.kind());
    assert_eq!(result.cons_car(), Value::fixnum(4));
    assert_eq!(result.cons_cdr(), Value::fixnum(2));
}

#[test]
fn test_window_text_pixel_size_matches_gnu_trailing_line_semantics() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-test", 80, 24, buf_id);
    {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        frame.char_width = 1.0;
        frame.char_height = 1.0;
        frame.font_pixel_size = 1.0;
        frame.set_window_system(None);
    }
    {
        let buffer = eval.buffers.get_mut(buf_id).expect("buffer");
        buffer.insert("hello\nworld\n\n");
    }
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window.0 as i64;

    let result =
        builtin_window_text_pixel_size_ctx(&mut eval, vec![Value::fixnum(selected_window)])
            .unwrap();
    assert!(result.is_cons(), "expected cons, got {:?}", result.kind());
    assert_eq!(result.cons_car(), Value::fixnum(5));
    assert_eq!(result.cons_cdr(), Value::fixnum(3));

    let result = builtin_window_text_pixel_size_ctx(
        &mut eval,
        vec![
            Value::fixnum(selected_window),
            Value::NIL,
            Value::T,
            Value::NIL,
            Value::fixnum(24),
            Value::T,
        ],
    )
    .unwrap();
    assert!(result.is_cons(), "expected cons, got {:?}", result.kind());
    assert_eq!(result.cons_car(), Value::fixnum(5));
    assert_eq!(
        result.cons_cdr(),
        Value::fixnum(3),
        "TO=t trims trailing blank lines before adding the mode line"
    );
}

#[test]
fn test_pos_visible_in_window_p() {
    crate::test_utils::init_test_tracing();
    let result = builtin_pos_visible_in_window_p(vec![Value::fixnum(1)]).unwrap();
    assert!(result.is_nil());

    let result = builtin_pos_visible_in_window_p(vec![Value::fixnum(100), Value::symbol("window")])
        .unwrap_err();
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data[0], Value::symbol("window-live-p"));
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let result =
        builtin_pos_visible_in_window_p(vec![Value::symbol("left"), Value::fixnum(1)]).unwrap_err();
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data[0], Value::symbol("window-live-p"));
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let result = builtin_pos_visible_in_window_p(vec![Value::symbol("left")]).unwrap_err();
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data[0], Value::symbol("integer-or-marker-p"));
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let result =
        builtin_pos_visible_in_window_p(vec![Value::fixnum(1), Value::NIL, Value::fixnum(1)])
            .unwrap();
    assert!(result.is_nil());
}

#[test]
fn test_pos_visible_in_window_p_eval_window_validation() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let err = builtin_pos_visible_in_window_p_ctx(&mut eval, vec![Value::NIL, Value::string("x")])
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let err = builtin_pos_visible_in_window_p_ctx(
        &mut eval,
        vec![Value::symbol("left"), Value::fixnum(1)],
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data[0], Value::symbol("window-live-p"));
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let ok = builtin_pos_visible_in_window_p_ctx(&mut eval, vec![Value::fixnum(1)]).unwrap();
    assert!(ok.is_nil());
}

#[test]
fn test_pos_visible_in_window_p_eval_returns_partial_geometry_for_live_window() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-pos", 160, 64, buf_id);
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window;
    {
        let buf = eval.buffers.get_mut(buf_id).expect("buffer");
        buf.insert("abc\ndef\nghi\n");
        buf.goto_byte(4);
    }
    {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        match window {
            crate::window::Window::Leaf {
                window_start,
                point,
                ..
            } => {
                *window_start = 1;
                *point = 5;
            }
            other => panic!("expected leaf window, got {:?}", other),
        }
    }

    let result = builtin_pos_visible_in_window_p_ctx(
        &mut eval,
        vec![
            Value::fixnum(5),
            Value::make_window(selected_window.0),
            Value::T,
        ],
    )
    .unwrap();
    assert_eq!(super::super::print::print_value(&result), "(0 16)");
}

#[test]
fn test_window_line_height_eval_returns_live_gui_row_metrics() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval
        .frames
        .create_frame("xdisp-line-height", 160, 64, buf_id);
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window;
    {
        let buf = eval.buffers.get_mut(buf_id).expect("buffer");
        buf.insert("abc\ndef\nghi\n");
        buf.goto_byte(4);
    }
    {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        match window {
            crate::window::Window::Leaf {
                window_start,
                point,
                ..
            } => {
                *window_start = 1;
                *point = 5;
            }
            other => panic!("expected leaf window, got {:?}", other),
        }
    }

    let current = builtin_window_line_height(
        &mut eval,
        vec![Value::NIL, Value::make_window(selected_window.0)],
    )
    .unwrap();
    let last = builtin_window_line_height(
        &mut eval,
        vec![Value::fixnum(-1), Value::make_window(selected_window.0)],
    )
    .unwrap();
    assert_eq!(super::super::print::print_value(&current), "(16 1 16 0)");
    assert_eq!(super::super::print::print_value(&last), "(16 1 16 0)");
}

#[test]
fn test_posn_at_point_eval_uses_exact_redisplay_snapshot() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-posn", 160, 64, buf_id);
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window;
    {
        let buf = eval.buffers.get_mut(buf_id).expect("buffer");
        buf.insert("abcdef\n");
        buf.goto_byte(4);
    }
    {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        match window {
            crate::window::Window::Leaf {
                window_start,
                point,
                ..
            } => {
                *window_start = 1;
                *point = 5;
            }
            other => panic!("expected leaf window, got {:?}", other),
        }
        frame.replace_display_snapshots(vec![crate::window::WindowDisplaySnapshot {
            window_id: selected_window,
            text_area_left_offset: 8,
            points: vec![crate::window::DisplayPointSnapshot {
                buffer_pos: 5,
                x: 24,
                y: 18,
                width: 21,
                height: 30,
                row: 1,
                col: 3,
            }],
            rows: vec![crate::window::DisplayRowSnapshot {
                row: 1,
                y: 18,
                height: 30,
                start_x: 0,
                start_col: 0,
                end_x: 0,
                end_col: 0,
                start_buffer_pos: Some(5),
                end_buffer_pos: Some(5),
            }],
            ..crate::window::WindowDisplaySnapshot::default()
        }]);
    }

    let result = builtin_posn_at_point(
        &mut eval,
        vec![Value::fixnum(5), Value::make_window(selected_window.0)],
    )
    .unwrap();
    assert_eq!(
        super::super::print::print_value(&result),
        "(#<window 1> 5 (24 . 18) 0 nil 5 (3 . 1) nil (0 . 0) (21 . 30))"
    );
}

#[test]
fn test_posn_at_x_y_eval_uses_exact_redisplay_snapshot() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-posn-xy", 160, 64, buf_id);
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window;
    {
        let buf = eval.buffers.get_mut(buf_id).expect("buffer");
        buf.insert("abcdef\n");
        buf.goto_byte(4);
    }
    {
        let frame = eval.frames.get_mut(frame_id).expect("frame");
        frame.replace_display_snapshots(vec![crate::window::WindowDisplaySnapshot {
            window_id: selected_window,
            text_area_left_offset: 8,
            points: vec![crate::window::DisplayPointSnapshot {
                buffer_pos: 5,
                x: 24,
                y: 18,
                width: 21,
                height: 30,
                row: 1,
                col: 3,
            }],
            rows: vec![crate::window::DisplayRowSnapshot {
                row: 1,
                y: 18,
                height: 30,
                start_x: 0,
                start_col: 0,
                end_x: 0,
                end_col: 0,
                start_buffer_pos: Some(5),
                end_buffer_pos: Some(5),
            }],
            ..crate::window::WindowDisplaySnapshot::default()
        }]);
    }

    let text_relative = builtin_posn_at_x_y(
        &mut eval,
        vec![
            Value::fixnum(30),
            Value::fixnum(20),
            Value::make_window(selected_window.0),
            Value::NIL,
        ],
    )
    .unwrap();
    assert_eq!(
        super::super::print::print_value(&text_relative),
        "(#<window 1> 5 (24 . 18) 0 nil 5 (3 . 1) nil (0 . 0) (21 . 30))"
    );

    let whole_window = builtin_posn_at_x_y(
        &mut eval,
        vec![
            Value::fixnum(38),
            Value::fixnum(20),
            Value::make_window(selected_window.0),
            Value::T,
        ],
    )
    .unwrap();
    assert_eq!(
        super::super::print::print_value(&whole_window),
        "(#<window 1> 5 (24 . 18) 0 nil 5 (3 . 1) nil (0 . 0) (21 . 30))"
    );
}

#[test]
fn test_posn_at_point_eval_returns_nil_outside_visible_snapshot_span() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("xdisp-posn-offscreen", 160, 64, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let buf = eval.buffers.get_mut(buf_id).expect("buffer");
        buf.insert("abcdefghijklmnopqrstuvwxyz\n");
        buf.goto_byte(0);
    }
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        frame.replace_display_snapshots(vec![crate::window::WindowDisplaySnapshot {
            window_id: selected_window,
            text_area_left_offset: 8,
            points: vec![
                crate::window::DisplayPointSnapshot {
                    buffer_pos: 10,
                    x: 24,
                    y: 18,
                    width: 8,
                    height: 16,
                    row: 0,
                    col: 2,
                },
                crate::window::DisplayPointSnapshot {
                    buffer_pos: 14,
                    x: 56,
                    y: 18,
                    width: 8,
                    height: 16,
                    row: 0,
                    col: 6,
                },
            ],
            rows: vec![crate::window::DisplayRowSnapshot {
                row: 0,
                y: 18,
                height: 16,
                start_x: 0,
                start_col: 0,
                end_x: 0,
                end_col: 0,
                start_buffer_pos: Some(10),
                end_buffer_pos: Some(14),
            }],
            ..crate::window::WindowDisplaySnapshot::default()
        }]);
    }

    let before = builtin_posn_at_point(
        &mut eval,
        vec![Value::fixnum(5), Value::make_window(selected_window.0)],
    )
    .unwrap();
    let after = builtin_posn_at_point(
        &mut eval,
        vec![Value::fixnum(20), Value::make_window(selected_window.0)],
    )
    .unwrap();
    let hidden_gap = builtin_posn_at_point(
        &mut eval,
        vec![Value::fixnum(12), Value::make_window(selected_window.0)],
    )
    .unwrap();

    assert!(
        before.is_nil(),
        "expected offscreen position before span to be nil, got {before:?}"
    );
    assert!(
        after.is_nil(),
        "expected offscreen position after span to be nil, got {after:?}"
    );
    assert_eq!(
        super::super::print::print_value(&hidden_gap),
        "(#<window 1> 14 (56 . 18) 0 nil 14 (6 . 0) nil (0 . 0) (8 . 16))"
    );
}

#[test]
fn test_posn_at_point_eval_returns_nil_for_positions_missing_entire_visible_row() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("xdisp-posn-missing-row", 160, 96, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let buf = eval.buffers.get_mut(buf_id).expect("buffer");
        buf.insert("abcdef\n");
        buf.goto_byte(0);
    }
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        frame.replace_display_snapshots(vec![crate::window::WindowDisplaySnapshot {
            window_id: selected_window,
            text_area_left_offset: 8,
            points: vec![
                crate::window::DisplayPointSnapshot {
                    buffer_pos: 1,
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 16,
                    row: 0,
                    col: 0,
                },
                crate::window::DisplayPointSnapshot {
                    buffer_pos: 4,
                    x: 0,
                    y: 18,
                    width: 8,
                    height: 16,
                    row: 1,
                    col: 0,
                },
            ],
            rows: vec![
                crate::window::DisplayRowSnapshot {
                    row: 0,
                    y: 0,
                    height: 16,
                    start_x: 0,
                    start_col: 0,
                    end_x: 0,
                    end_col: 0,
                    start_buffer_pos: Some(1),
                    end_buffer_pos: Some(1),
                },
                crate::window::DisplayRowSnapshot {
                    row: 1,
                    y: 18,
                    height: 16,
                    start_x: 0,
                    start_col: 0,
                    end_x: 0,
                    end_col: 0,
                    start_buffer_pos: Some(4),
                    end_buffer_pos: Some(4),
                },
            ],
            ..crate::window::WindowDisplaySnapshot::default()
        }]);
    }

    let missing = builtin_posn_at_point(
        &mut eval,
        vec![Value::fixnum(2), Value::make_window(selected_window.0)],
    )
    .unwrap();
    assert!(
        missing.is_nil(),
        "expected missing position between visible rows to be nil, got {missing:?}"
    );
}

#[test]
fn test_move_point_visually() {
    crate::test_utils::init_test_tracing();
    for direction in [1_i64, 0, -1, 2] {
        let err = builtin_move_point_visually(vec![Value::fixnum(direction)]).unwrap_err();
        match err {
            Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "args-out-of-range"),
            other => panic!("expected args-out-of-range, got {:?}", other),
        }
    }

    let err = builtin_move_point_visually(vec![Value::char('a')]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "args-out-of-range"),
        other => panic!("expected args-out-of-range, got {:?}", other),
    }

    let err = builtin_move_point_visually(vec![Value::symbol("left")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_lookup_image_map() {
    crate::test_utils::init_test_tracing();
    let result = builtin_lookup_image_map(vec![
        Value::symbol("map"),
        Value::fixnum(10),
        Value::fixnum(20),
    ])
    .unwrap();
    assert!(result.is_nil());

    let err = builtin_lookup_image_map(vec![
        Value::symbol("image"),
        Value::string("x"),
        Value::symbol("y"),
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let err = builtin_lookup_image_map(vec![
        Value::symbol("image"),
        Value::fixnum(1),
        Value::symbol("y"),
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let result =
        builtin_lookup_image_map(vec![Value::NIL, Value::fixnum(1), Value::string("y")]).unwrap();
    assert!(result.is_nil());

    let err = builtin_lookup_image_map(vec![]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {:?}", other),
    }
}

#[test]
fn test_current_bidi_paragraph_direction() {
    crate::test_utils::init_test_tracing();
    let result = builtin_current_bidi_paragraph_direction(vec![]).unwrap();
    assert_eq!(result, Value::symbol("left-to-right"));

    let result = builtin_current_bidi_paragraph_direction(vec![Value::make_buffer(
        crate::buffer::BufferId(1),
    )])
    .unwrap();
    assert_eq!(result, Value::symbol("left-to-right"));

    let err = builtin_current_bidi_paragraph_direction(vec![Value::symbol("buffer")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_bidi_resolved_levels() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_bidi_resolved_levels(vec![]).unwrap().is_nil());
    assert!(
        builtin_bidi_resolved_levels(vec![Value::NIL])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_bidi_resolved_levels(vec![Value::fixnum(0)])
            .unwrap()
            .is_nil()
    );

    let err = builtin_bidi_resolved_levels(vec![Value::T]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("fixnump"), Value::T]);
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_bidi_find_overridden_directionality() {
    crate::test_utils::init_test_tracing();
    assert!(
        builtin_bidi_find_overridden_directionality(vec![
            Value::string("abc"),
            Value::fixnum(0),
            Value::string("x"),
        ])
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_bidi_find_overridden_directionality(vec![
            Value::NIL,
            Value::fixnum(0),
            Value::string("x"),
        ])
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_bidi_find_overridden_directionality(vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::NIL,
        ])
        .unwrap()
        .is_nil()
    );

    let third_arg_err = builtin_bidi_find_overridden_directionality(vec![
        Value::string("abc"),
        Value::fixnum(0),
        Value::fixnum(3),
    ])
    .unwrap_err();
    match third_arg_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(3)]);
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let region_arg_err =
        builtin_bidi_find_overridden_directionality(vec![Value::NIL, Value::fixnum(2), Value::NIL])
            .unwrap_err();
    match region_arg_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("integer-or-marker-p"), Value::NIL]
            );
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_move_to_window_line() {
    crate::test_utils::init_test_tracing();
    // Without a selected frame, move-to-window-line should signal an error.
    let mut ev = crate::emacs_core::Context::new();
    for arg in [Value::fixnum(1), Value::fixnum(0), Value::symbol("left")] {
        let err = builtin_move_to_window_line(&mut ev, vec![arg]).unwrap_err();
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "error");
            }
            other => panic!("expected error signal, got {:?}", other),
        }
    }
}

#[test]
fn test_tool_bar_height() {
    crate::test_utils::init_test_tracing();
    let result = builtin_tool_bar_height(vec![]).unwrap();
    assert_eq!(result, Value::fixnum(0));

    let result = builtin_tool_bar_height(vec![Value::symbol("frame")]).unwrap();
    assert_eq!(result, Value::fixnum(0));
}

#[test]
fn test_tool_bar_height_eval_frame_validation() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-test", 80, 24, buf_id);

    {
        let frame = eval
            .frames
            .get_mut(frame_id)
            .expect("xdisp test frame should exist");
        frame.char_height = 17.0;
        frame.set_parameter(Value::symbol("tool-bar-lines"), Value::fixnum(2));
        frame.sync_tool_bar_height_from_parameters();
    }

    let result =
        builtin_tool_bar_height_ctx(&mut eval, vec![Value::fixnum(frame_id.0 as i64)]).unwrap();
    assert_eq!(result, Value::fixnum(2));

    let pixelwise =
        builtin_tool_bar_height_ctx(&mut eval, vec![Value::fixnum(frame_id.0 as i64), Value::T])
            .unwrap();
    assert_eq!(pixelwise, Value::fixnum(34));

    let err = builtin_tool_bar_height_ctx(&mut eval, vec![Value::string("x")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_tab_bar_height() {
    crate::test_utils::init_test_tracing();
    let result = builtin_tab_bar_height(vec![]).unwrap();
    assert_eq!(result, Value::fixnum(0));

    let result = builtin_tab_bar_height(vec![Value::symbol("frame")]).unwrap();
    assert_eq!(result, Value::fixnum(0));
}

#[test]
fn test_tab_bar_height_eval_frame_validation() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-test", 80, 24, buf_id);

    let result =
        builtin_tab_bar_height_ctx(&mut eval, vec![Value::fixnum(frame_id.0 as i64)]).unwrap();
    assert_eq!(result, Value::fixnum(0));

    let err = builtin_tab_bar_height_ctx(&mut eval, vec![Value::string("x")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_tab_bar_height_eval_reflects_tab_bar_lines_and_pixels() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
    let frame_id = super::super::window_cmds::ensure_selected_frame_id(&mut eval);
    {
        let frame = eval.frames.get_mut(frame_id).expect("selected frame");
        frame.char_height = 20.0;
    }
    super::super::window_cmds::builtin_modify_frame_parameters(
        &mut eval,
        vec![
            Value::fixnum(frame_id.0 as i64),
            Value::list(vec![Value::cons(
                Value::symbol("tab-bar-lines"),
                Value::fixnum(1),
            )]),
        ],
    )
    .unwrap();

    let lines =
        builtin_tab_bar_height_ctx(&mut eval, vec![Value::fixnum(frame_id.0 as i64)]).unwrap();
    assert_eq!(lines, Value::fixnum(1));

    let pixels =
        builtin_tab_bar_height_ctx(&mut eval, vec![Value::fixnum(frame_id.0 as i64), Value::T])
            .unwrap();
    assert_eq!(pixels, Value::fixnum(20));

    let frame = eval.frames.get(frame_id).expect("selected frame");
    assert_eq!(frame.tab_bar_height, 20);
}

#[test]
fn test_line_number_display_width() {
    crate::test_utils::init_test_tracing();
    let result = builtin_line_number_display_width(vec![]).unwrap();
    assert_eq!(result, Value::fixnum(0));

    let result = builtin_line_number_display_width(vec![Value::T]).unwrap();
    assert_eq!(result, Value::fixnum(0));
}

#[test]
fn test_long_line_optimizations_p() {
    crate::test_utils::init_test_tracing();
    let result = builtin_long_line_optimizations_p(vec![]).unwrap();
    assert!(result.is_nil());
}

// Test wrong arity errors
#[test]
fn test_wrong_arity() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_line_pixel_height(vec![Value::fixnum(1)]).is_err());
    assert!(builtin_invisible_p(vec![]).is_err());
    assert!(builtin_move_point_visually(vec![]).is_err());
    assert!(builtin_lookup_image_map(vec![Value::fixnum(1), Value::fixnum(2)]).is_err());
    {
        let mut ev = crate::emacs_core::Context::new();
        assert!(builtin_move_to_window_line(&mut ev, vec![]).is_err());
    }
}

// Test optional args
#[test]
fn test_optional_args() {
    crate::test_utils::init_test_tracing();
    // format-mode-line allows 1-4 args
    assert!(builtin_format_mode_line(vec![]).is_err());
    assert!(builtin_format_mode_line(vec![Value::string("fmt")]).is_ok());
    assert!(
        builtin_format_mode_line(vec![
            Value::string("fmt"),
            Value::symbol("face"),
            Value::NIL,
            Value::NIL,
        ])
        .is_ok()
    );
    assert!(
        builtin_format_mode_line(vec![
            Value::string("fmt"),
            Value::symbol("face"),
            Value::symbol("window"),
            Value::symbol("buffer"),
            Value::symbol("extra"),
        ])
        .is_err()
    );

    // window-text-pixel-size allows 0-7 args
    assert!(builtin_window_text_pixel_size(vec![]).is_ok());
    assert!(
        builtin_window_text_pixel_size(vec![
            Value::NIL,
            Value::fixnum(1),
            Value::fixnum(100),
            Value::fixnum(500),
            Value::fixnum(300),
            Value::symbol("mode"),
            Value::symbol("pixelwise"),
        ])
        .is_ok()
    );
    assert!(builtin_window_text_pixel_size(vec![Value::fixnum(1); 8]).is_err());
}
