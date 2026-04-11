use super::*;
use neovm_core::buffer::BufferManager;
use neovm_core::emacs_core::value::Value;
use neovm_core::window::{FrameManager, Rect as NeoRect, WindowId};

fn eval_lisp(eval: &mut neovm_core::emacs_core::Context, source: &str) -> Value {
    eval.eval_str(source).expect("evaluate form")
}

/// Create a minimal Context-like test fixture (FrameManager + BufferManager)
/// and verify `collect_layout_params` produces correct output.
#[test]
fn test_collect_layout_params_basic() {
    let mut evaluator = neovm_core::emacs_core::Context::new();

    // Create a buffer.
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*test*");

    // Create a frame with that buffer.
    let frame_id = evaluator
        .frame_manager_mut()
        .create_frame("test-frame", 800, 600, buf_id);

    // Set some frame font metrics.
    if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
        frame.font_pixel_size = 14.0;
        frame.char_width = 7.0;
        frame.char_height = 14.0;
    }

    let (fp, wps) = collect_layout_params(&evaluator, frame_id, None)
        .expect("collect_layout_params should succeed");

    // Check FrameParams.
    assert_eq!(fp.width, 800.0);
    assert_eq!(fp.height, 600.0);
    assert_eq!(fp.char_width, 7.0);
    assert_eq!(fp.char_height, 14.0);
    assert_eq!(fp.font_pixel_size, 14.0);

    // Should have 1 root leaf + 1 minibuffer = 2 windows.
    assert_eq!(wps.len(), 2, "expected root leaf + minibuffer");

    // First window: root leaf (not minibuffer).
    let root_wp = &wps[0];
    assert!(!root_wp.is_minibuffer);
    assert!(root_wp.selected); // first window is selected by default
    assert_eq!(root_wp.char_width, 7.0);
    assert_eq!(root_wp.char_height, 14.0);
    assert_eq!(root_wp.mode_line_height, 16.0); // mode-line includes face box pixels

    // Second window: minibuffer.
    let mini_wp = &wps[1];
    assert!(mini_wp.is_minibuffer);
    assert!(!mini_wp.selected);
    assert_eq!(mini_wp.mode_line_height, 0.0); // minibuffer has no mode-line
}

#[test]
fn test_frame_params_from_neovm() {
    let _runtime = neovm_core::emacs_core::Context::new();

    let mut buf_mgr = BufferManager::new();
    let buf_id = buf_mgr.create_buffer("*scratch*");
    let mut frame_mgr = FrameManager::new();
    let fid = frame_mgr.create_frame("test", 1024, 768, buf_id);
    let frame = frame_mgr.get(fid).unwrap();

    let face_table = FaceTable::new();
    let fp = frame_params_from_neovm(frame, &face_table);
    assert_eq!(fp.width, 1024.0);
    assert_eq!(fp.height, 768.0);
    assert_eq!(fp.tab_bar_height, 0.0);
}

#[test]
fn chrome_face_pixel_height_uses_ceil_for_fractional_metrics() {
    let mut face = ResolvedFace::default();
    face.font_line_height = 17.2;
    face.box_type = 1;
    face.box_line_width = 1;

    assert_eq!(chrome_face_pixel_height(&face, 14.1), 20.0);

    face.font_line_height = 0.0;
    assert_eq!(chrome_face_pixel_height(&face, 14.1), 17.0);
}

#[test]
fn chrome_face_pixel_height_never_shrinks_below_frame_line_height() {
    let mut face = ResolvedFace::default();
    face.font_line_height = 12.0;

    assert_eq!(chrome_face_pixel_height(&face, 14.1), 15.0);
}

#[test]
fn test_window_params_from_neovm_internal_returns_none() {
    use neovm_core::window::SplitDirection;

    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*test*");
    let frame_id = evaluator
        .frame_manager_mut()
        .create_frame("test", 800, 600, buf_id);
    let internal = Window::Internal {
        id: WindowId(99),
        direction: SplitDirection::Vertical,
        children: vec![],
        bounds: NeoRect::new(0.0, 0.0, 100.0, 100.0),
        parameters: Vec::new(),
        combination_limit: false,
        new_pixel: None,
        new_total: None,
        new_normal: Value::NIL,
        normal_lines: Value::NIL,
        normal_cols: Value::NIL,
    };
    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let frame = evaluator.frame_manager().get(frame_id).unwrap();

    let result = window_params_from_neovm(
        &internal,
        &buf,
        frame,
        evaluator.obarray(),
        evaluator.face_table(),
        None,
        false,
        false,
        Value::T,
    );
    assert!(result.is_none(), "Internal windows should return None");
}

#[test]
fn window_params_from_neovm_uses_default_header_line_and_tab_line_values() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*test*");
    let frame_id = evaluator
        .frame_manager_mut()
        .create_frame("test", 800, 600, buf_id);

    evaluator
        .obarray_mut()
        .set_symbol_value("header-line-format", Value::string("Header sample"));
    evaluator
        .obarray_mut()
        .set_symbol_value("tab-line-format", Value::string("Tab sample"));

    let frame = evaluator.frame_manager().get(frame_id).unwrap();
    let buffer = evaluator.buffer_manager().get(buf_id).unwrap();
    let window = frame.root_window.find(frame.selected_window).unwrap();

    let params = window_params_from_neovm(
        window,
        buffer,
        frame,
        evaluator.obarray(),
        evaluator.face_table(),
        None,
        true,
        false,
        Value::T,
    )
    .expect("leaf window params");

    assert!(params.header_line_height > 0.0);
    assert!(params.tab_line_height > 0.0);
}

#[test]
fn test_window_params_nonselected_reads_window_point() {
    // For NON-selected windows, `params.point` comes from
    // `Window::point` (the snapshotted pointm marker), NOT
    // `buffer.pt_char`. Mirrors GNU `window.c:window_point`:
    //
    //   return (w == XWINDOW (selected_window)
    //           ? BUF_PT (XBUFFER (w->contents))
    //           : XMARKER (w->pointm)->charpos);
    //
    // The selected-window branch is exercised elsewhere; this
    // test specifically verifies the non-selected branch so a
    // future refactor of `window_params_from_neovm` can't
    // silently collapse both branches to read from the buffer.
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*test*");
    {
        let buf = evaluator.buffer_manager_mut().get_mut(buf_id).unwrap();
        buf.insert("abcdef");
        buf.goto_byte(0);
    }
    let frame_id = evaluator
        .frame_manager_mut()
        .create_frame("test", 800, 600, buf_id);
    let selected_window = evaluator
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = evaluator
            .frame_manager_mut()
            .get_mut(frame_id)
            .expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let Window::Leaf { point, .. } = window {
            *point = 5;
        } else {
            panic!("expected leaf window");
        }
    }

    let frame = evaluator.frame_manager().get(frame_id).expect("frame");
    let buffer = evaluator.buffer_manager().get(buf_id).expect("buffer");
    // Pass `is_selected = false` to exercise the non-selected
    // branch of window_params_from_neovm. We're testing the
    // window_point_not_buffer_point rule for *this* branch.
    let params = window_params_from_neovm(
        frame.find_window(selected_window).expect("selected window"),
        buffer,
        frame,
        evaluator.obarray(),
        evaluator.face_table(),
        None,
        false, // is_selected
        false,
        Value::T,
    )
    .expect("window params");

    // Window::point = 5 (1-based); params.point is 0-based, so 4.
    // buffer.pt_char = 0 (we called goto_byte(0)). The non-selected
    // branch must NOT use the buffer's point.
    assert_ne!(buffer.point_char() as i64, params.point);
    assert_eq!(params.point, 4);
}

#[test]
fn test_effective_cursor_spec_prefers_window_cursor_type() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*cursor*");
    let frame_id = evaluator
        .frame_manager_mut()
        .create_frame("test", 800, 600, buf_id);
    let frame = evaluator.frame_manager().get(frame_id).unwrap();
    let buffer = evaluator.buffer_manager().get(buf_id).unwrap();

    let spec = effective_cursor_spec(
        frame,
        buffer,
        true,
        false,
        Value::cons(Value::symbol("bar"), Value::fixnum(5)),
    )
    .unwrap();

    assert_eq!(
        spec.cursor_kind,
        neomacs_display_protocol::frame_glyphs::CursorKind::Bar
    );
    assert_eq!(spec.bar_width, 5);
}

#[test]
fn test_effective_cursor_spec_nonselected_box_becomes_hollow() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*cursor*");
    let frame_id = evaluator
        .frame_manager_mut()
        .create_frame("test", 800, 600, buf_id);
    let frame = evaluator.frame_manager().get(frame_id).unwrap();
    let buffer = evaluator.buffer_manager().get(buf_id).unwrap();

    let spec = effective_cursor_spec(frame, buffer, false, false, Value::T).unwrap();

    assert_eq!(
        spec.cursor_kind,
        neomacs_display_protocol::frame_glyphs::CursorKind::HollowBox
    );
}

#[test]
fn test_frame_cursor_color_uses_cursor_face_background() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator
        .buffer_manager_mut()
        .create_buffer("*cursor-color*");
    let frame_id = evaluator
        .frame_manager_mut()
        .create_frame("test", 800, 600, buf_id);
    let frame = evaluator.frame_manager().get(frame_id).unwrap();

    let cursor_color = frame_cursor_color_pixel(frame, evaluator.face_table());
    let expected = evaluator
        .face_table()
        .resolve("cursor")
        .background
        .map(|color| color_to_pixel(&color))
        .unwrap();

    assert_eq!(cursor_color, expected);
}

#[test]
fn test_window_params_buffer_locals() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*locals*");

    // Set buffer-local variables.
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.set_buffer_local("truncate-lines", Value::T);
        buf.set_buffer_local("tab-width", Value::fixnum(4));
        buf.set_buffer_local("word-wrap", Value::NIL);
    }

    let frame_id = evaluator
        .frame_manager_mut()
        .create_frame("test", 800, 600, buf_id);

    let (_, wps) = collect_layout_params(&evaluator, frame_id, None).unwrap();

    // The root window should pick up the buffer-local vars.
    let wp = &wps[0];
    assert!(wp.truncate_lines);
    assert!(!wp.word_wrap);
    assert_eq!(wp.tab_width, 4);
}

#[test]
fn test_window_params_fringes_and_margins() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*fringe*");
    let frame_id = evaluator
        .frame_manager_mut()
        .create_frame("test", 800, 600, buf_id);

    // Set fringes and margins on the root window.
    if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
        frame.char_width = 8.0;
        if let Some(win) = frame.selected_window_mut() {
            if let Window::Leaf {
                display, margins, ..
            } = win
            {
                *margins = (2, 3);
                display.left_fringe_width = 10;
                display.right_fringe_width = 12;
            }
        }
    }

    let (_, wps) = collect_layout_params(&evaluator, frame_id, None).unwrap();
    let wp = &wps[0];

    assert_eq!(wp.left_fringe_width, 10.0);
    assert_eq!(wp.right_fringe_width, 12.0);
    assert_eq!(wp.left_margin_width, 16.0); // 2 * 8.0
    assert_eq!(wp.right_margin_width, 24.0); // 3 * 8.0

    // text_bounds should be narrower by fringes + margins.
    let expected_text_x = wp.bounds.x + 10.0 + 16.0;
    assert!((wp.text_bounds.x - expected_text_x).abs() < 0.01);
}

#[test]
fn test_collect_nonexistent_frame() {
    let evaluator = neovm_core::emacs_core::Context::new();
    let result = collect_layout_params(&evaluator, FrameId(999999), None);
    assert!(result.is_none());
}

// -----------------------------------------------------------------------
// RustBufferAccess tests
// -----------------------------------------------------------------------

#[test]
fn test_rust_buffer_access_copy_text() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*test-copy*");
    // Insert some text
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "Hello, world!");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustBufferAccess::new(buf);

    let mut out = Vec::new();
    access.copy_text(0, 5, &mut out);
    assert_eq!(&out, b"Hello");

    access.copy_text(7, 13, &mut out);
    assert_eq!(&out, b"world!");
}

#[test]
fn test_rust_buffer_access_charpos_to_bytepos() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*test-pos*");
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "abc");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustBufferAccess::new(buf);

    assert_eq!(access.charpos_to_bytepos(0), 0);
    assert_eq!(access.charpos_to_bytepos(1), 1);
    assert_eq!(access.charpos_to_bytepos(2), 2);
    assert_eq!(access.charpos_to_bytepos(3), 3);
    assert_eq!(access.charpos_to_bytepos(4), 3);
}

#[test]
fn test_rust_buffer_access_lisp_charpos_to_bytepos() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator
        .buffer_manager_mut()
        .create_buffer("*test-lisp-pos*");
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "abc");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustBufferAccess::new(buf);

    assert_eq!(access.lisp_charpos_to_bytepos(0), 0);
    assert_eq!(access.lisp_charpos_to_bytepos(1), 0);
    assert_eq!(access.lisp_charpos_to_bytepos(2), 1);
    assert_eq!(access.lisp_charpos_to_bytepos(3), 2);
    assert_eq!(access.lisp_charpos_to_bytepos(4), 3);
}

#[test]
fn test_rust_buffer_access_count_lines() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*test-lines*");
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "line1\nline2\nline3");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustBufferAccess::new(buf);

    assert_eq!(access.count_lines(0, 17), 2); // 2 newlines
    assert_eq!(access.count_lines(0, 6), 1); // 1 newline in "line1\n"
    assert_eq!(access.count_lines(0, 5), 0); // no newline in "line1"
}

#[test]
fn test_rust_buffer_access_metadata() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*meta*");
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "content");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
        buf.modified = true;
        buf.set_file_name_value(Some("/tmp/test.el".to_string()));
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustBufferAccess::new(buf);

    assert_eq!(access.name(), "*meta*");
    assert!(access.modified());
    assert_eq!(access.file_name(), Some("/tmp/test.el"));
    assert_eq!(access.zv(), 7);
}

// -----------------------------------------------------------------------
// RustTextPropAccess tests
// -----------------------------------------------------------------------

#[test]
fn test_text_prop_check_invisible() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*invis*");
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "visible hidden visible");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
        // Mark "hidden" (positions 8..14) as invisible
        buf.text
            .text_props_put_property(8, 14, "invisible", Value::T);
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustTextPropAccess::new(buf);

    // Position 0: not invisible
    let (invis, _next) = access.check_invisible(0);
    assert!(!invis);

    // Position 8: invisible
    let (invis, _next) = access.check_invisible(8);
    assert!(invis);

    // Position 14: visible again
    let (invis, _next) = access.check_invisible(14);
    assert!(!invis);
}

#[test]
fn test_text_prop_check_display() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*display*");
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "abcdef");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
        // Set a display property on positions 2..4
        buf.text
            .text_props_put_property(2, 4, "display", Value::fixnum(42));
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustTextPropAccess::new(buf);

    // Position 0: no display prop
    let (dp, _next) = access.check_display_prop(0);
    assert!(dp.is_none());

    // Position 2: has display prop
    let (dp, _next) = access.check_display_prop(2);
    assert!(dp.is_some());
    assert_eq!(dp.and_then(Value::as_fixnum), Some(42));
}

#[test]
fn test_text_prop_line_spacing() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*spacing*");
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "line1\nline2");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
        // Set line-spacing on "line2" area
        buf.text
            .text_props_put_property(6, 11, "line-spacing", Value::fixnum(4));
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustTextPropAccess::new(buf);

    // Position 0: no line-spacing
    assert_eq!(access.check_line_spacing(0, 16.0), 0.0);

    // Position 6: line-spacing = 4
    assert_eq!(access.check_line_spacing(6, 16.0), 4.0);
}

#[test]
fn test_text_prop_next_change() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*next*");
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "aabbcc");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
        buf.text.text_props_put_property(2, 4, "face", Value::T);
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustTextPropAccess::new(buf);

    // At position 0, next change should be at 2 (where face starts)
    let next = access.next_property_change(0);
    assert_eq!(next, 2);

    // At position 2, next change should be at 4 (where face ends)
    let next = access.next_property_change(2);
    assert_eq!(next, 4);
}

#[test]
fn test_text_prop_get_property() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*prop*");
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "test");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
        buf.text
            .text_props_put_property(0, 4, "face", Value::fixnum(5));
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustTextPropAccess::new(buf);

    let face = access.get_property(0, "face");
    assert_eq!(face.and_then(Value::as_fixnum), Some(5));

    let none = access.get_property(0, "nonexistent");
    assert!(none.is_none());
}

#[test]
fn test_text_prop_access_multibyte_positions_use_byte_offsets() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let buf_id = evaluator.buffer_manager_mut().create_buffer("*utf8-prop*");
    if let Some(buf) = evaluator.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, "a好b");
        buf.zv = buf.text.len();
        buf.zv_char = buf.text.char_count();
        buf.text
            .text_props_put_property(4, 5, "face", Value::fixnum(9));
    }

    let buf = evaluator.buffer_manager().get(buf_id).unwrap();
    let access = RustTextPropAccess::new(buf);

    let face = access.get_property(2, "face");
    assert_eq!(face.and_then(Value::as_fixnum), Some(9));

    let next = access.next_property_change(1);
    assert_eq!(next, 2);
}

// -----------------------------------------------------------------------
// FaceResolver tests
// -----------------------------------------------------------------------

#[test]
fn test_color_to_pixel() {
    let c = NeoColor::rgb(255, 128, 0);
    assert_eq!(color_to_pixel(&c), 0x00FF8000);

    let black = NeoColor::rgb(0, 0, 0);
    assert_eq!(color_to_pixel(&black), 0x00000000);

    let white = NeoColor::rgb(255, 255, 255);
    assert_eq!(color_to_pixel(&white), 0x00FFFFFF);
}

#[test]
fn test_face_resolver_default() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let table = FaceTable::new();

    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);
    let df = resolver.default_face();

    // The standard "default" face has foreground black (0,0,0) and
    // background white (255,255,255).
    assert_eq!(df.fg, 0x00000000); // black
    assert_eq!(df.bg, 0x00FFFFFF); // white
    assert_eq!(df.font_weight, FontWeight::NORMAL.0); // 400
    assert!(!df.italic);
    assert!(!df.overstrike);
    assert!(!df.extend);
    assert_eq!(df.underline_style, 0);
    assert!(!df.strike_through);
    assert!(!df.overline);
    assert_eq!(df.box_type, 0);
}

#[test]
fn test_face_resolver_with_text_property() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    // Create a buffer and set "face" text property to bold.
    let mut buf =
        neovm_core::buffer::Buffer::new(neovm_core::buffer::BufferId(1), "*test*".to_string());
    buf.text.insert_str(0, "hello world");
    buf.zv = buf.text.len();
    buf.zv_char = buf.text.char_count();
    // Set "face" to the symbol "bold" on positions 0..5.
    buf.text
        .text_props_put_property(0, 5, "face", Value::symbol("bold"));

    let mut next_check = buf.point_max_char();
    let resolved = resolver.face_at_pos(&buf, 0, &mut next_check);

    // Bold face should have weight 700.
    assert_eq!(resolved.font_weight, FontWeight::BOLD.0);
    // next_check should be 5 (where the property changes).
    assert_eq!(next_check, 5);

    // Position 6 should have default weight.
    let mut nc2 = buf.point_max_char();
    let resolved2 = resolver.face_at_pos(&buf, 6, &mut nc2);
    assert_eq!(resolved2.font_weight, FontWeight::NORMAL.0);
}

#[test]
fn test_face_resolver_with_font_lock_face() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    let mut buf =
        neovm_core::buffer::Buffer::new(neovm_core::buffer::BufferId(2), "*fontlock*".to_string());
    buf.text.insert_str(0, "defun myfunction");
    buf.zv = buf.text.len();
    buf.zv_char = buf.text.char_count();
    // Set "font-lock-face" to "font-lock-keyword-face" on "defun".
    buf.text.text_props_put_property(
        0,
        5,
        "font-lock-face",
        Value::symbol("font-lock-keyword-face"),
    );

    let mut next_check = buf.point_max_char();
    let resolved = resolver.face_at_pos(&buf, 2, &mut next_check);

    // font-lock-keyword-face has foreground purple (128, 0, 128).
    let expected_fg = color_to_pixel(&NeoColor::rgb(128, 0, 128));
    assert_eq!(resolved.fg, expected_fg);
}

#[test]
fn test_face_resolver_next_check() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    let mut buf =
        neovm_core::buffer::Buffer::new(neovm_core::buffer::BufferId(3), "*nextcheck*".to_string());
    buf.text.insert_str(0, "aabbccdd");
    buf.zv = buf.text.len();
    buf.zv_char = buf.text.char_count();
    // Face property on [2, 4)
    buf.text
        .text_props_put_property(2, 4, "face", Value::symbol("bold"));
    // Another property on [4, 6)
    buf.text
        .text_props_put_property(4, 6, "face", Value::symbol("italic"));

    // At position 0, next_check should be 2 (first property boundary).
    let mut nc = buf.point_max_char();
    let _ = resolver.face_at_pos(&buf, 0, &mut nc);
    assert_eq!(nc, 2);

    // At position 2, next_check should be 4 (end of bold range).
    let mut nc = buf.point_max_char();
    let _ = resolver.face_at_pos(&buf, 2, &mut nc);
    assert_eq!(nc, 4);

    // At position 4, next_check should be 6 (end of italic range).
    let mut nc = buf.point_max_char();
    let _ = resolver.face_at_pos(&buf, 4, &mut nc);
    assert_eq!(nc, 6);
}

#[test]
fn test_face_resolver_overlay_face() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    {
        let buf = evaluator
            .buffer_manager_mut()
            .current_buffer_mut()
            .expect("current buffer");
        buf.insert("overlay text here");
    }

    let _ = eval_lisp(
        &mut evaluator,
        "(let ((ov (make-overlay 1 8))) (overlay-put ov 'face 'bold) ov)",
    );

    let buf = evaluator
        .buffer_manager()
        .current_buffer()
        .expect("current buffer");
    let mut nc = buf.point_max_char();
    let resolved = resolver.face_at_pos(buf, 3, &mut nc);
    assert_eq!(resolved.font_weight, FontWeight::BOLD.0);
    // next_check should be at most 7 (end of overlay).
    assert!(nc <= 7);
}

#[test]
fn test_face_resolver_overlay_priority() {
    let mut evaluator = neovm_core::emacs_core::Context::new();
    let mut table = FaceTable::new();

    // Define two custom faces with different foreground colors.
    let mut face_a = NeoFace::new("face-a");
    face_a.foreground = Some(NeoColor::rgb(255, 0, 0)); // red
    table.define(face_a);

    let mut face_b = NeoFace::new("face-b");
    face_b.foreground = Some(NeoColor::rgb(0, 0, 255)); // blue
    table.define(face_b);

    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    {
        let buf = evaluator
            .buffer_manager_mut()
            .current_buffer_mut()
            .expect("current buffer");
        buf.insert("priority test");
    }

    let _ = eval_lisp(
        &mut evaluator,
        "(let ((a (make-overlay 1 11))
               (b (make-overlay 1 11)))
           (overlay-put a 'face 'face-a)
           (overlay-put a 'priority 10)
           (overlay-put b 'face 'face-b)
           (overlay-put b 'priority 20)
           (list a b))",
    );

    let buf = evaluator
        .buffer_manager()
        .current_buffer()
        .expect("current buffer");
    let mut nc = buf.point_max_char();
    let resolved = resolver.face_at_pos(buf, 5, &mut nc);
    // face-b (blue, priority 20) should override face-a (red, priority 10).
    assert_eq!(resolved.fg, color_to_pixel(&NeoColor::rgb(0, 0, 255)));
}

#[test]
fn test_face_resolver_face_ref_list_respects_gnu_precedence() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let mut table = FaceTable::new();

    let mut face_a = NeoFace::new("face-a");
    face_a.foreground = Some(NeoColor::rgb(255, 0, 0));
    table.define(face_a);

    let mut face_b = NeoFace::new("face-b");
    face_b.foreground = Some(NeoColor::rgb(0, 0, 255));
    table.define(face_b);

    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    let mut buf = neovm_core::buffer::Buffer::new(
        neovm_core::buffer::BufferId(51),
        "*face-ref-list*".to_string(),
    );
    buf.text.insert_str(0, "x");
    buf.zv = buf.text.len();
    buf.zv_char = buf.text.char_count();
    buf.text.text_props_put_property(
        0,
        1,
        "face",
        Value::list(vec![Value::symbol("face-a"), Value::symbol("face-b")]),
    );

    let mut next_check = buf.point_max_char();
    let resolved = resolver.face_at_pos(&buf, 0, &mut next_check);
    assert_eq!(resolved.fg, color_to_pixel(&NeoColor::rgb(255, 0, 0)));
}

#[test]
fn test_face_resolver_buffer_local_default_remap_applies_to_plain_text() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    let mut buf = neovm_core::buffer::Buffer::new(
        neovm_core::buffer::BufferId(52),
        "*default-remap*".to_string(),
    );
    buf.text.insert_str(0, "plain");
    buf.zv = buf.text.len();
    buf.zv_char = buf.text.char_count();
    buf.set_buffer_local(
        "face-remapping-alist",
        Value::list(vec![Value::list(vec![
            Value::symbol("default"),
            Value::list(vec![Value::keyword("foreground"), Value::string("#009acd")]),
            Value::symbol("default"),
        ])]),
    );

    let mut next_check = buf.point_max_char();
    let resolved = resolver.face_at_pos(&buf, 0, &mut next_check);
    assert_eq!(resolved.fg, color_to_pixel(&NeoColor::rgb(0, 154, 205)));
}

#[test]
fn test_face_resolver_buffer_local_named_face_remap_applies_to_face_prop() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    let mut buf = neovm_core::buffer::Buffer::new(
        neovm_core::buffer::BufferId(53),
        "*named-remap*".to_string(),
    );
    buf.text.insert_str(0, "bold");
    buf.zv = buf.text.len();
    buf.zv_char = buf.text.char_count();
    buf.set_buffer_local(
        "face-remapping-alist",
        Value::list(vec![Value::list(vec![
            Value::symbol("bold"),
            Value::list(vec![Value::keyword("foreground"), Value::string("#ff4500")]),
            Value::symbol("bold"),
        ])]),
    );
    buf.text
        .text_props_put_property(0, 4, "face", Value::symbol("bold"));

    let mut next_check = buf.point_max_char();
    let resolved = resolver.face_at_pos(&buf, 0, &mut next_check);
    assert_eq!(resolved.font_weight, FontWeight::BOLD.0);
    assert_eq!(resolved.fg, color_to_pixel(&NeoColor::rgb(255, 69, 0)));
}

#[test]
fn test_face_resolver_inverse_video() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let mut table = FaceTable::new();

    let mut inv_face = NeoFace::new("inverse-test");
    inv_face.foreground = Some(NeoColor::rgb(255, 255, 255)); // white
    inv_face.background = Some(NeoColor::rgb(0, 0, 0)); // black
    inv_face.inverse_video = Some(true);
    table.define(inv_face);

    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    let mut buf =
        neovm_core::buffer::Buffer::new(neovm_core::buffer::BufferId(6), "*inverse*".to_string());
    buf.text.insert_str(0, "inverted");
    buf.zv = buf.text.len();
    buf.zv_char = buf.text.char_count();
    buf.text
        .text_props_put_property(0, 8, "face", Value::symbol("inverse-test"));

    let mut nc = buf.point_max_char();
    let resolved = resolver.face_at_pos(&buf, 0, &mut nc);
    // Inverse: fg and bg should be swapped.
    assert_eq!(resolved.fg, 0x00000000); // was white, now black
    assert_eq!(resolved.bg, 0x00FFFFFF); // was black, now white
}

#[test]
fn test_face_resolver_multibyte_text_property_uses_byte_offsets() {
    let _evaluator = neovm_core::emacs_core::Context::new();

    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    let mut buf =
        neovm_core::buffer::Buffer::new(neovm_core::buffer::BufferId(7), "*utf8*".to_string());
    buf.text.insert_str(0, "a好b");
    buf.zv = buf.text.len();
    buf.zv_char = buf.text.char_count();
    buf.text
        .text_props_put_property(4, 5, "face", Value::symbol("bold"));

    let mut next_check = buf.point_max_char();
    let resolved = resolver.face_at_pos(&buf, 2, &mut next_check);

    assert_eq!(resolved.font_weight, FontWeight::BOLD.0);
    assert_eq!(next_check, 3);
}

#[test]
fn test_face_resolver_multibyte_overlay_uses_byte_offsets() {
    let mut evaluator = neovm_core::emacs_core::Context::new();

    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    {
        let buf = evaluator
            .buffer_manager_mut()
            .current_buffer_mut()
            .expect("current buffer");
        buf.insert("a好b");
    }
    let _ = eval_lisp(
        &mut evaluator,
        "(let ((ov (make-overlay 3 4))) (overlay-put ov 'face 'bold) ov)",
    );

    let buf = evaluator
        .buffer_manager()
        .current_buffer()
        .expect("current buffer");
    let mut next_check = buf.point_max_char();
    let resolved = resolver.face_at_pos(buf, 2, &mut next_check);

    assert_eq!(resolved.font_weight, FontWeight::BOLD.0);
    assert_eq!(next_check, 3);
}

#[test]
fn test_resolve_face_value_symbol() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let names = FaceResolver::resolve_face_value(&Value::symbol("bold"));
    assert_eq!(names, vec!["bold"]);
}

#[test]
fn test_resolve_face_value_nil() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let names = FaceResolver::resolve_face_value(&Value::NIL);
    assert!(names.is_empty());
}

#[test]
fn test_resolve_face_value_list() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let list = Value::list(vec![Value::symbol("bold"), Value::symbol("italic")]);
    let names = FaceResolver::resolve_face_value(&list);
    assert_eq!(names, vec!["bold", "italic"]);
}

#[test]
fn test_realize_face_height_absolute() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    let mut face = NeoFace::new("tall");
    face.height = Some(FaceHeight::Absolute(240)); // 24pt
    let realized = resolver.realize_face(&face);
    let expected = crate::fontconfig::face_height_to_pixels(240);
    assert!((realized.font_size - expected).abs() < 0.1);
}

#[test]
fn test_realize_face_height_relative() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 14.0);

    let mut face = NeoFace::new("scaled");
    face.height = Some(FaceHeight::Relative(2.0));
    let realized = resolver.realize_face(&face);
    // 2.0 * default_font_size
    let expected = resolver.default_face().font_size * 2.0;
    assert!((realized.font_size - expected).abs() < 0.1);
}

#[test]
fn test_face_from_plist_realizes_relative_height_family_and_weight() {
    let _evaluator = neovm_core::emacs_core::Context::new();
    let table = FaceTable::new();
    let resolver = FaceResolver::new(&table, 0x00FFFFFF, 0x00000000, 26.666666);

    let plist = Value::list(vec![
        Value::keyword("family"),
        Value::string("DejaVu Sans Mono"),
        Value::keyword("height"),
        Value::make_float(1.6),
        Value::keyword("weight"),
        Value::symbol("extra-bold"),
    ]);

    let inline_face = FaceResolver::face_from_plist(&plist).expect("inline plist face");
    let realized = resolver.realize_face(&inline_face);

    assert_eq!(realized.font_family, "DejaVu Sans Mono");
    assert_eq!(realized.font_weight, FontWeight::EXTRA_BOLD.0);
    assert!(
        (realized.font_size - (resolver.default_face().font_size * 1.6)).abs() < 0.1,
        "expected relative face height to scale from the default face size, got {}",
        realized.font_size
    );
}
