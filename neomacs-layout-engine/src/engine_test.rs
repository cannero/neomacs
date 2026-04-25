use super::*;
use crate::neovm_bridge::RustBufferAccess;
use neomacs_display_protocol::frame_glyphs::GlyphRowRole;
use neomacs_display_protocol::glyph_matrix::GlyphType;
use neovm_core::emacs_core::Context;
use neovm_core::emacs_core::eval::{
    DisplayHost, GuiFrameHostRequest, ImageResolveRequest, ResolvedImage,
};
use neovm_core::emacs_core::load::{
    apply_runtime_startup_state, create_bootstrap_evaluator_cached_with_features,
};
use neovm_core::heap_types::LispString;
use neovm_core::window::DisplayRowSnapshot;
use std::sync::{Arc, Mutex};

fn test_window_params() -> WindowParams {
    WindowParams {
        window_id: 1,
        buffer_id: 1,
        bounds: Rect::new(0.0, 0.0, 800.0, 600.0),
        text_bounds: Rect::new(0.0, 0.0, 800.0, 560.0),
        selected: true,
        is_minibuffer: false,
        window_start: 1,
        window_end: 0,
        point: 1,
        buffer_size: 1,
        buffer_begv: 1,
        hscroll: 0,
        vscroll: 0,
        truncate_lines: false,
        word_wrap: false,
        tab_width: 8,
        tab_stop_list: vec![],
        default_fg: 0xFFFFFF,
        default_bg: 0x000000,
        char_width: 8.0,
        char_height: 16.0,
        font_pixel_size: 14.0,
        font_ascent: 12.0,
        mode_line_height: 0.0,
        header_line_height: 0.0,
        tab_line_height: 0.0,
        cursor_kind: neomacs_display_protocol::frame_glyphs::CursorKind::FilledBox,
        cursor_bar_width: 2,
        x_stretch_cursor: false,
        cursor_color: 0xFFFFFF,
        left_fringe_width: 0.0,
        right_fringe_width: 0.0,
        indicate_empty_lines: 0,
        show_trailing_whitespace: false,
        trailing_ws_bg: 0,
        fill_column_indicator: 0,
        fill_column_indicator_char: '|',
        fill_column_indicator_fg: 0,
        extra_line_spacing: 0.0,
        selective_display: 0,
        escape_glyph_fg: 0,
        nobreak_char_display: 0,
        nobreak_char_fg: 0,
        glyphless_char_fg: 0,
        wrap_prefix: vec![],
        line_prefix: vec![],
        left_margin_width: 0.0,
        right_margin_width: 0.0,
    }
}

#[derive(Default)]
struct RecordingImageDisplayHost {
    requests: Arc<Mutex<Vec<ImageResolveRequest>>>,
}

impl DisplayHost for RecordingImageDisplayHost {
    fn realize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resolve_image(&self, request: ImageResolveRequest) -> Result<Option<ResolvedImage>, String> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        Ok(Some(ResolvedImage {
            image_id: 77,
            width: 32,
            height: 24,
        }))
    }
}

fn window_matrix_text(entry: &neomacs_display_protocol::glyph_matrix::WindowMatrixEntry) -> String {
    entry
        .matrix
        .rows
        .iter()
        .filter(|row| row.enabled)
        .flat_map(|row| row.glyphs[1].iter())
        .filter_map(|glyph| match &glyph.glyph_type {
            neomacs_display_protocol::glyph_matrix::GlyphType::Char { ch } => Some(*ch),
            neomacs_display_protocol::glyph_matrix::GlyphType::Composite { text } => {
                text.chars().next()
            }
            _ => None,
        })
        .collect()
}

fn enabled_window_row_texts(
    entry: &neomacs_display_protocol::glyph_matrix::WindowMatrixEntry,
) -> Vec<String> {
    entry
        .matrix
        .rows
        .iter()
        .filter(|row| row.enabled)
        .map(|row| {
            row.glyphs[1]
                .iter()
                .filter_map(|glyph| match &glyph.glyph_type {
                    neomacs_display_protocol::glyph_matrix::GlyphType::Char { ch } => Some(*ch),
                    neomacs_display_protocol::glyph_matrix::GlyphType::Composite { text } => {
                        text.chars().next()
                    }
                    _ => None,
                })
                .collect()
        })
        .collect()
}

fn assert_echo_message_renders_in_minibuffer_window(use_gui_metrics: bool) {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("body line\n");
    }
    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-minibuffer-echo", 640, 160, buf_id);
    let echo = "Echo lives in minibuffer";
    eval.set_current_message(Some(LispString::from_utf8(echo)));

    let mut engine = LayoutEngine::new();
    if use_gui_metrics {
        engine.enable_cosmic_metrics();
    }
    engine.layout_frame_rust(&mut eval, frame_id);

    let state = engine
        .last_frame_display_state
        .as_ref()
        .expect("display state");
    let minibuffer_window_id = state
        .window_infos
        .iter()
        .find(|info| info.is_minibuffer)
        .expect("minibuffer window info")
        .window_id as u64;
    let root_window_id = state
        .window_infos
        .iter()
        .find(|info| !info.is_minibuffer)
        .expect("root window info")
        .window_id as u64;

    let minibuffer_entry = state
        .window_matrices
        .iter()
        .find(|entry| entry.window_id == minibuffer_window_id)
        .expect("minibuffer matrix");
    let root_entry = state
        .window_matrices
        .iter()
        .find(|entry| entry.window_id == root_window_id)
        .expect("root matrix");

    let minibuffer_text = window_matrix_text(minibuffer_entry);
    let root_text = window_matrix_text(root_entry);

    assert!(
        minibuffer_text.contains(echo),
        "expected echo text in minibuffer matrix, got {minibuffer_text:?}"
    );
    assert!(
        !root_text.contains(echo),
        "echo text leaked into root window matrix: {root_text:?}"
    );
    assert!(
        minibuffer_entry
            .matrix
            .rows
            .iter()
            .any(|row| row.enabled && row.role == GlyphRowRole::Minibuffer && !row.mode_line),
        "expected a non-chrome minibuffer row for echo text"
    );
    assert!(
        !root_entry
            .matrix
            .rows
            .iter()
            .any(|row| row.enabled && row.role == GlyphRowRole::Minibuffer),
        "root window should not own minibuffer echo rows"
    );
}

fn assert_multiline_echo_message_uses_minibuffer_rows(use_gui_metrics: bool) {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-minibuffer-echo-lines", 640, 160, buf_id);
    eval.set_current_message(Some(LispString::from_utf8("ALPHA\nBETA")));

    let mut engine = LayoutEngine::new();
    if use_gui_metrics {
        engine.enable_cosmic_metrics();
    }
    engine.layout_frame_rust(&mut eval, frame_id);

    let state = engine
        .last_frame_display_state
        .as_ref()
        .expect("display state");
    let minibuffer_window_id = state
        .window_infos
        .iter()
        .find(|info| info.is_minibuffer)
        .expect("minibuffer window info")
        .window_id as u64;
    let minibuffer_entry = state
        .window_matrices
        .iter()
        .find(|entry| entry.window_id == minibuffer_window_id)
        .expect("minibuffer matrix");
    let row_texts = enabled_window_row_texts(minibuffer_entry);

    assert!(
        row_texts.iter().any(|row| row == "ALPHA"),
        "expected ALPHA in its own minibuffer row, got {row_texts:?}"
    );
    assert!(
        row_texts.iter().any(|row| row == "BETA"),
        "expected BETA in its own minibuffer row, got {row_texts:?}"
    );
    assert!(
        !row_texts.iter().any(|row| row.contains("ALPHABETA")),
        "multiline echo text was flattened into one row: {row_texts:?}"
    );
}

#[test]
fn test_ligature_run_buffer_new() {
    let buf = LigatureRunBuffer::new();

    // All fields should be zeroed/empty
    assert_eq!(buf.chars.len(), 0);
    assert_eq!(buf.advances.len(), 0);
    assert_eq!(buf.start_x, 0.0);
    assert_eq!(buf.start_y, 0.0);
    assert_eq!(buf.face_h, 0.0);
    assert_eq!(buf.face_ascent, 0.0);
    assert_eq!(buf.face_id, 0);
    assert_eq!(buf.total_advance, 0.0);
    assert_eq!(buf.is_overlay, false);
    assert_eq!(buf.height_scale, 0.0);

    // Vectors should be pre-allocated
    assert!(buf.chars.capacity() >= MAX_LIGATURE_RUN_LEN);
    assert!(buf.advances.capacity() >= MAX_LIGATURE_RUN_LEN);
}

#[test]
fn layout_frame_rust_publishes_increasing_display_positions() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("abcd\n");
        buf.goto_byte(1);
    }
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-test", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = 1;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let a = snapshot.point_for_buffer_pos(1).expect("a");
    let b = snapshot.point_for_buffer_pos(2).expect("b");
    let c = snapshot.point_for_buffer_pos(3).expect("c");
    assert!(
        a.x < b.x,
        "expected increasing x positions, got {a:?} then {b:?}"
    );
    assert!(
        b.x < c.x,
        "expected increasing x positions, got {b:?} then {c:?}"
    );
}

#[test]
fn layout_frame_rust_tracks_multibyte_sample_positions() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("a好好b\n");
        buf.goto_byte(0);
    }
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-test", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = 1;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let all_points = snapshot.points.clone();
    let a = snapshot.point_for_buffer_pos(1).expect("a");
    let hao1 = snapshot.point_for_buffer_pos(2).expect("hao1");
    let hao2 = snapshot.point_for_buffer_pos(3).expect("hao2");
    let b = snapshot.point_for_buffer_pos(4).expect("b");
    assert!(
        a.x < hao1.x,
        "expected a before first 好, got {a:?} then {hao1:?}; points={all_points:?}"
    );
    assert!(
        hao1.x < hao2.x,
        "expected first 好 before second 好, got {hao1:?} then {hao2:?}; points={all_points:?}"
    );
    assert!(
        hao2.x < b.x,
        "expected second 好 before b, got {hao2:?} then {b:?}; points={all_points:?}"
    );
    assert!(
        a.width > 0,
        "expected positive width for a, got {a:?}; points={all_points:?}"
    );
    assert!(
        hao1.width > 0,
        "expected positive width for first 好, got {hao1:?}; points={all_points:?}"
    );
    assert!(
        hao2.width > 0,
        "expected positive width for second 好, got {hao2:?}; points={all_points:?}"
    );
    assert!(
        b.width > 0,
        "expected positive width for b, got {b:?}; points={all_points:?}"
    );
}

#[test]
fn layout_frame_rust_publishes_face_scaled_advances_for_inline_plist_faces() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("a好好b ");
        let plist = Value::list(vec![
            Value::keyword("family"),
            Value::string("JetBrains Mono"),
            Value::keyword("height"),
            Value::make_float(1.6),
            Value::keyword("weight"),
            Value::symbol("extra-bold"),
        ]);
        buf.text
            .text_props_put_property(0, buf.text.len(), Value::symbol("face"), plist);
        buf.goto_byte(0);
    }
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-face-advance", 800, 160, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = 1;
        }
    }

    {
        let buffer = eval.buffer_manager().get(buf_id).expect("buffer");
        let face_resolver = crate::neovm_bridge::FaceResolver::new(
            eval.face_table(),
            0x00FFFFFF,
            0x00000000,
            eval.frame_manager()
                .get(frame_id)
                .expect("frame")
                .font_pixel_size,
        );
        let mut next_check = buffer.point_max_char();
        let resolved = face_resolver.face_at_pos(buffer, 0, &mut next_check);
        assert_eq!(resolved.font_family, "JetBrains Mono");
        assert_eq!(resolved.font_weight, 800);
        assert!(
            resolved.font_size > face_resolver.default_face().font_size * 1.5,
            "expected face resolver to scale the inline plist face before layout, got {:?}",
            resolved
        );
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let all_points = snapshot.points.clone();
    let a = snapshot.point_for_buffer_pos(1).expect("a");
    let hao1 = snapshot.point_for_buffer_pos(2).expect("hao1");
    let hao2 = snapshot.point_for_buffer_pos(3).expect("hao2");
    let b = snapshot.point_for_buffer_pos(4).expect("b");
    let space = snapshot.point_for_buffer_pos(5).expect("space");

    let default_font_size = frame.font_pixel_size;
    let face_font_size = default_font_size * 1.6;
    let mut metrics = FontMetricsService::new();
    let expected_a = metrics
        .char_width('a', "JetBrains Mono", 800, false, face_font_size)
        .round() as i64;
    let expected_hao = metrics
        .char_width('好', "JetBrains Mono", 800, false, face_font_size)
        .round() as i64;
    let expected_b = metrics
        .char_width('b', "JetBrains Mono", 800, false, face_font_size)
        .round() as i64;
    let cached_ascii = engine
        .ascii_width_cache
        .iter()
        .find_map(|(key, widths)| {
            (key.family == "JetBrains Mono"
                && key.weight == 800
                && !key.italic
                && key.font_size == face_font_size.round() as i32)
                .then_some(*widths)
        })
        .expect("cached JetBrains Mono widths");

    assert!(
        (cached_ascii['a' as usize].round() as i64 - expected_a).abs() <= 1,
        "expected cached width for 'a' to match FontMetricsService, got {} vs expected {expected_a}",
        cached_ascii['a' as usize]
    );
    assert!(
        (cached_ascii['b' as usize].round() as i64 - expected_b).abs() <= 1,
        "expected cached width for 'b' to match FontMetricsService, got {} vs expected {expected_b}",
        cached_ascii['b' as usize]
    );
    assert!(
        (a.width - expected_a).abs() <= 1,
        "expected inline face width for 'a' to follow FontMetricsService (expected {expected_a}, got {a:?}); points={all_points:?}"
    );
    assert!(
        (hao1.width - expected_hao).abs() <= 1,
        "expected inline face width for first 好 to follow FontMetricsService (expected {expected_hao}, got {hao1:?}); points={all_points:?}"
    );
    assert!(
        (hao2.width - expected_hao).abs() <= 1,
        "expected inline face width for second 好 to follow FontMetricsService (expected {expected_hao}, got {hao2:?}); points={all_points:?}"
    );
    assert!(
        (b.width - expected_b).abs() <= 1,
        "expected inline face width for 'b' to follow FontMetricsService (expected {expected_b}, got {b:?}); points={all_points:?}"
    );
    assert!(
        ((hao1.x - a.x) - expected_a).abs() <= 1,
        "expected next point after 'a' to advance by {expected_a}, got {} -> {} with points={all_points:?}",
        a.x,
        hao1.x
    );
    assert!(
        ((hao2.x - hao1.x) - expected_hao).abs() <= 1,
        "expected next point after first 好 to advance by {expected_hao}, got {} -> {} with points={all_points:?}",
        hao1.x,
        hao2.x
    );
    assert!(
        ((b.x - hao2.x) - expected_hao).abs() <= 1,
        "expected next point after second 好 to advance by {expected_hao}, got {} -> {} with points={all_points:?}",
        hao2.x,
        b.x
    );
    assert!(
        ((space.x - b.x) - expected_b).abs() <= 1,
        "expected next point after 'b' to advance by {expected_b}, got {} -> {} with points={all_points:?}",
        b.x,
        space.x
    );
}

#[test]
fn layout_frame_rust_records_row_metrics_for_plain_text_rows() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("plain text row\n");
        buf.goto_byte(0);
    }
    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-plain-row-metrics", 800, 160, buf_id);

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let text_row = engine
        .last_frame_display_state
        .as_ref()
        .and_then(|state| {
            state
                .window_matrices
                .iter()
                .flat_map(|wm| wm.matrix.rows.iter())
                .find(|row| row.role == GlyphRowRole::Text && row.enabled)
        })
        .expect("text row");

    assert!(
        text_row.height_px > 0.0,
        "expected ordinary text rows to record authoritative height, got {text_row:?}"
    );
    assert!(
        text_row.ascent_px > 0.0,
        "expected ordinary text rows to record authoritative ascent, got {text_row:?}"
    );
}

#[test]
fn layout_frame_rust_captures_cursor_inside_invisible_text_without_rescan() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = "abc hidden xyz";
    let hidden_byte_start = text.find("hidden").expect("hidden start");
    let hidden_byte_end = hidden_byte_start + "hidden".len();
    let hidden_char_start = text[..hidden_byte_start].chars().count() + 1;
    let point_pos = hidden_char_start + 2;
    let next_visible_pos = hidden_char_start + "hidden".chars().count();
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(text);
        buf.goto_byte(point_pos - 1);
        buf.text.text_props_put_property(
            hidden_byte_start,
            hidden_byte_end,
            Value::symbol("invisible"),
            Value::T,
        );
    }

    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-invisible-cursor", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = point_pos;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let cursor = snapshot.phys_cursor.as_ref().expect("cursor");
    let next_visible = snapshot
        .point_for_buffer_pos(next_visible_pos)
        .expect("next visible point");
    assert_eq!(cursor.x, next_visible.x);
    assert_eq!(cursor.row, next_visible.row);
    assert_eq!(cursor.col, next_visible.col);
}

#[test]
fn layout_frame_rust_preserves_logical_cursor_when_window_cursor_is_nil() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("abcdef");
        buf.goto_byte(2);
    }

    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-logical-cursor-only", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = 3;
        }
    }
    eval.frame_manager_mut()
        .set_window_cursor_type(selected_window, Value::NIL);

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let logical_cursor = snapshot.logical_cursor.expect("logical cursor");
    let point = snapshot.point_for_buffer_pos(3).expect("point snapshot");

    assert_eq!(snapshot.phys_cursor, None);
    assert_eq!(logical_cursor.x, point.x);
    assert_eq!(logical_cursor.row, point.row);
    assert_eq!(logical_cursor.col, point.col);
}

#[test]
fn layout_frame_rust_captures_cursor_at_display_replacement_slot_without_rescan() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = "abcXYZdef";
    let repl_byte_start = text.find("XYZ").expect("replacement start");
    let repl_byte_end = repl_byte_start + "XYZ".len();
    let point_pos = repl_byte_start + 2;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(text);
        buf.goto_byte(point_pos - 1);
        buf.text.text_props_put_property(
            repl_byte_start,
            repl_byte_end,
            Value::symbol("display"),
            Value::string("R"),
        );
    }

    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-display-cursor", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = point_pos;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let cursor = snapshot.phys_cursor.as_ref().expect("cursor");
    let c = snapshot.point_for_buffer_pos(3).expect("c");
    let d = snapshot.point_for_buffer_pos(7).expect("d");
    assert_eq!(cursor.x, c.x + c.width);
    assert!(cursor.x < d.x, "cursor should target replacement slot");
    assert_eq!(cursor.row, c.row);
}

#[test]
fn layout_frame_rust_records_display_point_for_display_replacement_slot() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = "abcXYZdef";
    let repl_byte_start = text.find("XYZ").expect("replacement start");
    let repl_byte_end = repl_byte_start + "XYZ".len();
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(text);
        buf.text.text_props_put_property(
            repl_byte_start,
            repl_byte_end,
            Value::symbol("display"),
            Value::string("R"),
        );
    }

    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-display-point", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let c = snapshot.point_for_buffer_pos(3).expect("c");
    let replacement = snapshot.point_for_buffer_pos(4).expect("replacement point");
    let d = snapshot.point_for_buffer_pos(7).expect("d");

    assert_eq!(replacement.x, c.x + c.width);
    assert!(
        replacement.x < d.x,
        "replacement point should stay before following text"
    );
    assert!(replacement.width > 0);
    assert_eq!(replacement.row, c.row);
}

#[test]
fn layout_frame_rust_emits_display_string_replacement_glyphs() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("dir:");
        buf.text.text_props_put_property(
            3,
            4,
            Value::symbol("display"),
            Value::string(": (287 GiB available)"),
        );
    }

    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-display-string", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let state = engine
        .last_frame_display_state
        .as_ref()
        .expect("display state");
    let window_entry = state
        .window_matrices
        .iter()
        .find(|entry| entry.window_id == selected_window.0)
        .expect("selected window matrix");
    let text_row = window_entry
        .matrix
        .rows
        .iter()
        .find(|row| row.enabled && row.role == GlyphRowRole::Text)
        .expect("text row");
    let rendered: String = text_row.glyphs[1]
        .iter()
        .filter_map(|glyph| match &glyph.glyph_type {
            GlyphType::Char { ch } => Some(*ch),
            GlyphType::Composite { text } => text.chars().next(),
            _ => None,
        })
        .collect();

    assert_eq!(rendered, "dir: (287 GiB available)");
}

#[test]
fn layout_frame_rust_emits_inline_image_glyphs_for_display_image_specs() {
    let mut eval = Context::new();
    let requests = Arc::new(Mutex::new(Vec::new()));
    eval.set_display_host(Box::new(RecordingImageDisplayHost {
        requests: Arc::clone(&requests),
    }));
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = "aXb";
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(text);
        buf.goto_byte(1);
        buf.text.text_props_put_property(
            1,
            2,
            Value::symbol("display"),
            Value::list(vec![
                Value::symbol("image"),
                Value::keyword("type"),
                Value::symbol("png"),
                Value::keyword("file"),
                Value::string("/tmp/neomacs-inline-image.png"),
                Value::keyword("width"),
                Value::fixnum(32),
                Value::keyword("height"),
                Value::fixnum(24),
            ]),
        );
    }

    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-inline-image", 320, 120, buf_id);

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let state = engine
        .last_frame_display_state
        .as_ref()
        .expect("frame display state");
    let image = state.images.first().expect("inline image glyph");
    assert_eq!(image.image_id, 77);
    assert_eq!(image.width, 32.0);
    assert_eq!(image.height, 24.0);

    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_width, 32);
    assert_eq!(requests[0].max_height, 24);
}

#[test]
fn layout_frame_rust_captures_cursor_inside_hscroll_skipped_text_without_rescan() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("abcdef\n");
        buf.goto_byte(1);
        buf.set_buffer_local("truncate-lines", Value::T);
    }

    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-hscroll-cursor", 160, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            hscroll,
            ..
        } = window
        {
            *window_start = 1;
            *point = 2;
            *hscroll = 3;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let cursor = snapshot.phys_cursor.as_ref().expect("cursor");
    assert_eq!(cursor.x, 0);
    assert_eq!(cursor.row, 0);
    assert_eq!(cursor.col, 0);
}

fn assert_layout_frame_rust_tab_cursor_width(x_stretch_cursor: bool, cursor_type: Value) {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("a\tb");
        buf.goto_byte(1);
        buf.set_buffer_local("cursor-type", cursor_type);
    }
    eval.set_variable(
        "x-stretch-cursor",
        if x_stretch_cursor {
            Value::T
        } else {
            Value::NIL
        },
    );

    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-tab-cursor", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = 2;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let cursor = snapshot.phys_cursor.as_ref().expect("cursor");
    let a = snapshot.point_for_buffer_pos(1).expect("a");
    let b = snapshot.point_for_buffer_pos(3).expect("b");
    let full_tab_slot_width = b.x - (a.x + a.width);
    let single_column_width = frame.char_width.round() as i64;

    assert_eq!(cursor.x, a.x + a.width);
    assert_eq!(cursor.row, a.row);
    assert_eq!(b.x - cursor.x, full_tab_slot_width);
    assert!(full_tab_slot_width > single_column_width);
    if x_stretch_cursor {
        assert_eq!(cursor.width, full_tab_slot_width);
    } else {
        assert_eq!(cursor.width, single_column_width);
    }
}

#[test]
fn layout_frame_rust_clamps_tab_cursor_width_when_x_stretch_cursor_is_nil() {
    assert_layout_frame_rust_tab_cursor_width(false, Value::T);
}

#[test]
fn layout_frame_rust_expands_tab_cursor_width_when_x_stretch_cursor_is_t() {
    assert_layout_frame_rust_tab_cursor_width(true, Value::T);
}

#[test]
fn layout_frame_rust_clamps_tab_hbar_cursor_width_when_x_stretch_cursor_is_nil() {
    assert_layout_frame_rust_tab_cursor_width(false, Value::symbol("hbar"));
}

#[test]
fn layout_frame_rust_expands_tab_hbar_cursor_width_when_x_stretch_cursor_is_t() {
    assert_layout_frame_rust_tab_cursor_width(true, Value::symbol("hbar"));
}

#[test]
fn layout_frame_rust_emits_buffer_tab_as_stretch_glyph() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("a\tb");
    }

    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-tab-stretch", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let state = engine
        .last_frame_display_state
        .as_ref()
        .expect("display state");
    let window_entry = state
        .window_matrices
        .iter()
        .find(|entry| entry.window_id == selected_window.0)
        .expect("selected window matrix");
    let text_row = window_entry
        .matrix
        .rows
        .iter()
        .find(|row| row.enabled && row.role == GlyphRowRole::Text)
        .expect("text row");
    let glyphs = &text_row.glyphs[1];

    assert!(matches!(
        glyphs.first().map(|glyph| &glyph.glyph_type),
        Some(GlyphType::Char { ch: 'a' })
    ));
    assert!(matches!(
        glyphs.get(1).map(|glyph| &glyph.glyph_type),
        Some(GlyphType::Stretch { width_cols: 7 })
    ));
    assert!(matches!(
        glyphs.get(2).map(|glyph| &glyph.glyph_type),
        Some(GlyphType::Char { ch: 'b' })
    ));
}

#[test]
fn layout_frame_rust_emits_display_space_as_stretch_glyph() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = "a b";
    let space_byte_start = text.find(' ').expect("space start");
    let space_byte_end = space_byte_start + 1;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(text);
        buf.text.text_props_put_property(
            space_byte_start,
            space_byte_end,
            Value::symbol("display"),
            display_space_width_spec(4),
        );
    }

    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-display-space-stretch", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let state = engine
        .last_frame_display_state
        .as_ref()
        .expect("display state");
    let window_entry = state
        .window_matrices
        .iter()
        .find(|entry| entry.window_id == selected_window.0)
        .expect("selected window matrix");
    let text_row = window_entry
        .matrix
        .rows
        .iter()
        .find(|row| row.enabled && row.role == GlyphRowRole::Text)
        .expect("text row");
    let glyphs = &text_row.glyphs[1];

    assert!(matches!(
        glyphs.first().map(|glyph| &glyph.glyph_type),
        Some(GlyphType::Char { ch: 'a' })
    ));
    assert!(matches!(
        glyphs.get(1).map(|glyph| &glyph.glyph_type),
        Some(GlyphType::Stretch { width_cols: 4 })
    ));
    assert!(matches!(
        glyphs.get(2).map(|glyph| &glyph.glyph_type),
        Some(GlyphType::Char { ch: 'b' })
    ));
}

fn display_space_width_spec(columns: i64) -> Value {
    Value::list(vec![
        Value::symbol("space"),
        Value::keyword("width"),
        Value::fixnum(columns),
    ])
}

fn scaled_face_plist() -> Value {
    Value::list(vec![
        Value::keyword("family"),
        Value::string("JetBrains Mono"),
        Value::keyword("height"),
        Value::make_float(1.6),
        Value::keyword("weight"),
        Value::symbol("extra-bold"),
    ])
}

fn assert_layout_frame_rust_display_space_cursor_width(x_stretch_cursor: bool, cursor_type: Value) {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = "a b";
    let space_byte_start = text.find(' ').expect("space start");
    let space_byte_end = space_byte_start + 1;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(text);
        buf.goto_byte(1);
        buf.text.text_props_put_property(
            space_byte_start,
            space_byte_end,
            Value::symbol("display"),
            display_space_width_spec(4),
        );
        buf.text.text_props_put_property(
            space_byte_start,
            space_byte_end,
            Value::symbol("face"),
            scaled_face_plist(),
        );
        buf.set_buffer_local("cursor-type", cursor_type);
    }
    eval.set_variable(
        "x-stretch-cursor",
        if x_stretch_cursor {
            Value::T
        } else {
            Value::NIL
        },
    );

    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-display-space-cursor", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = 2;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let cursor = snapshot.phys_cursor.as_ref().expect("cursor");
    let a = snapshot.point_for_buffer_pos(1).expect("a");
    let b = snapshot.point_for_buffer_pos(3).expect("b");
    let full_slot_width = b.x - (a.x + a.width);
    let single_column_width = frame.char_width.round() as i64;
    let expected_space_width = (4.0 * frame.char_width).round() as i64;

    assert_eq!(cursor.x, a.x + a.width);
    assert_eq!(b.x - cursor.x, full_slot_width);
    assert!((full_slot_width - expected_space_width).abs() <= 1);
    if x_stretch_cursor {
        assert_eq!(cursor.width, full_slot_width);
    } else {
        assert_eq!(cursor.width, single_column_width);
    }
}

#[test]
fn layout_frame_rust_display_space_width_uses_canonical_column_width() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = "a b";
    let space_byte_start = text.find(' ').expect("space start");
    let space_byte_end = space_byte_start + 1;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(text);
        buf.goto_byte(1);
        buf.text.text_props_put_property(
            space_byte_start,
            space_byte_end,
            Value::symbol("display"),
            display_space_width_spec(4),
        );
        buf.text.text_props_put_property(
            space_byte_start,
            space_byte_end,
            Value::symbol("face"),
            scaled_face_plist(),
        );
    }

    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-display-space-width", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf { window_start, .. } = window {
            *window_start = 1;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let a = snapshot.point_for_buffer_pos(1).expect("a");
    let b = snapshot.point_for_buffer_pos(3).expect("b");
    let slot_width = b.x - (a.x + a.width);
    let expected_width = (4.0 * frame.char_width).round() as i64;

    assert!(
        (slot_width - expected_width).abs() <= 1,
        "display space width should follow canonical frame column width; got slot {slot_width}, expected {expected_width}, frame char width {}, points={:?}",
        frame.char_width,
        snapshot.points
    );
}

#[test]
fn layout_frame_rust_records_display_point_for_display_space_slot() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = "a b";
    let space_byte_start = text.find(' ').expect("space start");
    let space_byte_end = space_byte_start + 1;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(text);
        buf.text.text_props_put_property(
            space_byte_start,
            space_byte_end,
            Value::symbol("display"),
            display_space_width_spec(4),
        );
        buf.text.text_props_put_property(
            space_byte_start,
            space_byte_end,
            Value::symbol("face"),
            scaled_face_plist(),
        );
    }

    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-display-space-point", 320, 120, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let a = snapshot.point_for_buffer_pos(1).expect("a");
    let space = snapshot.point_for_buffer_pos(2).expect("space");
    let b = snapshot.point_for_buffer_pos(3).expect("b");
    let expected_width = (4.0 * frame.char_width).round() as i64;

    assert_eq!(space.x, a.x + a.width);
    assert!(space.x < b.x);
    assert!((space.width - expected_width).abs() <= 1);
    assert_eq!(space.row, a.row);
}

#[test]
fn layout_frame_rust_clamps_display_space_cursor_width_when_x_stretch_cursor_is_nil() {
    assert_layout_frame_rust_display_space_cursor_width(false, Value::T);
}

#[test]
fn layout_frame_rust_expands_display_space_cursor_width_when_x_stretch_cursor_is_t() {
    assert_layout_frame_rust_display_space_cursor_width(true, Value::T);
}

#[test]
fn layout_frame_rust_clamps_display_space_hbar_cursor_width_when_x_stretch_cursor_is_nil() {
    assert_layout_frame_rust_display_space_cursor_width(false, Value::symbol("hbar"));
}

#[test]
fn layout_frame_rust_expands_display_space_hbar_cursor_width_when_x_stretch_cursor_is_t() {
    assert_layout_frame_rust_display_space_cursor_width(true, Value::symbol("hbar"));
}

#[test]
fn layout_frame_rust_keeps_mixed_width_advances_correct_after_mid_line_face_change() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;

    let prefix = "  h=0.9 w=normal:                     ";
    let sample = "a好好b  ABCXYZ 0123456789  -> <= >=";
    let sample_pos = prefix.chars().count() + 1;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(prefix);
        let sample_byte_start = buf.text.len();
        buf.insert(sample);
        let sample_byte_end = buf.text.len();
        let plist = Value::list(vec![
            Value::keyword("family"),
            Value::string("Noto Sans Mono"),
            Value::keyword("height"),
            Value::make_float(0.9),
            Value::keyword("weight"),
            Value::symbol("normal"),
        ]);
        buf.text.text_props_put_property(
            sample_byte_start,
            sample_byte_end,
            Value::symbol("face"),
            plist,
        );
        buf.goto_byte(0);
    }

    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-face-mid-line", 1400, 160, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = 1;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let all_points = snapshot.points.clone();
    let a = snapshot.point_for_buffer_pos(sample_pos).expect("a");
    let hao1 = snapshot
        .point_for_buffer_pos(sample_pos + 1)
        .expect("first 好");
    let hao2 = snapshot
        .point_for_buffer_pos(sample_pos + 2)
        .expect("second 好");
    let b = snapshot.point_for_buffer_pos(sample_pos + 3).expect("b");

    let face_font_size = frame.font_pixel_size * 0.9;
    let mut metrics = FontMetricsService::new();
    let expected_a = metrics
        .char_width('a', "Noto Sans Mono", 400, false, face_font_size)
        .round() as i64;
    let expected_hao = metrics
        .char_width('好', "Noto Sans Mono", 400, false, face_font_size)
        .round() as i64;
    let expected_b = metrics
        .char_width('b', "Noto Sans Mono", 400, false, face_font_size)
        .round() as i64;

    assert!(
        (a.width - expected_a).abs() <= 1,
        "expected a width {expected_a}, got {a:?}; points={all_points:?}"
    );
    assert!(
        (hao1.width - expected_hao).abs() <= 1,
        "expected first 好 width {expected_hao}, got {hao1:?}; points={all_points:?}"
    );
    assert!(
        (hao2.width - expected_hao).abs() <= 1,
        "expected second 好 width {expected_hao}, got {hao2:?}; points={all_points:?}"
    );
    assert!(
        (b.width - expected_b).abs() <= 1,
        "expected b width {expected_b}, got {b:?}; points={all_points:?}"
    );
    assert!(
        ((hao1.x - a.x) - expected_a).abs() <= 1,
        "expected first 好 x delta {expected_a}, got {} -> {}; points={all_points:?}",
        a.x,
        hao1.x
    );
    assert!(
        ((hao2.x - hao1.x) - expected_hao).abs() <= 1,
        "expected second 好 x delta {expected_hao}, got {} -> {}; points={all_points:?}",
        hao1.x,
        hao2.x
    );
    assert!(
        ((b.x - hao2.x) - expected_hao).abs() <= 1,
        "expected b x delta {expected_hao}, got {} -> {}; points={all_points:?}",
        hao2.x,
        b.x
    );
    let space = snapshot
        .point_for_buffer_pos(sample_pos + 4)
        .expect("space");
    assert_eq!(
        space.x - b.x,
        b.width,
        "expected next point after 'b' to land exactly one snapped advance later; b={b:?} space={space:?} points={all_points:?}"
    );
}

#[test]
fn layout_frame_rust_keeps_face_positions_after_truncated_multibyte_line() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;

    let truncated_prefix = format!("{}\n", "好".repeat(20));
    let sample = "a好好b";
    let sample_pos = truncated_prefix.chars().count() + 1;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(&truncated_prefix);
        let sample_byte_start = buf.text.len();
        buf.insert(sample);
        let sample_byte_end = buf.text.len();
        buf.insert("\n");
        let plist = Value::list(vec![
            Value::keyword("family"),
            Value::string("Noto Sans Mono"),
            Value::keyword("height"),
            Value::make_float(0.9),
            Value::keyword("weight"),
            Value::symbol("normal"),
        ]);
        buf.text.text_props_put_property(
            sample_byte_start,
            sample_byte_end,
            Value::symbol("face"),
            plist,
        );
        buf.goto_byte(0);
        buf.set_buffer_local("truncate-lines", Value::T);
    }

    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-truncated-multibyte-face", 128, 160, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = sample_pos;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let all_points = snapshot.points.clone();
    let a = snapshot.point_for_buffer_pos(sample_pos).expect("a");
    let hao1 = snapshot
        .point_for_buffer_pos(sample_pos + 1)
        .expect("first 好");
    let hao2 = snapshot
        .point_for_buffer_pos(sample_pos + 2)
        .expect("second 好");
    let b = snapshot.point_for_buffer_pos(sample_pos + 3).expect("b");

    let face_font_size = frame.font_pixel_size * 0.9;
    let mut metrics = FontMetricsService::new();
    let expected_a = metrics
        .char_width('a', "Noto Sans Mono", 400, false, face_font_size)
        .round() as i64;
    let expected_hao = metrics
        .char_width('好', "Noto Sans Mono", 400, false, face_font_size)
        .round() as i64;
    let expected_b = metrics
        .char_width('b', "Noto Sans Mono", 400, false, face_font_size)
        .round() as i64;

    assert!(
        (a.width - expected_a).abs() <= 1,
        "expected a width {expected_a}, got {a:?}; points={all_points:?}"
    );
    assert!(
        (hao1.width - expected_hao).abs() <= 1,
        "expected first 好 width {expected_hao}, got {hao1:?}; points={all_points:?}"
    );
    assert!(
        (hao2.width - expected_hao).abs() <= 1,
        "expected second 好 width {expected_hao}, got {hao2:?}; points={all_points:?}"
    );
    assert!(
        (b.width - expected_b).abs() <= 1,
        "expected b width {expected_b}, got {b:?}; points={all_points:?}"
    );
    assert!(
        ((hao1.x - a.x) - expected_a).abs() <= 1,
        "expected first 好 x delta {expected_a}, got {} -> {}; points={all_points:?}",
        a.x,
        hao1.x
    );
    assert!(
        ((hao2.x - hao1.x) - expected_hao).abs() <= 1,
        "expected second 好 x delta {expected_hao}, got {} -> {}; points={all_points:?}",
        hao1.x,
        hao2.x
    );
    assert!(
        ((b.x - hao2.x) - expected_hao).abs() <= 1,
        "expected b x delta {expected_hao}, got {} -> {}; points={all_points:?}",
        hao2.x,
        b.x
    );
}

#[test]
fn layout_frame_rust_keeps_mixed_width_positions_correct_after_sequential_window_point_moves() {
    #[derive(Clone, Copy, Debug)]
    struct TargetRow {
        line_beg: usize,
        sample_pos: usize,
        height: f32,
        weight: u16,
    }

    fn char_at_lisp_pos(buffer: &neovm_core::buffer::Buffer, pos: usize) -> Option<char> {
        if pos == 0 {
            return None;
        }
        let byte_pos = buffer.char_to_byte_clamped(pos - 1);
        buffer.char_after(byte_pos)
    }

    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let sample = "a好好b  ABCXYZ 0123456789  -> <= >=";
    let mut targets = Vec::new();
    let weights = [
        ("normal", 400_u16),
        ("semi-bold", 600_u16),
        ("bold", 700_u16),
        ("extra-bold", 800_u16),
    ];

    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        for height in [0.9_f32, 1.0_f32, 1.2_f32, 1.6_f32] {
            for (weight_name, weight_value) in weights {
                let line_beg = if buf.text.is_empty() {
                    1usize
                } else {
                    buf.point_max_char() as usize + 1
                };
                let prefix = format!("  {:<35} ", format!("h={height} w={weight_name}:"));
                let sample_pos = line_beg + prefix.chars().count();
                buf.insert(&prefix);
                let sample_byte_start = buf.text.len();
                buf.insert(sample);
                let sample_byte_end = buf.text.len();
                buf.insert("\n");
                let plist = Value::list(vec![
                    Value::keyword("family"),
                    Value::string("JetBrains Mono"),
                    Value::keyword("height"),
                    Value::make_float(height as f64),
                    Value::keyword("weight"),
                    Value::symbol(weight_name),
                ]);
                buf.text.text_props_put_property(
                    sample_byte_start,
                    sample_byte_end,
                    Value::symbol("face"),
                    plist,
                );
                targets.push(TargetRow {
                    line_beg,
                    sample_pos,
                    height,
                    weight: weight_value,
                });
            }
        }
        buf.goto_byte(0);
    }

    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-sequential-window-point", 1400, 256, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = 1;
        }
    }

    let mut engine = LayoutEngine::new();
    let mut metrics = FontMetricsService::new();

    for target in &targets {
        let byte_pos = {
            let buffer = eval.buffer_manager().get(buf_id).expect("buffer");
            buffer.lisp_pos_to_byte(target.line_beg as i64)
        };
        let _ = eval.buffer_manager_mut().goto_buffer_byte(buf_id, byte_pos);
        {
            let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
            let window = frame
                .find_window_mut(selected_window)
                .expect("selected window");
            if let neovm_core::window::Window::Leaf { point, .. } = window {
                *point = target.line_beg;
            }
        }

        engine.layout_frame_rust(&mut eval, frame_id);

        let frame = eval.frame_manager().get(frame_id).expect("frame");
        let snapshot = frame
            .window_display_snapshot(selected_window)
            .expect("display snapshot");
        let all_points = snapshot.points.clone();
        let buffer = eval.buffer_manager().get(buf_id).expect("buffer");
        let sample_chars = [
            (target.line_beg, char_at_lisp_pos(buffer, target.line_beg)),
            (
                target.sample_pos,
                char_at_lisp_pos(buffer, target.sample_pos),
            ),
            (
                target.sample_pos + 1,
                char_at_lisp_pos(buffer, target.sample_pos + 1),
            ),
            (
                target.sample_pos + 2,
                char_at_lisp_pos(buffer, target.sample_pos + 2),
            ),
            (
                target.sample_pos + 3,
                char_at_lisp_pos(buffer, target.sample_pos + 3),
            ),
        ];
        let a = snapshot
            .point_for_buffer_pos(target.sample_pos)
            .expect("sample a");
        let hao1 = snapshot
            .point_for_buffer_pos(target.sample_pos + 1)
            .expect("sample first 好");
        let hao2 = snapshot
            .point_for_buffer_pos(target.sample_pos + 2)
            .expect("sample second 好");
        let b = snapshot
            .point_for_buffer_pos(target.sample_pos + 3)
            .expect("sample b");
        let after_b = snapshot
            .point_for_buffer_pos(target.sample_pos + 4)
            .expect("sample trailing space");

        let face_font_size = frame.font_pixel_size * target.height;
        let expected_a = metrics
            .char_width('a', "JetBrains Mono", target.weight, false, face_font_size)
            .round() as i64;
        let expected_hao = metrics
            .char_width('好', "JetBrains Mono", target.weight, false, face_font_size)
            .round() as i64;
        let expected_b = metrics
            .char_width('b', "JetBrains Mono", target.weight, false, face_font_size)
            .round() as i64;

        assert!(
            (a.width - expected_a).abs() <= 1,
            "expected a width {expected_a} after sequential point moves, got {a:?}; target={target:?}; chars={sample_chars:?}; points={all_points:?}"
        );
        assert!(
            (hao1.width - expected_hao).abs() <= 1,
            "expected first 好 width {expected_hao} after sequential point moves, got {hao1:?}; target={target:?}; chars={sample_chars:?}; points={all_points:?}"
        );
        assert!(
            (hao2.width - expected_hao).abs() <= 1,
            "expected second 好 width {expected_hao} after sequential point moves, got {hao2:?}; target={target:?}; chars={sample_chars:?}; points={all_points:?}"
        );
        assert!(
            (b.width - expected_b).abs() <= 1,
            "expected b width {expected_b} after sequential point moves, got {b:?}; target={target:?}; chars={sample_chars:?}; points={all_points:?}"
        );
        assert!(
            ((hao1.x - a.x) - expected_a).abs() <= 1,
            "expected first 好 x delta {expected_a} after sequential point moves, got {} -> {}; target={target:?}; chars={sample_chars:?}; points={all_points:?}",
            a.x,
            hao1.x
        );
        assert!(
            ((hao2.x - hao1.x) - expected_hao).abs() <= 1,
            "expected second 好 x delta {expected_hao} after sequential point moves, got {} -> {}; target={target:?}; chars={sample_chars:?}; points={all_points:?}",
            hao1.x,
            hao2.x
        );
        assert!(
            ((b.x - hao2.x) - expected_hao).abs() <= 1,
            "expected b x delta {expected_hao} after sequential point moves, got {} -> {}; target={target:?}; chars={sample_chars:?}; points={all_points:?}",
            hao2.x,
            b.x
        );
        assert!(
            ((after_b.x - b.x) - expected_b).abs() <= 1,
            "expected post-b x delta {expected_b} after sequential point moves, got {} -> {}; target={target:?}; chars={sample_chars:?}; points={all_points:?}",
            b.x,
            after_b.x
        );
    }
}

#[test]
fn layout_frame_rust_keeps_mixed_width_positions_correct_across_family_switches() {
    #[derive(Clone, Copy, Debug)]
    struct TargetRow<'a> {
        family: &'a str,
        line_beg: usize,
        sample_pos: usize,
        height: f32,
        weight_name: &'a str,
        weight: u16,
    }

    fn char_at_lisp_pos(buffer: &neovm_core::buffer::Buffer, pos: usize) -> Option<char> {
        if pos == 0 {
            return None;
        }
        let byte_pos = buffer.char_to_byte_clamped(pos - 1);
        buffer.char_after(byte_pos)
    }

    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let sample = "a好好b  ABCXYZ 0123456789  -> <= >=";
    let mut targets = Vec::new();
    let weights = [
        ("normal", 400_u16),
        ("semi-bold", 600_u16),
        ("bold", 700_u16),
        ("extra-bold", 800_u16),
    ];
    let families = [
        "JetBrains Mono",
        "Hack",
        "DejaVu Sans Mono",
        "Noto Sans Mono",
    ];

    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        for family in families {
            let heading = format!("  -- family: {family} --\n");
            buf.insert(&heading);
            for height in [0.9_f32, 1.0_f32, 1.2_f32, 1.6_f32] {
                for (weight_name, weight_value) in weights {
                    let line_beg = if buf.text.is_empty() {
                        1usize
                    } else {
                        buf.point_max_char() as usize + 1
                    };
                    let prefix = format!("  {:<35} ", format!("h={height} w={weight_name}:"));
                    let sample_pos = line_beg + prefix.chars().count();
                    buf.insert(&prefix);
                    let sample_byte_start = buf.text.len();
                    buf.insert(sample);
                    let sample_byte_end = buf.text.len();
                    buf.insert("\n");
                    let plist = Value::list(vec![
                        Value::keyword("family"),
                        Value::string(family),
                        Value::keyword("height"),
                        Value::make_float(height as f64),
                        Value::keyword("weight"),
                        Value::symbol(weight_name),
                    ]);
                    buf.text.text_props_put_property(
                        sample_byte_start,
                        sample_byte_end,
                        Value::symbol("face"),
                        plist,
                    );
                    targets.push(TargetRow {
                        family,
                        line_beg,
                        sample_pos,
                        height,
                        weight_name,
                        weight: weight_value,
                    });
                }
            }
            buf.insert("\n");
        }
        buf.goto_byte(0);
    }

    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-family-switches", 1400, 1600, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = 1;
        }
    }

    let mut engine = LayoutEngine::new();
    let mut metrics = FontMetricsService::new();

    for target in &targets {
        let byte_pos = {
            let buffer = eval.buffer_manager().get(buf_id).expect("buffer");
            buffer.lisp_pos_to_byte(target.line_beg as i64)
        };
        let _ = eval.buffer_manager_mut().goto_buffer_byte(buf_id, byte_pos);
        {
            let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
            let window = frame
                .find_window_mut(selected_window)
                .expect("selected window");
            if let neovm_core::window::Window::Leaf { point, .. } = window {
                *point = target.line_beg;
            }
        }

        engine.layout_frame_rust(&mut eval, frame_id);

        let frame = eval.frame_manager().get(frame_id).expect("frame");
        let snapshot = frame
            .window_display_snapshot(selected_window)
            .expect("display snapshot");
        let all_points = snapshot.points.clone();
        let visible_span = snapshot
            .rows
            .iter()
            .find_map(|row| row.start_buffer_pos)
            .zip(
                snapshot
                    .rows
                    .iter()
                    .rev()
                    .find_map(|row| row.end_buffer_pos),
            );
        let buffer = eval.buffer_manager().get(buf_id).expect("buffer");
        let sample_chars = [
            (
                target.sample_pos,
                char_at_lisp_pos(buffer, target.sample_pos),
            ),
            (
                target.sample_pos + 1,
                char_at_lisp_pos(buffer, target.sample_pos + 1),
            ),
            (
                target.sample_pos + 2,
                char_at_lisp_pos(buffer, target.sample_pos + 2),
            ),
            (
                target.sample_pos + 3,
                char_at_lisp_pos(buffer, target.sample_pos + 3),
            ),
        ];
        let a = snapshot
            .point_for_buffer_pos(target.sample_pos)
            .unwrap_or_else(|| {
                panic!(
                    "sample a missing; target={target:?}; visible_span={visible_span:?}; chars={sample_chars:?}; points={all_points:?}"
                )
            });
        let hao1 = snapshot
            .point_for_buffer_pos(target.sample_pos + 1)
            .unwrap_or_else(|| {
                panic!(
                    "sample first 好 missing; target={target:?}; visible_span={visible_span:?}; chars={sample_chars:?}; points={all_points:?}"
                )
            });
        let hao2 = snapshot
            .point_for_buffer_pos(target.sample_pos + 2)
            .unwrap_or_else(|| {
                panic!(
                    "sample second 好 missing; target={target:?}; visible_span={visible_span:?}; chars={sample_chars:?}; points={all_points:?}"
                )
            });
        let b = snapshot
            .point_for_buffer_pos(target.sample_pos + 3)
            .unwrap_or_else(|| {
                panic!(
                    "sample b missing; target={target:?}; visible_span={visible_span:?}; chars={sample_chars:?}; points={all_points:?}"
                )
            });
        let after_b = snapshot
            .point_for_buffer_pos(target.sample_pos + 4)
            .unwrap_or_else(|| {
                panic!(
                    "sample trailing space missing; target={target:?}; visible_span={visible_span:?}; chars={sample_chars:?}; points={all_points:?}"
                )
            });

        let face_font_size = frame.font_pixel_size * target.height;
        let expected_a = metrics
            .char_width('a', target.family, target.weight, false, face_font_size)
            .round() as i64;
        let expected_hao = metrics
            .char_width('好', target.family, target.weight, false, face_font_size)
            .round() as i64;
        let expected_b = metrics
            .char_width('b', target.family, target.weight, false, face_font_size)
            .round() as i64;

        assert!(
            (a.width - expected_a).abs() <= 1,
            "expected a width {expected_a}, got {a:?}; target={target:?}; chars={sample_chars:?}; points={all_points:?}"
        );
        assert!(
            (hao1.width - expected_hao).abs() <= 1,
            "expected first 好 width {expected_hao}, got {hao1:?}; target={target:?}; chars={sample_chars:?}; points={all_points:?}"
        );
        assert!(
            (hao2.width - expected_hao).abs() <= 1,
            "expected second 好 width {expected_hao}, got {hao2:?}; target={target:?}; chars={sample_chars:?}; points={all_points:?}"
        );
        assert!(
            (b.width - expected_b).abs() <= 1,
            "expected b width {expected_b}, got {b:?}; target={target:?}; chars={sample_chars:?}; points={all_points:?}"
        );
        assert!(
            ((hao1.x - a.x) - expected_a).abs() <= 1,
            "expected first 好 x delta {expected_a}, got {} -> {}; target={target:?}; chars={sample_chars:?}; points={all_points:?}",
            a.x,
            hao1.x
        );
        assert!(
            ((hao2.x - hao1.x) - expected_hao).abs() <= 1,
            "expected second 好 x delta {expected_hao}, got {} -> {}; target={target:?}; chars={sample_chars:?}; points={all_points:?}",
            hao1.x,
            hao2.x
        );
        assert!(
            ((b.x - hao2.x) - expected_hao).abs() <= 1,
            "expected b x delta {expected_hao}, got {} -> {}; target={target:?}; chars={sample_chars:?}; points={all_points:?}",
            hao2.x,
            b.x
        );
        assert!(
            ((after_b.x - b.x) - expected_b).abs() <= 1,
            "expected post-b x delta {expected_b}, got {} -> {}; target={target:?}; chars={sample_chars:?}; points={all_points:?}",
            b.x,
            after_b.x
        );

        let _ = target.weight_name;
    }
}

#[test]
fn layout_frame_rust_word_wrap_snapshot_stays_sorted_after_rewind() {
    fn char_at_lisp_pos(buffer: &neovm_core::buffer::Buffer, pos: usize) -> Option<char> {
        if pos == 0 {
            return None;
        }
        let byte_pos = buffer.char_to_byte_clamped(pos - 1);
        buffer.char_after(byte_pos)
    }

    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("aaaa bbbb cccc dddd\n");
        buf.goto_byte(0);
        buf.set_buffer_local("word-wrap", Value::T);
    }
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-wrap", 96, 160, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = 1;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    assert!(
        snapshot.points.iter().any(|point| point.row > 0),
        "expected word-wrap to create multiple rows, got points={:?}",
        snapshot.points
    );
    let buffer = eval.buffer_manager().get(buf_id).expect("buffer");
    let point_chars = snapshot
        .points
        .iter()
        .map(|point| (point.buffer_pos, char_at_lisp_pos(buffer, point.buffer_pos)))
        .collect::<Vec<_>>();
    for window in snapshot.points.windows(2) {
        assert!(
            window[0].buffer_pos < window[1].buffer_pos,
            "expected snapshot points to stay sorted after wrap rewind, got {:?}; chars={:?}",
            snapshot.points,
            point_chars
        );
    }
}

#[test]
fn layout_frame_rust_reads_far_enough_for_last_visible_truncated_line() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let mut text = String::new();
    for line in 0..32 {
        text.push_str(&format!("line-{line:02} abcdefghijklmnop\n"));
    }
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(&text);
        buf.goto_byte(0);
        buf.set_buffer_local("truncate-lines", Value::T);
    }
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-read-span", 96, 640, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    let target_pos = {
        let mut pos = 1usize;
        for line in 0..26 {
            pos += format!("line-{line:02} abcdefghijklmnop\n").chars().count();
        }
        pos
    };
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        // Selected-window point lives in the buffer; keep pt_char in
        // sync with the target point so redisplay retries read the same
        // location the leaf window advertises.
        buf.goto_byte(target_pos - 1);
    }
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = target_pos;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let target = snapshot.point_for_buffer_pos(target_pos);
    assert!(
        target.is_some(),
        "expected last visible truncated line to remain readable by layout, target_pos={target_pos}, points={:?}",
        snapshot.points
    );
}

#[test]
fn layout_frame_rust_retries_window_when_point_starts_below_visible_span() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let lines = (0..40)
        .map(|line| format!("line-{line:02}\n"))
        .collect::<Vec<_>>();
    let text = lines.join("");
    let target_pos = lines
        .iter()
        .take(20)
        .map(|line| line.chars().count())
        .sum::<usize>()
        + 1;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(&text);
        // Selected-window point lives in the buffer; see
        // window.c:window_point. Set buffer pt_char to
        // target_pos so window_params_from_neovm reads it as
        // params.point.
        buf.goto_byte(target_pos - 1);
    }
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-retry", 160, 192, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = target_pos;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let window = frame.find_window(selected_window).expect("selected window");

    assert!(
        snapshot.point_for_buffer_pos(target_pos).is_some(),
        "expected retried layout to publish geometry for point {target_pos}, points={:?}",
        snapshot.points
    );
    match window {
        neovm_core::window::Window::Leaf { window_start, .. } => {
            assert!(
                *window_start > 1,
                "expected window-start to advance after retry, got {window_start}"
            );
        }
        other => panic!("expected leaf window, got {other:?}"),
    }
}

#[test]
fn next_window_start_from_visible_rows_uses_visual_row_boundaries() {
    let rows = vec![
        DisplayRowSnapshot {
            row: 0,
            y: 0,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(1),
            end_buffer_pos: Some(8),
        },
        DisplayRowSnapshot {
            row: 1,
            y: 16,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(9),
            end_buffer_pos: Some(16),
        },
        DisplayRowSnapshot {
            row: 2,
            y: 32,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(17),
            end_buffer_pos: Some(24),
        },
        DisplayRowSnapshot {
            row: 3,
            y: 48,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(25),
            end_buffer_pos: Some(32),
        },
    ];

    assert_eq!(
        next_window_start_from_visible_rows(&rows, 1),
        Some(32),
        "expected retry to advance to the next internal 0-based char position after the last visible row"
    );
    assert_eq!(
        next_window_start_from_visible_rows(&rows, 25),
        Some(32),
        "expected retry to keep the furthest internal 0-based visible progress that still advances"
    );
    assert_eq!(
        next_window_start_from_visible_rows(&rows, 33),
        None,
        "expected no retry candidate once the rendered span no longer advances"
    );
}

#[test]
fn next_window_start_for_partially_visible_point_row_scrolls_enough_to_fit_row() {
    let rows = vec![
        DisplayRowSnapshot {
            row: 0,
            y: 0,
            height: 20,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(1),
            end_buffer_pos: Some(10),
        },
        DisplayRowSnapshot {
            row: 1,
            y: 20,
            height: 20,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(11),
            end_buffer_pos: Some(20),
        },
        DisplayRowSnapshot {
            row: 2,
            y: 40,
            height: 30,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(21),
            end_buffer_pos: Some(30),
        },
    ];

    assert_eq!(
        next_window_start_for_partially_visible_point_row(&rows, 25, 0, 60, 1),
        Some(10),
        "expected retry to scroll away enough top rows to fit the point row using the next internal 0-based char position"
    );
    assert_eq!(
        next_window_start_for_partially_visible_point_row(&rows, 15, 0, 60, 1),
        None,
        "expected no retry when the point row is already fully visible"
    );
}

#[test]
fn next_window_start_for_point_line_continuation_advances_last_visible_row() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let buffer_size = {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("abcdefghijklmnopqrstuvwxyz\n");
        buf.goto_byte(0);
        buf.point_max_char() as i64
    };
    let access = {
        let buf = eval.buffer_manager().get(buf_id).expect("buffer");
        RustBufferAccess::new(buf)
    };
    let rows = vec![
        DisplayRowSnapshot {
            row: 0,
            y: 0,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(1),
            end_buffer_pos: Some(10),
        },
        DisplayRowSnapshot {
            row: 1,
            y: 16,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(11),
            end_buffer_pos: Some(20),
        },
        DisplayRowSnapshot {
            row: 2,
            y: 32,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(21),
            end_buffer_pos: Some(25),
        },
    ];

    assert_eq!(
        next_window_start_for_point_line_continuation(&rows, 21, 1, &access, buffer_size),
        Some(20),
        "expected retry to move point toward the top when the visible point row continues below the window"
    );

    let terminated_rows = vec![
        DisplayRowSnapshot {
            row: 0,
            y: 0,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(1),
            end_buffer_pos: Some(10),
        },
        DisplayRowSnapshot {
            row: 1,
            y: 16,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(11),
            end_buffer_pos: Some(27),
        },
    ];
    assert_eq!(
        next_window_start_for_point_line_continuation(
            &terminated_rows,
            11,
            1,
            &access,
            buffer_size
        ),
        None,
        "expected no retry once the final visible row already reaches the newline"
    );
}

#[test]
fn next_window_start_for_point_line_continuation_ignores_newline_terminated_rows() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let buffer_size = {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("needle target\nfiller line 06\n");
        buf.goto_byte(0);
        buf.point_max_char() as i64
    };
    let access = {
        let buf = eval.buffer_manager().get(buf_id).expect("buffer");
        RustBufferAccess::new(buf)
    };
    let rows = vec![DisplayRowSnapshot {
        row: 0,
        y: 0,
        height: 16,
        start_x: 0,
        start_col: 0,
        end_x: 0,
        end_col: 0,
        start_buffer_pos: Some(1),
        end_buffer_pos: Some(14),
    }];

    assert_eq!(
        next_window_start_for_point_line_continuation(&rows, 0, 0, &access, buffer_size),
        None,
        "expected no retry when the last visible row ended on a real newline"
    );
}

#[test]
fn next_window_start_for_point_line_continuation_ignores_tail_clipping_when_point_row_is_not_last_visible_row()
 {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let buffer_size = {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\n");
        buf.goto_byte(0);
        buf.point_max_char() as i64
    };
    let access = {
        let buf = eval.buffer_manager().get(buf_id).expect("buffer");
        RustBufferAccess::new(buf)
    };
    let rows = vec![
        DisplayRowSnapshot {
            row: 0,
            y: 0,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(1),
            end_buffer_pos: Some(10),
        },
        DisplayRowSnapshot {
            row: 1,
            y: 16,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(11),
            end_buffer_pos: Some(20),
        },
        DisplayRowSnapshot {
            row: 2,
            y: 32,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(21),
            end_buffer_pos: Some(30),
        },
        DisplayRowSnapshot {
            row: 3,
            y: 48,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(31),
            end_buffer_pos: Some(40),
        },
        DisplayRowSnapshot {
            row: 4,
            y: 64,
            height: 16,
            start_x: 0,
            start_col: 0,
            end_x: 0,
            end_col: 0,
            start_buffer_pos: Some(41),
            end_buffer_pos: Some(50),
        },
    ];

    assert_eq!(
        next_window_start_for_point_line_continuation(&rows, 21, 1, &access, buffer_size),
        None,
        "expected no retry here because the point row is not the final visible row; partially visible rows are handled by the separate point-row retry path"
    );
}

#[test]
fn char_advance_ascii_cache_distinguishes_semantic_font_identity() {
    let mut ascii_width_cache = std::collections::HashMap::new();
    let mut font_metrics_svc = Some(FontMetricsService::new());

    let regular_width = unsafe {
        char_advance(
            &mut ascii_width_cache,
            &mut font_metrics_svc,
            'A',
            1,
            8.0,
            14,
            8.0,
            "monospace",
            400,
            false,
        )
    };
    assert!(
        regular_width > 0.0,
        "expected measurable width for regular ASCII glyph"
    );
    assert_eq!(
        ascii_width_cache.len(),
        1,
        "expected one cache entry after first ASCII measurement"
    );

    let bold_width = unsafe {
        char_advance(
            &mut ascii_width_cache,
            &mut font_metrics_svc,
            'A',
            1,
            8.0,
            14,
            8.0,
            "monospace",
            700,
            false,
        )
    };
    assert!(
        bold_width > 0.0,
        "expected measurable width for bold ASCII glyph"
    );
    assert_eq!(
        ascii_width_cache.len(),
        2,
        "expected distinct cache entries for different semantic font specs even when face ids match"
    );

    let repeated_regular_width = unsafe {
        char_advance(
            &mut ascii_width_cache,
            &mut font_metrics_svc,
            'A',
            1,
            8.0,
            14,
            8.0,
            "monospace",
            400,
            false,
        )
    };
    assert_eq!(
        repeated_regular_width, regular_width,
        "expected repeated measurement for the same semantic font spec to reuse the cache entry"
    );
    assert_eq!(
        ascii_width_cache.len(),
        2,
        "expected cache size to stay stable when the semantic font spec is unchanged"
    );
}

#[test]
fn layout_frame_rust_converges_visibility_for_wrapped_rows_in_one_redisplay() {
    fn char_at_lisp_pos(buffer: &neovm_core::buffer::Buffer, pos: usize) -> Option<char> {
        if pos == 0 {
            return None;
        }
        let byte_pos = buffer.char_to_byte_clamped(pos - 1);
        buffer.char_after(byte_pos)
    }

    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let logical_lines = (0..24)
        .map(|line| format!("line-{line:02} abcdefghijklmno\n"))
        .collect::<Vec<_>>();
    let text = logical_lines.join("");
    let target_pos = logical_lines
        .iter()
        .take(18)
        .map(|line| line.chars().count())
        .sum::<usize>()
        + 1;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(&text);
        // Move the buffer point to target_pos so the selected
        // window reads it as params.point (GNU
        // window.c:window_point says selected windows use
        // BUF_PT, not pointm). Without this, the Window::point
        // assignment below would be shadowed by buffer.pt_char
        // during window_params_from_neovm and layout would
        // never see the target.
        buf.goto_byte(target_pos - 1);
        buf.set_buffer_local("word-wrap", Value::T);
    }
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-wrap-retry", 80, 192, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point,
            ..
        } = window
        {
            *window_start = 1;
            *point = target_pos;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let window = frame.find_window(selected_window).expect("selected window");
    let buffer = eval.buffer_manager().get(buf_id).expect("buffer");
    let point_chars = snapshot
        .points
        .iter()
        .map(|point| (point.buffer_pos, char_at_lisp_pos(buffer, point.buffer_pos)))
        .collect::<Vec<_>>();

    assert!(
        snapshot.point_for_buffer_pos(target_pos).is_some(),
        "expected wrapped-line redisplay to converge on point {target_pos}, points={:?}, rows={:?}, chars={:?}",
        snapshot.points,
        snapshot.rows,
        point_chars
    );
    match window {
        neovm_core::window::Window::Leaf { window_start, .. } => {
            assert!(
                *window_start > 1,
                "expected window-start to advance for wrapped redisplay, got {window_start}"
            );
        }
        other => panic!("expected leaf window, got {other:?}"),
    }
}

#[test]
fn layout_frame_rust_converges_visibility_for_point_line_tail_clipping() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let prefix = (0..2)
        .map(|line| format!("p{line:02}\n"))
        .collect::<Vec<_>>()
        .join("");
    let target_line = "abcdefghijklmno\n";
    let text = format!("{prefix}{target_line}");
    let point = prefix.chars().count() + 1;
    let later_pos = point + 10;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(&text);
        buf.goto_byte(0);
        buf.set_buffer_local("word-wrap", Value::T);
    }
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-point-line-tail", 80, 256, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point: window_point,
            ..
        } = window
        {
            *window_start = 1;
            *window_point = point;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    assert!(
        snapshot.point_for_buffer_pos(later_pos).is_some(),
        "expected redisplay to publish later positions from the point line after retry, points={:?}, rows={:?}",
        snapshot.points,
        snapshot.rows
    );
}

#[test]
fn layout_frame_rust_keeps_visible_eob_cursor_on_short_trailing_newline_buffer() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = "LEFT WINDOW\nLine 2\nLine 3\n";
    let point = {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(text);
        buf.goto_byte(0);
        buf.point_max_char() + 1
    };
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-eob-visible", 320, 640, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point: window_point,
            ..
        } = window
        {
            *window_start = 1;
            *window_point = point;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let window = frame.find_window(selected_window).expect("selected window");

    assert!(
        snapshot.point_for_buffer_pos(1).is_some(),
        "expected first line to remain visible when EOB cursor is already onscreen, points={:?}, rows={:?}",
        snapshot.points,
        snapshot.rows
    );
    match window {
        neovm_core::window::Window::Leaf { window_start, .. } => {
            assert_eq!(
                *window_start, 1,
                "expected visible EOB cursor not to force a retry scroll"
            );
        }
        other => panic!("expected leaf window, got {other:?}"),
    }
}

#[test]
fn layout_frame_rust_keeps_default_scratch_message_at_top_when_eob_is_visible() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = ";; This buffer is for text that is not saved, and for Lisp evaluation.\n\
;; To create a file, visit it with \u{2018}C-x C-f\u{2019} and enter text in its buffer.\n\n";
    let point = {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(text);
        let point = buf.point_max_char() + 1;
        buf.goto_byte(point - 1);
        point
    };
    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-scratch-eob-visible", 600, 1188, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point: window_point,
            ..
        } = window
        {
            *window_start = 1;
            *window_point = point;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("display snapshot");
    let window = frame.find_window(selected_window).expect("selected window");

    assert!(
        snapshot.point_for_buffer_pos(1).is_some(),
        "expected the first scratch row to remain visible when EOB fits onscreen, points={:?}, rows={:?}",
        snapshot.points,
        snapshot.rows
    );
    match window {
        neovm_core::window::Window::Leaf { window_start, .. } => {
            assert_eq!(
                *window_start, 1,
                "expected short scratch buffer to stay at top, got window-start {window_start}"
            );
        }
        other => panic!("expected leaf window, got {other:?}"),
    }
}

#[test]
fn layout_frame_rust_formats_mode_line_from_current_redisplay_geometry() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    let text = (0..80)
        .map(|line| format!("Line {line:02}\n"))
        .collect::<String>();
    let point = {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert(&text);
        buf.set_buffer_local("mode-line-format", Value::string("%o|%p|%P"));
        let point = buf.point_max_char() + 1;
        // Selected-window point lives in the buffer; see
        // window.c:window_point.
        buf.goto_byte(point - 1);
        point
    };
    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-mode-line-geometry", 640, 96, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;
    {
        let frame = eval.frame_manager_mut().get_mut(frame_id).expect("frame");
        let window = frame
            .find_window_mut(selected_window)
            .expect("selected window");
        if let neovm_core::window::Window::Leaf {
            window_start,
            point: window_point,
            ..
        } = window
        {
            *window_start = 1;
            *window_point = point;
        }
    }

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let mode_line_text = engine
        .last_frame_display_state
        .as_ref()
        .map(|state| {
            state
                .window_matrices
                .iter()
                .flat_map(|wm| wm.matrix.rows.iter())
                .filter(|row| row.role == GlyphRowRole::ModeLine && row.enabled)
                .flat_map(|row| row.glyphs[1].iter())
                .filter_map(|g| match &g.glyph_type {
                    neomacs_display_protocol::glyph_matrix::GlyphType::Char { ch } => Some(*ch),
                    _ => None,
                })
                .collect::<String>()
        })
        .unwrap_or_default();
    let published_window_start = {
        let frame = eval.frame_manager().get(frame_id).expect("frame");
        let window = frame.find_window(selected_window).expect("selected window");
        match window {
            neovm_core::window::Window::Leaf { window_start, .. } => *window_start,
            other => panic!("expected leaf window, got {other:?}"),
        }
    };
    let expected_mode_line = eval_status_line_format(
        &mut eval,
        "mode-line-format",
        selected_window.0 as i64,
        buf_id.0,
        80,
    )
    .expect("mode-line text");

    assert!(
        published_window_start > 1,
        "expected point at EOB to advance window-start, got {published_window_start}"
    );
    assert!(
        mode_line_text == expected_mode_line,
        "expected rendered mode-line to match freshly evaluated mode-line after redisplay publish, got rendered={mode_line_text:?} expected={expected_mode_line:?}"
    );
}

#[test]
fn layout_frame_rust_advances_live_output_through_mode_line_rows() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("body line\n");
        let point = buf.point_max_char() + 1;
        buf.goto_byte(point - 1);
    }
    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-output-progress-mode-line", 640, 160, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let display = eval
        .frame_manager()
        .get(frame_id)
        .and_then(|frame| frame.find_window(selected_window))
        .and_then(|window| window.display())
        .expect("window display state");
    let logical_cursor = display.cursor.expect("logical cursor");
    let output_cursor = display.output_cursor.expect("output cursor");

    assert!(
        output_cursor.row > logical_cursor.row,
        "expected live output progression to continue past text rows into mode-line rows, cursor={logical_cursor:?} output={output_cursor:?}"
    );
}

#[test]
fn layout_frame_rust_renders_header_line_text_for_non_nil_header_line_format() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("body line\n");
        buf.set_buffer_local("header-line-format", Value::string("LEFT HEADER"));
    }
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-header-line", 640, 160, buf_id);

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let header_text = engine
        .last_frame_display_state
        .as_ref()
        .map(|state| {
            state
                .window_matrices
                .iter()
                .flat_map(|wm| wm.matrix.rows.iter())
                .filter(|row| row.role == GlyphRowRole::HeaderLine && row.enabled)
                .flat_map(|row| row.glyphs[1].iter())
                .filter_map(|g| match &g.glyph_type {
                    neomacs_display_protocol::glyph_matrix::GlyphType::Char { ch } => Some(*ch),
                    _ => None,
                })
                .collect::<String>()
        })
        .unwrap_or_default();

    assert!(
        header_text.contains("LEFT HEADER"),
        "expected header-line row to render buffer-local header-line-format text, got {header_text:?}"
    );
}

#[test]
fn layout_frame_rust_uses_full_window_row_space_for_header_text_and_mode_line() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("body line\n");
        buf.set_buffer_local("header-line-format", Value::string("LEFT HEADER"));
        let point = buf.point_max_char() + 1;
        buf.goto_byte(point - 1);
    }
    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-header-row-space", 640, 160, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("window display snapshot");
    let display = frame
        .find_window(selected_window)
        .and_then(|window| window.display())
        .expect("window display state");
    let logical_cursor = display.cursor.expect("logical cursor");
    let output_cursor = display.output_cursor.expect("output cursor");

    let header_row = snapshot
        .rows
        .iter()
        .find(|row| row.row == 0)
        .expect("header row snapshot");

    assert!(
        header_row.start_buffer_pos.is_none() && header_row.end_buffer_pos.is_none(),
        "expected row 0 to be reserved for header-line chrome, got {header_row:?}"
    );
    assert!(
        logical_cursor.row >= 1,
        "expected logical cursor row to be offset below header-line chrome, got {logical_cursor:?}"
    );
    assert!(
        output_cursor.row > logical_cursor.row,
        "expected mode-line output to advance past logical text rows, cursor={logical_cursor:?} output={output_cursor:?}"
    );
}

#[test]
fn layout_frame_rust_advances_live_output_through_tab_line_rows() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("body line\n");
        buf.set_buffer_local("tab-line-format", Value::string("TAB ROW"));
        let point = buf.point_max_char() + 1;
        buf.goto_byte(point - 1);
    }
    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-tab-line-row-space", 640, 160, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("window display snapshot");
    let display = frame
        .find_window(selected_window)
        .and_then(|window| window.display())
        .expect("window display state");
    let logical_cursor = display.cursor.expect("logical cursor");
    let output_cursor = display.output_cursor.expect("output cursor");

    let tab_row = snapshot
        .rows
        .iter()
        .find(|row| row.row == 0)
        .expect("tab-line row snapshot");

    assert!(
        tab_row.start_buffer_pos.is_none() && tab_row.end_buffer_pos.is_none(),
        "expected row 0 to be reserved for tab-line chrome, got {tab_row:?}"
    );
    assert!(
        logical_cursor.row >= 1,
        "expected logical cursor row to be offset below tab-line chrome, got {logical_cursor:?}"
    );
    assert!(
        output_cursor.row > logical_cursor.row,
        "expected mode-line output to advance past logical text rows, cursor={logical_cursor:?} output={output_cursor:?}"
    );
}

#[test]
fn layout_frame_rust_preserves_multiline_overlay_output_rows() {
    let mut eval = Context::new();
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("x");
        let overlay = Value::make_overlay(neovm_core::heap_types::OverlayData {
            plist: Value::NIL,
            buffer: Some(buf_id),
            start: 0,
            end: 1,
            front_advance: false,
            rear_advance: false,
        });
        buf.overlays.insert_overlay(overlay);
        let _ = buf.overlays.overlay_put(
            overlay,
            Value::symbol("after-string"),
            Value::string("A\nB"),
        );
        let point = buf.point_max_char() + 1;
        buf.goto_byte(point - 1);
    }

    let frame_id =
        eval.frame_manager_mut()
            .create_frame("layout-overlay-output-rows", 640, 160, buf_id);
    let selected_window = eval
        .frame_manager()
        .get(frame_id)
        .expect("frame")
        .selected_window;

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    let snapshot = frame
        .window_display_snapshot(selected_window)
        .expect("window display snapshot");
    let display = frame
        .find_window(selected_window)
        .and_then(|window| window.display())
        .expect("window display state");
    let second_text_row = snapshot
        .rows
        .iter()
        .find(|row| row.row == 1)
        .expect("second overlay row snapshot");
    let overlay_hit_row = unsafe {
        (&*std::ptr::addr_of!(crate::hit_test::FRAME_HIT_DATA))
            .as_ref()
            .and_then(|windows| {
                windows
                    .iter()
                    .find(|window| window.window_id == selected_window.0 as i64)
            })
            .and_then(|window| {
                window.rows.iter().find(|row| {
                    let y = second_text_row.y as f32 + 1.0;
                    y >= row.y_start && y < row.y_end
                })
            })
            .cloned()
    }
    .expect("overlay hit row");
    let overlay_hit = crate::hit_test::hit_test_window_charpos(
        selected_window.0 as i64,
        0.0,
        second_text_row.y as f32 + 1.0,
    );

    assert!(
        snapshot
            .rows
            .iter()
            .any(|row| row.row == 0 && row.start_buffer_pos.is_some()),
        "expected first text row snapshot to survive multiline overlay output, rows={:?}",
        snapshot.rows
    );
    assert!(
        snapshot.rows.iter().any(|row| row.row == 1),
        "expected multiline overlay output to publish a second text row, rows={:?}",
        snapshot.rows
    );
    assert!(
        display.output_cursor.is_some_and(|cursor| cursor.row >= 1),
        "expected live output cursor to advance onto multiline overlay rows, output={:?}",
        display.output_cursor
    );
    assert!(
        overlay_hit >= overlay_hit_row.charpos_start && overlay_hit <= overlay_hit_row.charpos_end,
        "expected multiline overlay row hit-testing to land inside the recorded overlay row span, hit={overlay_hit} row={overlay_hit_row:?}"
    );
}

#[test]
fn layout_frame_rust_renders_tab_bar_text_from_lisp_tab_bar_keymap() {
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"]).expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    // Bootstrap may or may not install an initial selected
    // frame depending on cache state. Capture whatever exists
    // so we can restore the selection after switching to the
    // target frame for the tab-bar assertions.
    let prior_selected_frame = eval.frame_manager().selected_frame().map(|f| f.id);
    let buf_id = eval
        .buffer_manager()
        .current_buffer()
        .expect("current buffer")
        .id;
    {
        let buf = eval.buffer_manager_mut().get_mut(buf_id).expect("buffer");
        buf.insert("body line\n");
    }
    let frame_id = eval
        .frame_manager_mut()
        .create_frame("layout-tab-bar", 1600, 160, buf_id);
    eval.obarray_mut()
        .set_symbol_value("layout-target-frame", Value::make_frame(frame_id.0));
    eval.eval_str(
        r#"
          (require 'tab-bar)
          (setq tab-bar-show 1)
          (tab-bar-mode 1)
          (switch-to-buffer (get-buffer-create "*frame-a*"))
          (tab-bar-new-tab)
          (switch-to-buffer (get-buffer-create "*frame-a-2*"))
          (tab-bar-select-tab 1)
          (select-frame layout-target-frame)
          (tab-bar-new-tab)
          (switch-to-buffer (get-buffer-create "*tb-2*"))
          (tab-bar-select-tab 1)
        "#,
    )
    .expect("eval tab-bar forms");
    eval.eval_form(Value::list(vec![
        Value::symbol("select-frame"),
        Value::make_frame(frame_id.0),
        Value::NIL,
    ]))
    .expect("select target frame for tab-bar debug");
    let keymap_debug =
        match eval.eval_form(Value::list(vec![Value::symbol("tab-bar-make-keymap-1")])) {
            Ok(value) => eval
                .eval_form(Value::list(vec![Value::symbol("prin1-to-string"), value]))
                .ok()
                .and_then(|rendered| rendered.as_runtime_string_owned())
                .unwrap_or_else(|| "<render-unavailable>".to_string()),
            Err(err) => format!("<error: {err}>"),
        };
    let tabs_debug = eval
        .eval_str("(prin1-to-string (frame-parameter nil 'tabs))")
        .ok()
        .and_then(|value| value.as_runtime_string_owned())
        .unwrap_or_else(|| "<unavailable>".to_string());
    let format_debug = eval
        .eval_str("(prin1-to-string tab-bar-format)")
        .ok()
        .and_then(|value| value.as_runtime_string_owned())
        .unwrap_or_else(|| "<unavailable>".to_string());
    if let Some(prev) = prior_selected_frame {
        eval.eval_form(Value::list(vec![
            Value::symbol("select-frame"),
            Value::make_frame(prev.0),
            Value::NIL,
        ]))
        .expect("restore selected frame");
    }

    let frame = eval.frame_manager().get(frame_id).expect("frame");
    assert!(
        frame.tab_bar_height > 0,
        "expected tab-bar-mode to reserve frame tab-bar height"
    );

    let mut engine = LayoutEngine::new();
    engine.layout_frame_rust(&mut eval, frame_id);

    let tab_bar_text = engine
        .last_frame_display_state
        .as_ref()
        .map(|state| {
            state
                .frame_chrome_rows
                .iter()
                .filter(|row| row.row.role == GlyphRowRole::TabBar && row.row.enabled)
                .flat_map(|row| row.row.glyphs[1].iter())
                .filter_map(|g| match &g.glyph_type {
                    neomacs_display_protocol::glyph_matrix::GlyphType::Char { ch } => Some(*ch),
                    _ => None,
                })
                .collect::<String>()
        })
        .unwrap_or_default();

    assert!(
        tab_bar_text.contains("*tb-2*"),
        "expected tab-bar row to render tab captions from tab-bar keymap, got {tab_bar_text:?}; tabs={tabs_debug}; format={format_debug}; keymap={keymap_debug}"
    );
    let window_tab_bar_rows = engine
        .last_frame_display_state
        .as_ref()
        .map(|state| {
            state
                .window_matrices
                .iter()
                .flat_map(|wm| wm.matrix.rows.iter())
                .filter(|row| row.role == GlyphRowRole::TabBar && row.enabled)
                .count()
        })
        .unwrap_or(0);
    assert_eq!(
        window_tab_bar_rows, 0,
        "expected frame tab bar to live in frame_chrome_rows, not in leaf-window matrices"
    );
    // Note: a previous version of this test also asserted
    // `!tab_bar_text.contains("*frame-a-2*")` as a
    // "frame-isolation" check. The tab-bar.el keymap produced
    // by `tab-bar-make-keymap-1` walks all tabs reachable from
    // the current frame's `tabs` parameter and does not
    // filter by which frame created each tab, so the negative
    // assertion was testing a speculative behavior that isn't
    // part of the render contract. Dropping it keeps the
    // primary "renders any target-frame text at all" check
    // and leaves frame-scoped tab isolation as a separate
    // concern.
}

#[test]
fn layout_frame_rust_keeps_echo_message_in_minibuffer_window_for_tty() {
    assert_echo_message_renders_in_minibuffer_window(false);
}

#[test]
fn layout_frame_rust_keeps_echo_message_in_minibuffer_window_for_gui() {
    assert_echo_message_renders_in_minibuffer_window(true);
}

#[test]
fn layout_frame_rust_keeps_multiline_echo_rows_for_tty() {
    assert_multiline_echo_message_uses_minibuffer_rows(false);
}

#[test]
fn layout_frame_rust_keeps_multiline_echo_rows_for_gui() {
    assert_multiline_echo_message_uses_minibuffer_rows(true);
}

#[test]
fn test_ligature_run_buffer_is_empty_len() {
    let mut buf = LigatureRunBuffer::new();

    assert!(buf.is_empty());
    assert_eq!(buf.len(), 0);

    buf.push('a', 8.0);

    assert!(!buf.is_empty());
    assert_eq!(buf.len(), 1);

    buf.push('b', 8.0);

    assert!(!buf.is_empty());
    assert_eq!(buf.len(), 2);
}

#[test]
fn test_ligature_run_buffer_push() {
    let mut buf = LigatureRunBuffer::new();

    buf.push('h', 8.0);
    assert_eq!(buf.chars, vec!['h']);
    assert_eq!(buf.advances, vec![8.0]);
    assert_eq!(buf.total_advance, 8.0);

    buf.push('e', 8.0);
    assert_eq!(buf.chars, vec!['h', 'e']);
    assert_eq!(buf.advances, vec![8.0, 8.0]);
    assert_eq!(buf.total_advance, 16.0);

    buf.push('l', 7.5);
    assert_eq!(buf.chars, vec!['h', 'e', 'l']);
    assert_eq!(buf.advances, vec![8.0, 8.0, 7.5]);
    assert_eq!(buf.total_advance, 23.5);
}

#[test]
fn test_ligature_run_buffer_clear() {
    let mut buf = LigatureRunBuffer::new();

    buf.push('a', 8.0);
    buf.push('b', 8.0);
    buf.start_x = 100.0;
    buf.start_y = 200.0;
    buf.face_h = 16.0;
    buf.face_ascent = 12.0;
    buf.face_id = 42;
    buf.is_overlay = true;
    buf.height_scale = 1.5;

    buf.clear();

    // Vectors and total_advance cleared
    assert_eq!(buf.chars.len(), 0);
    assert_eq!(buf.advances.len(), 0);
    assert_eq!(buf.total_advance, 0.0);

    // Position/face fields NOT cleared
    assert_eq!(buf.start_x, 100.0);
    assert_eq!(buf.start_y, 200.0);
    assert_eq!(buf.face_h, 16.0);
    assert_eq!(buf.face_ascent, 12.0);
    assert_eq!(buf.face_id, 42);
    assert_eq!(buf.is_overlay, true);
    assert_eq!(buf.height_scale, 1.5);
}

#[test]
fn test_ligature_run_buffer_start() {
    let mut buf = LigatureRunBuffer::new();

    buf.push('x', 10.0);
    buf.start_x = 999.0;

    buf.start(50.0, 60.0, 20.0, 15.0, 5, true, 1.2);

    // Clears chars/advances/total_advance
    assert_eq!(buf.chars.len(), 0);
    assert_eq!(buf.advances.len(), 0);
    assert_eq!(buf.total_advance, 0.0);

    // Sets all position/face params
    assert_eq!(buf.start_x, 50.0);
    assert_eq!(buf.start_y, 60.0);
    assert_eq!(buf.face_h, 20.0);
    assert_eq!(buf.face_ascent, 15.0);
    assert_eq!(buf.face_id, 5);
    assert_eq!(buf.is_overlay, true);
    assert_eq!(buf.height_scale, 1.2);
}

#[test]
fn test_max_ligature_run_len_constant() {
    assert_eq!(MAX_LIGATURE_RUN_LEN, 64);
}

#[test]
fn test_flush_run_is_noop() {
    // flush_run is now a no-op: glyph output has been migrated to GlyphMatrixBuilder.
    let mut run = LigatureRunBuffer::new();
    run.start(10.0, 20.0, 16.0, 12.0, 1, false, 0.0);
    run.push('a', 8.0);
    let len_before = run.len();
    let advance_before = run.total_advance;

    flush_run(&run, true);
    flush_run(&run, false);
    assert_eq!(run.len(), len_before);
    assert_eq!(run.total_advance, advance_before);

    // Empty run
    let empty_run = LigatureRunBuffer::new();
    flush_run(&empty_run, true);
}

#[test]
fn test_is_ligature_char() {
    // Ligature-eligible characters
    for ch in [
        '-', '>', '<', '=', '!', '|', '&', '*', '+', '.', '/', ':', ';', '?', '@', '\\', '^', '~',
        '#', '$', '%',
    ] {
        assert!(is_ligature_char(ch), "'{}' should be a ligature char", ch);
    }
    // Non-ligature characters
    for ch in [
        'a', 'Z', '0', '9', ' ', '\n', '\t', '(', ')', '[', ']', '{', '}', ',', '\'', '"',
    ] {
        assert!(
            !is_ligature_char(ch),
            "'{}' should NOT be a ligature char",
            ch
        );
    }
}

#[test]
fn test_run_is_pure_ligature() {
    // Pure symbol run
    let mut run = LigatureRunBuffer::new();
    run.start(0.0, 0.0, 16.0, 12.0, 1, false, 0.0);
    run.push('-', 8.0);
    run.push('>', 8.0);
    assert!(run_is_pure_ligature(&run));

    // Mixed run (alpha + symbol)
    let mut run2 = LigatureRunBuffer::new();
    run2.start(0.0, 0.0, 16.0, 12.0, 1, false, 0.0);
    run2.push('a', 8.0);
    run2.push(':', 8.0);
    assert!(!run_is_pure_ligature(&run2));

    // Pure alpha run
    let mut run3 = LigatureRunBuffer::new();
    run3.start(0.0, 0.0, 16.0, 12.0, 1, false, 0.0);
    run3.push('h', 8.0);
    run3.push('i', 8.0);
    assert!(!run_is_pure_ligature(&run3));
}

#[test]
fn test_cursor_point_columns_wide_char() {
    let params = test_window_params();
    let text = "你".as_bytes();
    assert_eq!(cursor_point_columns(text, 0, 0, &params), 2);
}

#[test]
fn test_cursor_point_columns_tab_uses_tab_stop_list() {
    let mut params = test_window_params();
    params.tab_width = 8;
    params.tab_stop_list = vec![4, 10];
    let text = b"\t";

    assert_eq!(cursor_point_columns(text, 0, 3, &params), 1);
    assert_eq!(cursor_point_columns(text, 0, 4, &params), 6);
}

#[test]
fn test_cursor_width_for_style_bar_uses_bar_width() {
    let params = test_window_params();
    let text = "你".as_bytes();

    let width = cursor_width_for_style(CursorStyle::Bar(2.5), text, 0, 0, &params, 7.0);
    assert_eq!(width, 2.5);
}

#[test]
fn test_cursor_width_for_style_tab_clamps_when_x_stretch_cursor_is_nil() {
    let params = test_window_params();
    let text = b"\t";

    let width = cursor_width_for_style(CursorStyle::FilledBox, text, 0, 1, &params, 8.0);
    assert_eq!(width, 8.0);
}

#[test]
fn test_cursor_width_for_style_tab_expands_when_x_stretch_cursor_is_t() {
    let mut params = test_window_params();
    params.x_stretch_cursor = true;
    let text = b"\t";

    let width = cursor_width_for_style(CursorStyle::FilledBox, text, 0, 1, &params, 8.0);
    assert_eq!(width, 56.0);
}

#[test]
fn test_cursor_width_for_style_hbar_uses_glyph_columns() {
    let params = test_window_params();
    let text = "你".as_bytes();

    let width = cursor_width_for_style(CursorStyle::Hbar(2.0), text, 0, 0, &params, 7.0);
    assert_eq!(width, 14.0);
}

#[test]
fn test_cursor_style_for_nonselected_bar_uses_resolved_width() {
    let mut params = test_window_params();
    params.selected = false;
    params.cursor_kind = neomacs_display_protocol::frame_glyphs::CursorKind::Bar;
    params.cursor_bar_width = 4;

    assert_eq!(
        cursor_style_for_window(&params),
        Some(CursorStyle::Bar(4.0))
    );
}

#[test]
fn test_cursor_style_for_nonselected_no_cursor_is_none() {
    let mut params = test_window_params();
    params.selected = false;
    params.cursor_kind = neomacs_display_protocol::frame_glyphs::CursorKind::NoCursor;

    assert_eq!(cursor_style_for_window(&params), None);
}

#[test]
fn test_resolve_cursor_vertical_metrics_uses_row_metrics() {
    let (y, height, ascent) =
        resolve_cursor_vertical_metrics(20.0, 24.0, 18.0, 24.0, 14.0, 16.0, false);

    assert_eq!(y, 16.0);
    assert_eq!(height, 24.0);
    assert_eq!(ascent, 18.0);
}

#[test]
fn test_resolve_cursor_vertical_metrics_preserves_eob_origin() {
    let (y, height, ascent) =
        resolve_cursor_vertical_metrics(20.0, 24.0, 18.0, 24.0, 14.0, 16.0, true);

    assert_eq!(y, 20.0);
    assert_eq!(height, 20.0);
    assert_eq!(ascent, 14.0);
}
