use super::*;

#[test]
fn test_format_mode_line() {
    let result =
        builtin_format_mode_line(vec![Value::string("test"), Value::symbol("default")]).unwrap();
    assert_eq!(result, Value::string(""));

    let result = builtin_format_mode_line(vec![
        Value::string("test"),
        Value::symbol("default"),
        Value::Nil,
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
        Value::Nil,
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
    let mut eval = super::super::eval::Evaluator::new();
    let buffer_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-format", 80, 24, buffer_id);
    let window_id = eval.frames.get(frame_id).expect("frame").selected_window.0 as i64;

    let ok = builtin_format_mode_line_eval(
        &mut eval,
        vec![
            Value::string("%b"),
            Value::Nil,
            Value::Int(window_id),
            Value::Buffer(buffer_id),
        ],
    )
    .unwrap();
    // %b expands to the current buffer name
    let buf_name = eval
        .buffers
        .current_buffer()
        .map(|b| b.name.as_str())
        .unwrap_or("");
    assert_eq!(ok, Value::string(buf_name));

    let err = builtin_format_mode_line_eval(
        &mut eval,
        vec![Value::string("%b"), Value::Nil, Value::string("x")],
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let err = builtin_format_mode_line_eval(
        &mut eval,
        vec![
            Value::string("%b"),
            Value::Nil,
            Value::Nil,
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
fn test_format_mode_line_eval_uses_explicit_buffer_instead_of_current_buffer() {
    let mut eval = super::super::eval::Evaluator::new();
    let saved_current = eval.buffers.current_buffer_id().expect("current buffer");
    let other_id = eval.buffers.create_buffer("*other*");

    let ok = builtin_format_mode_line_eval(
        &mut eval,
        vec![
            Value::string("%b"),
            Value::Nil,
            Value::Nil,
            Value::Buffer(other_id),
        ],
    )
    .unwrap();

    assert_eq!(ok, Value::string("*other*"));
    assert_eq!(eval.buffers.current_buffer_id(), Some(saved_current));
}

#[test]
fn test_format_mode_line_eval_uses_window_buffer_instead_of_current_buffer() {
    let mut eval = super::super::eval::Evaluator::new();
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

    let ok = builtin_format_mode_line_eval(
        &mut eval,
        vec![
            Value::string("%b"),
            Value::Nil,
            Value::Window(window_id as u64),
            Value::Nil,
        ],
    )
    .unwrap();

    assert_eq!(ok, Value::string("*window*"));
    assert_eq!(eval.buffers.current_buffer_id(), Some(saved_current));
}

#[test]
fn test_format_mode_line_in_state_uses_buffer_local_symbols_and_restores_buffer() {
    let mut eval = super::super::eval::Evaluator::new();
    let saved_current = eval.buffers.current_buffer_id().expect("current buffer");
    let other_id = eval.buffers.create_buffer("*mode-line*");
    eval.buffers
        .set_buffer_local_property(other_id, "mode-name", Value::string("Neo"))
        .expect("mode-name local should set");

    let rendered = builtin_format_mode_line_in_state(
        &eval.obarray,
        eval.dynamic.as_slice(),
        &eval.frames,
        &mut eval.buffers,
        vec![
            Value::list(vec![
                Value::string("%b "),
                Value::symbol("mode-name"),
                Value::string(" "),
                Value::symbol("mode-line-front-space"),
            ]),
            Value::Nil,
            Value::Nil,
            Value::Buffer(other_id),
        ],
    )
    .expect("format-mode-line shared state")
    .expect("non-:eval format should stay on shared state");

    assert_eq!(rendered, Value::string("*mode-line* Neo  "));
    assert_eq!(eval.buffers.current_buffer_id(), Some(saved_current));
}

#[test]
fn test_format_mode_line_eval_keeps_shared_buffer_context_around_eval_forms() {
    let mut eval = super::super::eval::Evaluator::new();
    let saved_current = eval.buffers.current_buffer_id().expect("current buffer");
    let other_id = eval.buffers.create_buffer("*mode-line-eval*");
    eval.buffers
        .set_buffer_local_property(other_id, "mode-name", Value::string("Neo"))
        .expect("mode-name local should set");

    let rendered = builtin_format_mode_line_eval(
        &mut eval,
        vec![
            Value::list(vec![
                Value::string("%b "),
                Value::list(vec![Value::symbol(":eval"), Value::symbol("mode-name")]),
            ]),
            Value::Nil,
            Value::Nil,
            Value::Buffer(other_id),
        ],
    )
    .expect("format-mode-line eval");

    assert_eq!(rendered, Value::string("*mode-line-eval* Neo"));
    assert_eq!(eval.buffers.current_buffer_id(), Some(saved_current));
}

#[test]
fn test_invisible_p() {
    let err = builtin_invisible_p(vec![Value::Int(0)]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "args-out-of-range"),
        other => panic!("expected args-out-of-range, got {:?}", other),
    }
    let result = builtin_invisible_p(vec![Value::Int(1)]).unwrap();
    assert!(result.is_nil());

    let result = builtin_invisible_p(vec![Value::symbol("invisible")]).unwrap();
    assert!(result.is_truthy());

    let result = builtin_invisible_p(vec![Value::Int(-1)]).unwrap();
    assert!(result.is_truthy());

    let result = builtin_invisible_p(vec![Value::Nil]).unwrap();
    assert!(result.is_nil());

    let result = builtin_invisible_p(vec![Value::string("x")]).unwrap();
    assert!(result.is_truthy());

    let result = builtin_invisible_p(vec![Value::Float(1.5, next_float_id())]).unwrap();
    assert!(result.is_truthy());
}

#[test]
fn test_line_pixel_height() {
    let result = builtin_line_pixel_height(vec![]).unwrap();
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_window_text_pixel_size() {
    let result = builtin_window_text_pixel_size(vec![]).unwrap();
    if let Value::Cons(cell) = result {
        let pair = read_cons(cell);
        assert_eq!(pair.car, Value::Int(0));
        assert_eq!(pair.cdr, Value::Int(0));
    } else {
        panic!("expected cons");
    }
}

#[test]
fn test_window_text_pixel_size_arg_validation() {
    let err = builtin_window_text_pixel_size(vec![Value::Int(1)]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let err = builtin_window_text_pixel_size(vec![Value::Nil, Value::symbol("x")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let err = builtin_window_text_pixel_size(vec![Value::Nil, Value::Nil, Value::symbol("x")])
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    // X-LIMIT / Y-LIMIT / MODE / PIXELWISE are accepted without strict type checks.
    assert!(
        builtin_window_text_pixel_size(vec![
            Value::Nil,
            Value::Nil,
            Value::Nil,
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
    let mut eval = super::super::eval::Evaluator::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-test", 80, 24, buf_id);
    let selected_window = eval.frames.get(frame_id).expect("frame").selected_window.0 as i64;

    let ok =
        builtin_window_text_pixel_size_eval(&mut eval, vec![Value::Int(selected_window)]).unwrap();
    match ok {
        Value::Cons(_) => {}
        other => panic!("expected cons return, got {other:?}"),
    }

    let err =
        builtin_window_text_pixel_size_eval(&mut eval, vec![Value::Int(999_999)]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_pos_visible_in_window_p() {
    let result = builtin_pos_visible_in_window_p(vec![Value::Int(1)]).unwrap();
    assert!(result.is_nil());

    let result = builtin_pos_visible_in_window_p(vec![Value::Int(100), Value::symbol("window")])
        .unwrap_err();
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data[0], Value::symbol("window-live-p"));
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let result =
        builtin_pos_visible_in_window_p(vec![Value::symbol("left"), Value::Int(1)]).unwrap_err();
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
        builtin_pos_visible_in_window_p(vec![Value::Int(1), Value::Nil, Value::Int(1)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn test_pos_visible_in_window_p_eval_window_validation() {
    let mut eval = super::super::eval::Evaluator::new();
    let err = builtin_pos_visible_in_window_p_eval(&mut eval, vec![Value::Nil, Value::string("x")])
        .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let err =
        builtin_pos_visible_in_window_p_eval(&mut eval, vec![Value::symbol("left"), Value::Int(1)])
            .unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data[0], Value::symbol("window-live-p"));
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let ok = builtin_pos_visible_in_window_p_eval(&mut eval, vec![Value::Int(1)]).unwrap();
    assert!(ok.is_nil());
}

#[test]
fn test_pos_visible_in_window_p_eval_returns_partial_geometry_for_live_window() {
    let mut eval = super::super::eval::Evaluator::new();
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

    let result = builtin_pos_visible_in_window_p_eval(
        &mut eval,
        vec![Value::Int(5), Value::Window(selected_window.0), Value::True],
    )
    .unwrap();
    assert_eq!(super::super::print::print_value(&result), "(0 16)");
}

#[test]
fn test_window_line_height_eval_returns_live_gui_row_metrics() {
    let mut eval = super::super::eval::Evaluator::new();
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

    let current = builtin_window_line_height_eval(
        &mut eval,
        vec![Value::Nil, Value::Window(selected_window.0)],
    )
    .unwrap();
    let last = builtin_window_line_height_eval(
        &mut eval,
        vec![Value::Int(-1), Value::Window(selected_window.0)],
    )
    .unwrap();
    assert_eq!(super::super::print::print_value(&current), "(16 1 16 0)");
    assert_eq!(super::super::print::print_value(&last), "(16 2 32 0)");
}

#[test]
fn test_move_point_visually() {
    for direction in [1_i64, 0, -1, 2] {
        let err = builtin_move_point_visually(vec![Value::Int(direction)]).unwrap_err();
        match err {
            Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "args-out-of-range"),
            other => panic!("expected args-out-of-range, got {:?}", other),
        }
    }

    let err = builtin_move_point_visually(vec![Value::Char('a')]).unwrap_err();
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
    let result =
        builtin_lookup_image_map(vec![Value::symbol("map"), Value::Int(10), Value::Int(20)])
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
        Value::Int(1),
        Value::symbol("y"),
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let result =
        builtin_lookup_image_map(vec![Value::Nil, Value::Int(1), Value::string("y")]).unwrap();
    assert!(result.is_nil());

    let err = builtin_lookup_image_map(vec![]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {:?}", other),
    }
}

#[test]
fn test_current_bidi_paragraph_direction() {
    let result = builtin_current_bidi_paragraph_direction(vec![]).unwrap();
    assert_eq!(result, Value::symbol("left-to-right"));

    let result =
        builtin_current_bidi_paragraph_direction(vec![Value::Buffer(crate::buffer::BufferId(1))])
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
    assert!(builtin_bidi_resolved_levels(vec![]).unwrap().is_nil());
    assert!(
        builtin_bidi_resolved_levels(vec![Value::Nil])
            .unwrap()
            .is_nil()
    );
    assert!(
        builtin_bidi_resolved_levels(vec![Value::Int(0)])
            .unwrap()
            .is_nil()
    );

    let err = builtin_bidi_resolved_levels(vec![Value::True]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("fixnump"), Value::True]);
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_bidi_find_overridden_directionality() {
    assert!(
        builtin_bidi_find_overridden_directionality(vec![
            Value::string("abc"),
            Value::Int(0),
            Value::string("x"),
        ])
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_bidi_find_overridden_directionality(vec![
            Value::Nil,
            Value::Int(0),
            Value::string("x"),
        ])
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_bidi_find_overridden_directionality(
            vec![Value::Int(1), Value::Int(2), Value::Nil,]
        )
        .unwrap()
        .is_nil()
    );

    let third_arg_err = builtin_bidi_find_overridden_directionality(vec![
        Value::string("abc"),
        Value::Int(0),
        Value::Int(3),
    ])
    .unwrap_err();
    match third_arg_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::Int(3)]);
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let region_arg_err =
        builtin_bidi_find_overridden_directionality(vec![Value::Nil, Value::Int(2), Value::Nil])
            .unwrap_err();
    match region_arg_err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("integer-or-marker-p"), Value::Nil]
            );
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_move_to_window_line() {
    for arg in [Value::Int(1), Value::Int(0), Value::symbol("left")] {
        let err = builtin_move_to_window_line(vec![arg]).unwrap_err();
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "error");
                assert_eq!(
                    sig.data,
                    vec![Value::string(
                        "move-to-window-line called from unrelated buffer"
                    )]
                );
            }
            other => panic!("expected error signal, got {:?}", other),
        }
    }
}

#[test]
fn test_tool_bar_height() {
    let result = builtin_tool_bar_height(vec![]).unwrap();
    assert_eq!(result, Value::Int(0));

    let result = builtin_tool_bar_height(vec![Value::symbol("frame")]).unwrap();
    assert_eq!(result, Value::Int(0));
}

#[test]
fn test_tool_bar_height_eval_frame_validation() {
    let mut eval = super::super::eval::Evaluator::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-test", 80, 24, buf_id);

    let result =
        builtin_tool_bar_height_eval(&mut eval, vec![Value::Int(frame_id.0 as i64)]).unwrap();
    assert_eq!(result, Value::Int(0));

    let err = builtin_tool_bar_height_eval(&mut eval, vec![Value::string("x")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_tab_bar_height() {
    let result = builtin_tab_bar_height(vec![]).unwrap();
    assert_eq!(result, Value::Int(0));

    let result = builtin_tab_bar_height(vec![Value::symbol("frame")]).unwrap();
    assert_eq!(result, Value::Int(0));
}

#[test]
fn test_tab_bar_height_eval_frame_validation() {
    let mut eval = super::super::eval::Evaluator::new();
    let buf_id = eval.buffers.current_buffer().expect("current buffer").id;
    let frame_id = eval.frames.create_frame("xdisp-test", 80, 24, buf_id);

    let result =
        builtin_tab_bar_height_eval(&mut eval, vec![Value::Int(frame_id.0 as i64)]).unwrap();
    assert_eq!(result, Value::Int(0));

    let err = builtin_tab_bar_height_eval(&mut eval, vec![Value::string("x")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_line_number_display_width() {
    let result = builtin_line_number_display_width(vec![]).unwrap();
    assert_eq!(result, Value::Int(0));

    let result = builtin_line_number_display_width(vec![Value::True]).unwrap();
    assert_eq!(result, Value::Int(0));
}

#[test]
fn test_long_line_optimizations_p() {
    let result = builtin_long_line_optimizations_p(vec![]).unwrap();
    assert!(result.is_nil());
}

// Test wrong arity errors
#[test]
fn test_wrong_arity() {
    assert!(builtin_line_pixel_height(vec![Value::Int(1)]).is_err());
    assert!(builtin_invisible_p(vec![]).is_err());
    assert!(builtin_move_point_visually(vec![]).is_err());
    assert!(builtin_lookup_image_map(vec![Value::Int(1), Value::Int(2)]).is_err());
    assert!(builtin_move_to_window_line(vec![]).is_err());
}

// Test optional args
#[test]
fn test_optional_args() {
    // format-mode-line allows 1-4 args
    assert!(builtin_format_mode_line(vec![]).is_err());
    assert!(builtin_format_mode_line(vec![Value::string("fmt")]).is_ok());
    assert!(
        builtin_format_mode_line(vec![
            Value::string("fmt"),
            Value::symbol("face"),
            Value::Nil,
            Value::Nil,
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
            Value::Nil,
            Value::Int(1),
            Value::Int(100),
            Value::Int(500),
            Value::Int(300),
            Value::symbol("mode"),
            Value::symbol("pixelwise"),
        ])
        .is_ok()
    );
    assert!(builtin_window_text_pixel_size(vec![Value::Int(1); 8]).is_err());
}
