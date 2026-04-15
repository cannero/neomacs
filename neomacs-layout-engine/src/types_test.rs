use super::*;

// --- Color and Rect helpers ---

fn test_color() -> Color {
    Color::new(0.5, 0.6, 0.7, 1.0)
}

fn test_rect() -> Rect {
    Rect::new(10.0, 20.0, 800.0, 600.0)
}

// --- LayoutOutput ---

#[test]
fn layout_output_construction() {
    let output = LayoutOutput {
        width: 1920.0,
        height: 1080.0,
        background: test_color(),
        char_width: 8.0,
        char_height: 16.0,
        font_pixel_size: 14.0,
        windows: vec![],
    };
    assert_eq!(output.width, 1920.0);
    assert_eq!(output.height, 1080.0);
    assert_eq!(output.char_width, 8.0);
    assert_eq!(output.char_height, 16.0);
    assert_eq!(output.font_pixel_size, 14.0);
    assert!(output.windows.is_empty());
}

#[test]
fn layout_output_with_windows() {
    let window = WindowLayout {
        window_id: 1,
        buffer_id: 100,
        bounds: test_rect(),
        selected: true,
        window_start: 1,
        mode_line_height: 20.0,
        rows: vec![],
        cursor: None,
        window_end_pos: 500,
    };
    let output = LayoutOutput {
        width: 1920.0,
        height: 1080.0,
        background: test_color(),
        char_width: 8.0,
        char_height: 16.0,
        font_pixel_size: 14.0,
        windows: vec![window],
    };
    assert_eq!(output.windows.len(), 1);
    assert_eq!(output.windows[0].window_id, 1);
}

#[test]
fn layout_output_clone() {
    let output = LayoutOutput {
        width: 800.0,
        height: 600.0,
        background: Color::rgb(1.0, 0.0, 0.0),
        char_width: 7.0,
        char_height: 14.0,
        font_pixel_size: 12.0,
        windows: vec![],
    };
    let cloned = output.clone();
    assert_eq!(cloned.width, output.width);
    assert_eq!(cloned.height, output.height);
    assert_eq!(cloned.char_width, output.char_width);
}

// --- WindowLayout ---

#[test]
fn window_layout_construction() {
    let wl = WindowLayout {
        window_id: 42,
        buffer_id: 99,
        bounds: Rect::new(0.0, 0.0, 400.0, 300.0),
        selected: false,
        window_start: 100,
        mode_line_height: 18.0,
        rows: vec![],
        cursor: None,
        window_end_pos: 500,
    };
    assert_eq!(wl.window_id, 42);
    assert_eq!(wl.buffer_id, 99);
    assert!(!wl.selected);
    assert_eq!(wl.window_start, 100);
    assert_eq!(wl.mode_line_height, 18.0);
    assert!(wl.rows.is_empty());
    assert!(wl.cursor.is_none());
    assert_eq!(wl.window_end_pos, 500);
}

#[test]
fn window_layout_with_cursor() {
    let cursor = CursorLayout {
        x: 100.0,
        y: 200.0,
        width: 8.0,
        height: 16.0,
        style: 0, // filled box
        color: Color::rgb(1.0, 1.0, 1.0),
        char_under: Some('A'),
        char_face_id: Some(5),
    };
    let wl = WindowLayout {
        window_id: 1,
        buffer_id: 1,
        bounds: test_rect(),
        selected: true,
        window_start: 1,
        mode_line_height: 20.0,
        rows: vec![],
        cursor: Some(cursor),
        window_end_pos: 100,
    };
    assert!(wl.cursor.is_some());
    let c = wl.cursor.unwrap();
    assert_eq!(c.style, 0);
    assert_eq!(c.char_under, Some('A'));
}

#[test]
fn window_layout_with_rows() {
    let row = LayoutRow {
        glyphs: vec![],
        y: 0.0,
        height: 16.0,
        ascent: 12.0,
        is_mode_line: false,
    };
    let wl = WindowLayout {
        window_id: 1,
        buffer_id: 1,
        bounds: test_rect(),
        selected: true,
        window_start: 1,
        mode_line_height: 0.0,
        rows: vec![row],
        cursor: None,
        window_end_pos: 80,
    };
    assert_eq!(wl.rows.len(), 1);
    assert_eq!(wl.rows[0].height, 16.0);
    assert!(!wl.rows[0].is_mode_line);
}

// --- LayoutRow ---

#[test]
fn layout_row_construction() {
    let row = LayoutRow {
        glyphs: vec![],
        y: 50.0,
        height: 20.0,
        ascent: 15.0,
        is_mode_line: true,
    };
    assert!(row.glyphs.is_empty());
    assert_eq!(row.y, 50.0);
    assert_eq!(row.height, 20.0);
    assert_eq!(row.ascent, 15.0);
    assert!(row.is_mode_line);
}

#[test]
fn layout_row_with_mixed_glyphs() {
    let char_glyph = LayoutGlyph::Char {
        ch: 'H',
        x: 0.0,
        width: 8.0,
        face_id: 0,
        charpos: 1,
    };
    let stretch_glyph = LayoutGlyph::Stretch {
        x: 8.0,
        width: 40.0,
        face_id: 1,
    };
    let image_glyph = LayoutGlyph::Image {
        image_id: 7,
        x: 48.0,
        width: 100.0,
        height: 80.0,
    };
    let row = LayoutRow {
        glyphs: vec![char_glyph, stretch_glyph, image_glyph],
        y: 0.0,
        height: 80.0,
        ascent: 60.0,
        is_mode_line: false,
    };
    assert_eq!(row.glyphs.len(), 3);
}

// --- LayoutGlyph variants ---

#[test]
fn layout_glyph_char_variant() {
    let g = LayoutGlyph::Char {
        ch: 'Z',
        x: 120.0,
        width: 9.5,
        face_id: 3,
        charpos: 42,
    };
    if let LayoutGlyph::Char {
        ch,
        x,
        width,
        face_id,
        charpos,
    } = g
    {
        assert_eq!(ch, 'Z');
        assert_eq!(x, 120.0);
        assert_eq!(width, 9.5);
        assert_eq!(face_id, 3);
        assert_eq!(charpos, 42);
    } else {
        panic!("Expected LayoutGlyph::Char");
    }
}

#[test]
fn layout_glyph_stretch_variant() {
    let g = LayoutGlyph::Stretch {
        x: 200.0,
        width: 50.0,
        face_id: 10,
    };
    if let LayoutGlyph::Stretch { x, width, face_id } = g {
        assert_eq!(x, 200.0);
        assert_eq!(width, 50.0);
        assert_eq!(face_id, 10);
    } else {
        panic!("Expected LayoutGlyph::Stretch");
    }
}

#[test]
fn layout_glyph_image_variant() {
    let g = LayoutGlyph::Image {
        image_id: 42,
        x: 300.0,
        width: 640.0,
        height: 480.0,
    };
    if let LayoutGlyph::Image {
        image_id,
        x,
        width,
        height,
    } = g
    {
        assert_eq!(image_id, 42);
        assert_eq!(x, 300.0);
        assert_eq!(width, 640.0);
        assert_eq!(height, 480.0);
    } else {
        panic!("Expected LayoutGlyph::Image");
    }
}

#[test]
fn layout_glyph_clone() {
    let g = LayoutGlyph::Char {
        ch: 'A',
        x: 10.0,
        width: 8.0,
        face_id: 0,
        charpos: 1,
    };
    let cloned = g.clone();
    if let LayoutGlyph::Char { ch, charpos, .. } = cloned {
        assert_eq!(ch, 'A');
        assert_eq!(charpos, 1);
    } else {
        panic!("Clone should preserve variant");
    }
}

#[test]
fn layout_glyph_debug() {
    let g = LayoutGlyph::Char {
        ch: 'X',
        x: 0.0,
        width: 8.0,
        face_id: 0,
        charpos: 5,
    };
    let debug_str = format!("{:?}", g);
    assert!(debug_str.contains("Char"));
    assert!(debug_str.contains("'X'"));
}

// --- CursorLayout ---

#[test]
fn cursor_layout_all_styles() {
    for (style, name) in [(0u8, "box"), (1, "bar"), (2, "hbar"), (3, "hollow")] {
        let cursor = CursorLayout {
            x: 0.0,
            y: 0.0,
            width: 8.0,
            height: 16.0,
            style,
            color: Color::rgb(1.0, 1.0, 1.0),
            char_under: None,
            char_face_id: None,
        };
        assert_eq!(cursor.style, style, "Failed for cursor style: {}", name);
    }
}

#[test]
fn cursor_layout_with_char_under() {
    let cursor = CursorLayout {
        x: 80.0,
        y: 32.0,
        width: 10.0,
        height: 20.0,
        style: 0,
        color: Color::new(0.0, 1.0, 0.0, 1.0),
        char_under: Some('W'),
        char_face_id: Some(7),
    };
    assert_eq!(cursor.char_under, Some('W'));
    assert_eq!(cursor.char_face_id, Some(7));
}

#[test]
fn cursor_layout_without_char_under() {
    let cursor = CursorLayout {
        x: 0.0,
        y: 0.0,
        width: 2.0,
        height: 16.0,
        style: 1, // bar
        color: Color::rgb(1.0, 1.0, 1.0),
        char_under: None,
        char_face_id: None,
    };
    assert!(cursor.char_under.is_none());
    assert!(cursor.char_face_id.is_none());
}

// --- WindowParams ---

#[test]
fn window_params_construction() {
    let params = WindowParams {
        window_id: 12345,
        buffer_id: 67890,
        bounds: Rect::new(0.0, 0.0, 800.0, 600.0),
        text_bounds: Rect::new(10.0, 0.0, 780.0, 580.0),
        selected: true,
        is_minibuffer: false,
        window_start: 1,
        window_end: 0,
        point: 42,
        buffer_size: 10000,
        buffer_begv: 1,
        hscroll: 0,
        vscroll: 0,
        truncate_lines: false,
        word_wrap: true,
        tab_width: 8,
        tab_stop_list: vec![],
        default_fg: 0x00FFFFFF,
        default_bg: 0x00000000,
        char_width: 8.0,
        char_height: 16.0,
        font_pixel_size: 14.0,
        font_ascent: 12.0,
        mode_line_height: 20.0,
        header_line_height: 0.0,
        tab_line_height: 0.0,
        cursor_kind: neomacs_display_protocol::frame_glyphs::CursorKind::FilledBox,
        cursor_bar_width: 2,
        x_stretch_cursor: false,
        cursor_color: 0x00000000,
        left_fringe_width: 8.0,
        right_fringe_width: 8.0,
        indicate_empty_lines: 0,
        show_trailing_whitespace: false,
        trailing_ws_bg: 0,
        fill_column_indicator: 80,
        fill_column_indicator_char: '|',
        fill_column_indicator_fg: 0x00808080,
        extra_line_spacing: 0.0,
        selective_display: 0,
        escape_glyph_fg: 0x00FF0000,
        nobreak_char_display: 1,
        nobreak_char_fg: 0x0000FF00,
        glyphless_char_fg: 0x00808080,
        wrap_prefix: vec![],
        line_prefix: vec![],
        left_margin_width: 0.0,
        right_margin_width: 0.0,
    };
    assert_eq!(params.window_id, 12345);
    assert_eq!(params.buffer_id, 67890);
    assert!(params.selected);
    assert!(!params.is_minibuffer);
    assert_eq!(params.point, 42);
    assert!(params.word_wrap);
    assert!(!params.truncate_lines);
    assert_eq!(params.tab_width, 8);
    assert_eq!(params.fill_column_indicator, 80);
    assert_eq!(params.fill_column_indicator_char, '|');
}

#[test]
fn window_params_minibuffer() {
    let params = WindowParams {
        window_id: 1,
        buffer_id: 1,
        bounds: Rect::new(0.0, 580.0, 800.0, 20.0),
        text_bounds: Rect::new(0.0, 580.0, 800.0, 20.0),
        selected: true,
        is_minibuffer: true,
        window_start: 1,
        window_end: 0,
        point: 1,
        buffer_size: 0,
        buffer_begv: 1,
        hscroll: 0,
        vscroll: 0,
        truncate_lines: true,
        word_wrap: false,
        tab_width: 8,
        tab_stop_list: vec![],
        default_fg: 0x00FFFFFF,
        default_bg: 0x00000000,
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
        cursor_color: 0x00000000,
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
    };
    assert!(params.is_minibuffer);
    assert_eq!(params.mode_line_height, 0.0);
}

#[test]
fn window_params_clone() {
    let params = WindowParams {
        window_id: 1,
        buffer_id: 1,
        bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
        text_bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
        selected: false,
        is_minibuffer: false,
        window_start: 1,
        window_end: 0,
        point: 1,
        buffer_size: 100,
        buffer_begv: 1,
        hscroll: 5,
        vscroll: 0,
        truncate_lines: true,
        word_wrap: false,
        tab_width: 4,
        tab_stop_list: vec![],
        default_fg: 0,
        default_bg: 0,
        char_width: 8.0,
        char_height: 16.0,
        font_pixel_size: 14.0,
        font_ascent: 12.0,
        mode_line_height: 20.0,
        header_line_height: 20.0,
        tab_line_height: 20.0,
        cursor_kind: neomacs_display_protocol::frame_glyphs::CursorKind::Bar,
        cursor_bar_width: 3,
        x_stretch_cursor: false,
        cursor_color: 0x00000000,
        left_fringe_width: 10.0,
        right_fringe_width: 10.0,
        indicate_empty_lines: 1,
        show_trailing_whitespace: true,
        trailing_ws_bg: 0x00FF0000,
        fill_column_indicator: 0,
        fill_column_indicator_char: '|',
        fill_column_indicator_fg: 0,
        extra_line_spacing: 2.0,
        selective_display: 3,
        escape_glyph_fg: 0,
        nobreak_char_display: 2,
        nobreak_char_fg: 0,
        glyphless_char_fg: 0,
        wrap_prefix: b"  ".to_vec(),
        line_prefix: b"> ".to_vec(),
        left_margin_width: 5.0,
        right_margin_width: 5.0,
    };
    let cloned = params.clone();
    assert_eq!(cloned.window_id, params.window_id);
    assert_eq!(cloned.hscroll, 5);
    assert_eq!(cloned.tab_width, 4);
    assert!(cloned.truncate_lines);
    assert!(cloned.show_trailing_whitespace);
    assert_eq!(cloned.wrap_prefix, b"  ".to_vec());
    assert_eq!(cloned.line_prefix, b"> ".to_vec());
    assert_eq!(cloned.selective_display, 3);
    assert_eq!(cloned.extra_line_spacing, 2.0);
}

// --- FrameParams ---

#[test]
fn frame_params_construction() {
    let fp = FrameParams {
        width: 1920.0,
        height: 1080.0,
        menu_bar_height: 0.0,
        tool_bar_height: 0.0,
        tab_bar_height: 0.0,
        char_width: 8.0,
        char_height: 16.0,
        font_pixel_size: 14.0,
        background: 0x00282828,
        vertical_border_fg: 0x00808080,
        right_divider_width: 1,
        bottom_divider_width: 1,
        divider_fg: 0x00444444,
        divider_first_fg: 0x00555555,
        divider_last_fg: 0x00333333,
    };
    assert_eq!(fp.width, 1920.0);
    assert_eq!(fp.height, 1080.0);
    assert_eq!(fp.char_width, 8.0);
    assert_eq!(fp.char_height, 16.0);
    assert_eq!(fp.font_pixel_size, 14.0);
    assert_eq!(fp.background, 0x00282828);
    assert_eq!(fp.right_divider_width, 1);
    assert_eq!(fp.bottom_divider_width, 1);
}

#[test]
fn frame_params_no_dividers() {
    let fp = FrameParams {
        width: 800.0,
        height: 600.0,
        menu_bar_height: 0.0,
        tool_bar_height: 0.0,
        tab_bar_height: 0.0,
        char_width: 7.0,
        char_height: 14.0,
        font_pixel_size: 12.0,
        background: 0x00FFFFFF,
        vertical_border_fg: 0x00000000,
        right_divider_width: 0,
        bottom_divider_width: 0,
        divider_fg: 0,
        divider_first_fg: 0,
        divider_last_fg: 0,
    };
    assert_eq!(fp.right_divider_width, 0);
    assert_eq!(fp.bottom_divider_width, 0);
}

#[test]
fn frame_params_clone() {
    let fp = FrameParams {
        width: 1024.0,
        height: 768.0,
        menu_bar_height: 0.0,
        tool_bar_height: 0.0,
        tab_bar_height: 0.0,
        char_width: 9.0,
        char_height: 18.0,
        font_pixel_size: 16.0,
        background: 0x001A1A1A,
        vertical_border_fg: 0x00AAAAAA,
        right_divider_width: 2,
        bottom_divider_width: 3,
        divider_fg: 0x00BBBBBB,
        divider_first_fg: 0x00CCCCCC,
        divider_last_fg: 0x00DDDDDD,
    };
    let cloned = fp.clone();
    assert_eq!(cloned.width, fp.width);
    assert_eq!(cloned.background, fp.background);
    assert_eq!(cloned.right_divider_width, fp.right_divider_width);
    assert_eq!(cloned.divider_fg, fp.divider_fg);
}

#[test]
fn frame_params_debug() {
    let fp = FrameParams {
        width: 800.0,
        height: 600.0,
        menu_bar_height: 0.0,
        tool_bar_height: 0.0,
        tab_bar_height: 0.0,
        char_width: 8.0,
        char_height: 16.0,
        font_pixel_size: 14.0,
        background: 0,
        vertical_border_fg: 0,
        right_divider_width: 0,
        bottom_divider_width: 0,
        divider_fg: 0,
        divider_first_fg: 0,
        divider_last_fg: 0,
    };
    let debug_str = format!("{:?}", fp);
    assert!(debug_str.contains("FrameParams"));
    assert!(debug_str.contains("800"));
}
