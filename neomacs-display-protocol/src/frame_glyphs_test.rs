use super::*;

// -----------------------------------------------------------------------
// Helper: assert a Color matches expected RGBA (with tolerance for floats)
// -----------------------------------------------------------------------
fn assert_color_eq(actual: &Color, expected: &Color) {
    assert!(
        (actual.r - expected.r).abs() < 1e-5
            && (actual.g - expected.g).abs() < 1e-5
            && (actual.b - expected.b).abs() < 1e-5
            && (actual.a - expected.a).abs() < 1e-5,
        "Colors differ: actual {:?} vs expected {:?}",
        actual,
        expected,
    );
}

fn make_window_info(
    window_id: i64,
    buffer_id: u64,
    window_start: i64,
    bounds: Rect,
) -> WindowInfo {
    WindowInfo {
        window_id,
        buffer_id,
        window_start,
        window_end: window_start + 200,
        buffer_size: 10_000,
        bounds,
        mode_line_height: 20.0,
        header_line_height: 0.0,
        tab_line_height: 0.0,
        selected: false,
        is_minibuffer: false,
        char_height: 16.0,
        buffer_file_name: String::new(),
        modified: false,
    }
}

// =======================================================================
// new() - initial state
// =======================================================================

#[test]
fn new_creates_empty_buffer() {
    let buf = FrameGlyphBuffer::new();
    assert!(buf.glyphs.is_empty());
    assert!(buf.window_infos.is_empty());
    assert!(buf.faces.is_empty());
    assert!(buf.stipple_patterns.is_empty());
    assert!(buf.phys_cursor.is_none());
}

#[test]
fn new_has_correct_defaults() {
    let buf = FrameGlyphBuffer::new();
    assert_eq!(buf.width, 0.0);
    assert_eq!(buf.height, 0.0);
    assert_eq!(buf.char_width, 8.0);
    assert_eq!(buf.char_height, 16.0);
    assert_eq!(buf.font_pixel_size, 14.0);
    assert_color_eq(&buf.background, &Color::BLACK);
    assert_eq!(buf.frame_id, 0);
    assert_eq!(buf.parent_id, 0);
    assert_eq!(buf.background_alpha, 1.0);
    assert!(!buf.no_accept_focus);
}

#[test]
fn new_is_empty_and_len_zero() {
    let buf = FrameGlyphBuffer::new();
    assert!(buf.is_empty());
    assert_eq!(buf.len(), 0);
}

// =======================================================================
// with_size()
// =======================================================================

#[test]
fn with_size_sets_dimensions() {
    let buf = FrameGlyphBuffer::with_size(1920.0, 1080.0);
    assert_eq!(buf.width, 1920.0);
    assert_eq!(buf.height, 1080.0);
    // Everything else should match new()
    assert!(buf.glyphs.is_empty());
    assert_eq!(buf.char_width, 8.0);
}

// =======================================================================
// clear_all()
// =======================================================================

#[test]
fn clear_all_resets_glyphs_and_metadata() {
    let mut buf = FrameGlyphBuffer::new();

    // Populate some data
    buf.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);
    buf.add_stretch(0.0, 0.0, 100.0, 16.0, Color::RED, 0, false);
    buf.add_cursor(
        1,
        10.0,
        20.0,
        2.0,
        16.0,
        CursorStyle::Bar(2.0),
        Color::WHITE,
    );
    buf.add_window_info(
        1,
        100,
        0,
        500,
        1000,
        0.0,
        0.0,
        800.0,
        600.0,
        20.0,
        0.0,
        0.0,
        true,
        false,
        16.0,
        "test.rs".to_string(),
        false,
    );
    buf.set_phys_cursor(PhysCursor {
        window_id: 1,
        charpos: 10,
        row: 1,
        col: 2,
        slot_id: DisplaySlotId::from_pixels(1, 10.0, 20.0, buf.char_width, buf.char_height),
        x: 10.0,
        y: 20.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    });
    buf.stipple_patterns.insert(
        1,
        StipplePattern {
            width: 8,
            height: 8,
            bits: vec![0xAA; 8],
        },
    );
    assert!(!buf.glyphs.is_empty());
    assert!(!buf.window_infos.is_empty());

    buf.clear_all();

    assert!(buf.glyphs.is_empty());
    assert!(buf.window_infos.is_empty());
    assert!(buf.transition_hints.is_empty());
    assert!(buf.effect_hints.is_empty());
    assert!(buf.phys_cursor.is_none());
    assert!(buf.stipple_patterns.is_empty());
    assert!(buf.faces.is_empty());
}

#[test]
fn clear_all_preserves_frame_dimensions() {
    let mut buf = FrameGlyphBuffer::with_size(1920.0, 1080.0);
    buf.background = Color::BLUE;
    buf.add_char('X', 0.0, 0.0, 8.0, 16.0, 12.0, false);

    buf.clear_all();

    // Dimensions and background should survive clear_all
    assert_eq!(buf.width, 1920.0);
    assert_eq!(buf.height, 1080.0);
    assert_color_eq(&buf.background, &Color::BLUE);
}

#[test]
fn take_runtime_hints_drains_transition_and_effect_hints() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_transition_hint(WindowTransitionHint {
        window_id: 1,
        bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
        kind: WindowTransitionKind::Crossfade,
        effect: None,
        easing: None,
    });
    buf.add_effect_hint(WindowEffectHint::TextFadeIn {
        window_id: 1,
        bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
    });

    let (transition_hints, effect_hints) = buf.take_runtime_hints();
    assert_eq!(transition_hints.len(), 1);
    assert_eq!(effect_hints.len(), 1);
    assert!(buf.transition_hints.is_empty());
    assert!(buf.effect_hints.is_empty());
}

#[test]
fn set_face_with_font_registers_baseline_render_face() {
    let mut buf = FrameGlyphBuffer::new();
    let fg = Color::rgb(0.8, 0.7, 0.6);
    let bg = Color::rgb(0.1, 0.2, 0.3);
    let ul = Color::rgb(0.9, 0.1, 0.2);

    buf.set_face_with_font(
        42,
        fg,
        Some(bg),
        "DejaVu Sans",
        700,
        true,
        18.0,
        2,
        Some(ul),
        1,
        None,
        0,
        None,
        false,
    );

    let face = buf.faces.get(&42).expect("face entry should exist");
    assert_eq!(face.id, 42);
    assert_eq!(face.font_family, "DejaVu Sans");
    assert_eq!(face.font_size, 18.0);
    assert_eq!(face.font_weight, 700);
    assert!(face.attributes.contains(FaceAttributes::BOLD));
    assert!(face.attributes.contains(FaceAttributes::ITALIC));
    assert!(face.attributes.contains(FaceAttributes::UNDERLINE));
    assert!(face.attributes.contains(FaceAttributes::STRIKE_THROUGH));
    assert_eq!(face.underline_style, UnderlineStyle::Wave);
    assert_eq!(face.underline_color, Some(ul));
    assert_color_eq(&face.foreground, &fg);
    assert_color_eq(&face.background, &bg);
}

#[test]
fn set_face_uses_current_font_context_for_face_entry() {
    let mut buf = FrameGlyphBuffer::new();
    let fg = Color::rgb(0.4, 0.5, 0.6);

    buf.set_face_with_font(
        1, fg, None, "Iosevka", 400, false, 15.0, 0, None, 0, None, 0, None, false,
    );
    buf.set_face(2, fg, None, 600, true, 0, None, 0, None, 1, None);

    let face = buf.faces.get(&2).expect("face entry should exist");
    assert_eq!(face.font_family, "Iosevka");
    assert_eq!(face.font_size, 15.0);
    assert_eq!(face.font_weight, 600);
    assert!(face.attributes.contains(FaceAttributes::ITALIC));
    assert!(face.attributes.contains(FaceAttributes::OVERLINE));
}

// =======================================================================
// add_char()
// =======================================================================

#[test]
fn add_char_appends_char_glyph() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_char('H', 10.0, 20.0, 8.0, 16.0, 12.0, false);

    assert_eq!(buf.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Char {
            char: ch,
            x,
            y,
            width,
            height,
            ascent,
            composed,
            ..
        } => {
            assert_eq!(*ch, 'H');
            assert_eq!(*x, 10.0);
            assert_eq!(*y, 20.0);
            assert_eq!(*width, 8.0);
            assert_eq!(*height, 16.0);
            assert_eq!(*ascent, 12.0);
            assert!(!buf.glyphs[0].is_overlay());
            assert!(composed.is_none());
        }
        other => panic!("Expected Char glyph, got {:?}", other),
    }
}

#[test]
fn add_char_uses_current_face_attributes() {
    let mut buf = FrameGlyphBuffer::new();
    let fg = Color::rgb(1.0, 0.0, 0.0);
    let bg = Color::rgb(0.0, 0.0, 1.0);
    buf.set_face(
        42,
        fg,
        Some(bg),
        700,
        true,
        1,
        Some(Color::GREEN), // underline
        1,
        Some(Color::RED), // strike-through
        1,
        Some(Color::BLUE), // overline
    );
    buf.set_draw_context(1, GlyphRowRole::ModeLine, None);
    buf.add_char('X', 0.0, 0.0, 8.0, 16.0, 12.0, true);

    match &buf.glyphs[0] {
        FrameGlyph::Char {
            fg: glyph_fg,
            bg: glyph_bg,
            face_id,
            font_weight,
            italic,
            underline,
            strike_through,
            overline,
            underline_color,
            strike_through_color,
            overline_color,
            ..
        } => {
            assert_color_eq(glyph_fg, &fg);
            assert_eq!(*glyph_bg, Some(bg));
            assert_eq!(*face_id, 42);
            assert_eq!(*font_weight, 700);
            assert!(*italic);
            assert_eq!(*underline, 1);
            assert_eq!(*underline_color, Some(Color::GREEN));
            assert_eq!(*strike_through, 1);
            assert_eq!(*strike_through_color, Some(Color::RED));
            assert_eq!(*overline, 1);
            assert_eq!(*overline_color, Some(Color::BLUE));
            assert!(buf.glyphs[0].is_overlay());
        }
        other => panic!("Expected Char glyph, got {:?}", other),
    }
}

#[test]
fn add_char_multiple_appends_in_order() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);
    buf.add_char('B', 8.0, 0.0, 8.0, 16.0, 12.0, false);
    buf.add_char('C', 16.0, 0.0, 8.0, 16.0, 12.0, false);

    assert_eq!(buf.len(), 3);
    let chars: Vec<char> = buf
        .glyphs
        .iter()
        .map(|g| match g {
            FrameGlyph::Char { char: ch, .. } => *ch,
            _ => panic!("Expected Char"),
        })
        .collect();
    assert_eq!(chars, vec!['A', 'B', 'C']);
}

#[test]
fn add_char_overlay_flag() {
    let mut buf = FrameGlyphBuffer::new();
    buf.set_draw_context(1, GlyphRowRole::ModeLine, None);
    buf.add_char('M', 0.0, 0.0, 8.0, 16.0, 12.0, true);
    assert!(buf.glyphs[0].is_overlay());

    buf.set_draw_context(1, GlyphRowRole::Text, None);
    buf.add_char('N', 0.0, 0.0, 8.0, 16.0, 12.0, false);
    assert!(!buf.glyphs[1].is_overlay());
}

// =======================================================================
// add_composed_char()
// =======================================================================

#[test]
fn add_composed_char_stores_text_and_base() {
    let mut buf = FrameGlyphBuffer::new();
    // Emoji ZWJ sequence: family emoji
    let composed_text = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    buf.add_composed_char(
        composed_text,
        '\u{1F468}',
        0.0,
        0.0,
        24.0,
        16.0,
        12.0,
        false,
    );

    assert_eq!(buf.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Char {
            char: ch,
            composed,
            width,
            ..
        } => {
            assert_eq!(*ch, '\u{1F468}');
            assert!(composed.is_some());
            assert_eq!(&**composed.as_ref().unwrap(), composed_text);
            assert_eq!(*width, 24.0);
        }
        other => panic!("Expected Char glyph, got {:?}", other),
    }
}

#[test]
fn add_composed_char_uses_current_face() {
    let mut buf = FrameGlyphBuffer::new();
    let fg = Color::rgb(0.5, 0.5, 0.5);
    buf.set_face(10, fg, None, 400, false, 0, None, 0, None, 0, None);
    buf.add_composed_char("e\u{0301}", 'e', 0.0, 0.0, 8.0, 16.0, 12.0, false);

    match &buf.glyphs[0] {
        FrameGlyph::Char {
            face_id,
            fg: glyph_fg,
            bg: glyph_bg,
            ..
        } => {
            assert_eq!(*face_id, 10);
            assert_color_eq(glyph_fg, &fg);
            assert_eq!(*glyph_bg, None);
        }
        other => panic!("Expected Char glyph, got {:?}", other),
    }
}

// =======================================================================
// add_cursor()
// =======================================================================

#[test]
fn add_cursor_appends_window_cursor_visual() {
    let mut buf = FrameGlyphBuffer::new();
    let cursor_color = Color::rgb(0.0, 1.0, 0.0);
    buf.add_cursor(
        42,
        100.0,
        200.0,
        2.0,
        16.0,
        CursorStyle::Bar(2.0),
        cursor_color,
    );

    assert!(buf.glyphs.is_empty());
    assert_eq!(buf.window_cursors.len(), 1);
    let cursor = &buf.window_cursors[0];
    assert_eq!(cursor.window_id, 42);
    assert_eq!(
        cursor.slot_id,
        DisplaySlotId::from_pixels(42, 100.0, 200.0, 8.0, 16.0)
    );
    assert_eq!(cursor.x, 100.0);
    assert_eq!(cursor.y, 200.0);
    assert_eq!(cursor.width, 2.0);
    assert_eq!(cursor.height, 16.0);
    assert_eq!(cursor.style, CursorStyle::Bar(2.0));
    assert_color_eq(&cursor.color, &cursor_color);
}

#[test]
fn add_cursor_all_styles() {
    let mut buf = FrameGlyphBuffer::new();
    let c = Color::WHITE;
    buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::FilledBox, c);
    buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::Bar(2.0), c);
    buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::Hbar(2.0), c);
    buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::Hollow, c);

    assert!(buf.glyphs.is_empty());
    assert_eq!(buf.window_cursors.len(), 4);
    let expected = [
        CursorStyle::FilledBox,
        CursorStyle::Bar(2.0),
        CursorStyle::Hbar(2.0),
        CursorStyle::Hollow,
    ];
    for (i, expected_style) in expected.iter().enumerate() {
        assert_eq!(
            buf.window_cursors[i].style, *expected_style,
            "Cursor {} has wrong style",
            i
        );
    }
}

#[test]
fn cursor_visual_is_not_counted_as_overlay_glyph() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::FilledBox, Color::WHITE);
    assert!(buf.glyphs.is_empty());
    assert_eq!(buf.window_cursors.len(), 1);
}

// =======================================================================
// add_stretch()
// =======================================================================

#[test]
fn add_stretch_appends_stretch_glyph() {
    let mut buf = FrameGlyphBuffer::new();
    let bg = Color::rgb(0.2, 0.2, 0.2);
    buf.add_stretch(0.0, 100.0, 800.0, 16.0, bg, 5, false);

    assert_eq!(buf.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Stretch {
            x,
            y,
            width,
            height,
            bg: stretch_bg,
            face_id,
            stipple_id,
            stipple_fg,
            ..
        } => {
            assert_eq!(*x, 0.0);
            assert_eq!(*y, 100.0);
            assert_eq!(*width, 800.0);
            assert_eq!(*height, 16.0);
            assert_color_eq(stretch_bg, &bg);
            assert_eq!(*face_id, 5);
            assert!(!buf.glyphs[0].is_overlay());
            assert_eq!(*stipple_id, 0);
            assert!(stipple_fg.is_none());
        }
        other => panic!("Expected Stretch glyph, got {:?}", other),
    }
}

#[test]
fn add_stretch_overlay() {
    let mut buf = FrameGlyphBuffer::new();
    buf.set_draw_context(1, GlyphRowRole::ModeLine, None);
    buf.add_stretch(0.0, 0.0, 800.0, 20.0, Color::BLUE, 0, true);
    assert!(buf.glyphs[0].is_overlay());
}

#[test]
fn add_stretch_stipple_stores_pattern_info() {
    let mut buf = FrameGlyphBuffer::new();
    let bg = Color::BLACK;
    let fg = Color::WHITE;
    buf.add_stretch_stipple(0.0, 0.0, 100.0, 100.0, bg, fg, 3, false, 7);

    assert_eq!(buf.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Stretch {
            stipple_id,
            stipple_fg,
            ..
        } => {
            assert_eq!(*stipple_id, 7);
            assert_eq!(*stipple_fg, Some(fg));
        }
        other => panic!("Expected Stretch glyph, got {:?}", other),
    }
}

#[test]
fn slot_glyph_returns_matching_stretch() {
    let mut buf = FrameGlyphBuffer::new();
    buf.set_draw_context(3, GlyphRowRole::Text, None);
    buf.add_stretch(8.0, 16.0, 24.0, 16.0, Color::BLACK, 7, false);

    let slot_id = buf.glyphs[0].slot_id().expect("stretch slot id");
    let glyph = buf.slot_glyph(slot_id).expect("slot glyph");

    match glyph {
        FrameGlyph::Stretch {
            bidi_level,
            width,
            face_id,
            ..
        } => {
            assert_eq!(*bidi_level, 0);
            assert_eq!(*width, 24.0);
            assert_eq!(*face_id, 7);
            assert_eq!(glyph.bidi_level(), Some(0));
        }
        other => panic!("Expected Stretch glyph, got {:?}", other),
    }
}

// =======================================================================
// add_window_info()
// =======================================================================

#[test]
fn add_window_info_appends_metadata() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_window_info(
        0x1234,
        0xABCD,
        1,
        500,
        1000,
        10.0,
        20.0,
        780.0,
        560.0,
        22.0,
        0.0,
        0.0,
        true,
        false,
        16.0,
        "main.rs".to_string(),
        true,
    );

    assert_eq!(buf.window_infos.len(), 1);
    let info = &buf.window_infos[0];
    assert_eq!(info.window_id, 0x1234);
    assert_eq!(info.buffer_id, 0xABCD);
    assert_eq!(info.window_start, 1);
    assert_eq!(info.window_end, 500);
    assert_eq!(info.buffer_size, 1000);
    assert_eq!(info.bounds, Rect::new(10.0, 20.0, 780.0, 560.0));
    assert_eq!(info.mode_line_height, 22.0);
    assert!(info.selected);
    assert!(!info.is_minibuffer);
    assert_eq!(info.char_height, 16.0);
    assert_eq!(info.buffer_file_name, "main.rs");
    assert!(info.modified);
}

#[test]
fn add_window_info_multiple_windows() {
    let mut buf = FrameGlyphBuffer::new();

    // Two side-by-side windows
    buf.add_window_info(
        1,
        100,
        0,
        200,
        500,
        0.0,
        0.0,
        400.0,
        600.0,
        20.0,
        0.0,
        0.0,
        true,
        false,
        16.0,
        "left.rs".to_string(),
        false,
    );
    buf.add_window_info(
        2,
        200,
        0,
        300,
        800,
        400.0,
        0.0,
        400.0,
        600.0,
        20.0,
        0.0,
        0.0,
        false,
        false,
        16.0,
        "right.rs".to_string(),
        true,
    );

    assert_eq!(buf.window_infos.len(), 2);
    assert_eq!(buf.window_infos[0].window_id, 1);
    assert!(buf.window_infos[0].selected);
    assert_eq!(buf.window_infos[1].window_id, 2);
    assert!(!buf.window_infos[1].selected);
}

#[test]
fn add_window_info_minibuffer() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_window_info(
        99,
        50,
        0,
        0,
        0,
        0.0,
        580.0,
        800.0,
        20.0,
        0.0,
        0.0,
        0.0,
        false,
        true,
        16.0,
        String::new(),
        false,
    );

    let info = &buf.window_infos[0];
    assert!(info.is_minibuffer);
    assert!(!info.selected);
    assert_eq!(info.buffer_file_name, "");
}

#[test]
fn derive_transition_hint_buffer_switch_crossfade() {
    let prev = make_window_info(1, 100, 10, Rect::new(0.0, 0.0, 800.0, 600.0));
    let curr = make_window_info(1, 200, 10, Rect::new(0.0, 0.0, 800.0, 600.0));

    let hint = FrameGlyphBuffer::derive_transition_hint(&prev, &curr).unwrap();
    assert_eq!(hint.window_id, 1);
    assert_eq!(hint.bounds, curr.bounds);
    assert!(matches!(hint.kind, WindowTransitionKind::Crossfade));
}

#[test]
fn derive_transition_hint_scroll_slide() {
    let prev = make_window_info(1, 100, 10, Rect::new(0.0, 0.0, 800.0, 600.0));
    let curr = make_window_info(1, 100, 42, Rect::new(0.0, 0.0, 800.0, 600.0));

    let hint = FrameGlyphBuffer::derive_transition_hint(&prev, &curr).unwrap();
    assert_eq!(hint.window_id, 1);
    match hint.kind {
        WindowTransitionKind::ScrollSlide {
            direction,
            scroll_distance,
        } => {
            assert_eq!(direction, 1);
            assert!(scroll_distance > 0.0);
        }
        other => panic!("expected ScrollSlide, got {:?}", other),
    }
}

#[test]
fn derive_transition_hint_skips_minibuffer() {
    let prev = make_window_info(1, 100, 10, Rect::new(0.0, 0.0, 800.0, 600.0));
    let mut curr = make_window_info(1, 100, 20, Rect::new(0.0, 0.0, 800.0, 600.0));
    curr.is_minibuffer = true;

    assert!(FrameGlyphBuffer::derive_transition_hint(&prev, &curr).is_none());
}

// =======================================================================
// set_face() / set_face_with_font()
// =======================================================================

#[test]
fn set_face_affects_subsequent_chars() {
    let mut buf = FrameGlyphBuffer::new();

    // Default face
    buf.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);

    // Change face
    let red = Color::rgb(1.0, 0.0, 0.0);
    buf.set_face(5, red, None, 700, true, 0, None, 0, None, 0, None);
    buf.add_char('B', 8.0, 0.0, 8.0, 16.0, 12.0, false);

    // First char uses default face
    match &buf.glyphs[0] {
        FrameGlyph::Char {
            face_id,
            font_weight,
            italic,
            ..
        } => {
            assert_eq!(*face_id, 0);
            assert_eq!(*font_weight, 400);
            assert!(!*italic);
        }
        _ => panic!("Expected Char"),
    }

    // Second char uses newly set face
    match &buf.glyphs[1] {
        FrameGlyph::Char {
            face_id,
            font_weight,
            italic,
            fg,
            ..
        } => {
            assert_eq!(*face_id, 5);
            assert_eq!(*font_weight, 700);
            assert!(*italic);
            assert_color_eq(fg, &red);
        }
        _ => panic!("Expected Char"),
    }
}

#[test]
fn set_face_with_font_stores_font_family() {
    let mut buf = FrameGlyphBuffer::new();
    let fg = Color::WHITE;
    buf.set_face_with_font(
        7,
        fg,
        None,
        "Fira Code",
        400,
        false,
        14.0,
        0,
        None,
        0,
        None,
        0,
        None,
        false,
    );

    // current_font_family is set by set_face_with_font
    assert_eq!(buf.get_current_font_family(), "Fira Code");

    // set_face_with_font now keeps the face table coherent as well.
    assert_eq!(buf.get_face_font(7), "Fira Code");
}

#[test]
fn set_face_with_font_updates_font_size() {
    let mut buf = FrameGlyphBuffer::new();
    buf.set_face_with_font(
        1,
        Color::WHITE,
        None,
        "monospace",
        400,
        false,
        24.0,
        0,
        None,
        0,
        None,
        0,
        None,
        false,
    );
    buf.add_char('A', 0.0, 0.0, 12.0, 24.0, 18.0, false);

    match &buf.glyphs[0] {
        FrameGlyph::Char { font_size, .. } => {
            assert_eq!(*font_size, 24.0);
        }
        _ => panic!("Expected Char"),
    }
}

#[test]
fn get_face_font_reads_from_faces_map() {
    let mut buf = FrameGlyphBuffer::new();

    // No face inserted yet — falls back to "monospace"
    assert_eq!(buf.get_face_font(1), "monospace");

    // Insert faces (as layout engine's apply_face would)
    let mut face1 = Face::new(1);
    face1.font_family = "JetBrains Mono".to_string();
    buf.faces.insert(1, face1);

    assert_eq!(buf.get_face_font(1), "JetBrains Mono");
    assert_eq!(buf.get_face_font(2), "monospace"); // not inserted
}

#[test]
fn set_face_with_font_decoration_attributes() {
    let mut buf = FrameGlyphBuffer::new();
    let ul_color = Color::rgb(1.0, 1.0, 0.0);
    let st_color = Color::rgb(1.0, 0.0, 1.0);
    let ol_color = Color::rgb(0.0, 1.0, 1.0);
    buf.set_face_with_font(
        3,
        Color::WHITE,
        None,
        "monospace",
        400,
        false,
        14.0,
        2,
        Some(ul_color), // wave underline
        1,
        Some(st_color), // strike-through
        1,
        Some(ol_color), // overline
        false,
    );
    buf.add_char('D', 0.0, 0.0, 8.0, 16.0, 12.0, false);

    match &buf.glyphs[0] {
        FrameGlyph::Char {
            underline,
            underline_color,
            strike_through,
            strike_through_color,
            overline,
            overline_color,
            ..
        } => {
            assert_eq!(*underline, 2);
            assert_eq!(*underline_color, Some(ul_color));
            assert_eq!(*strike_through, 1);
            assert_eq!(*strike_through_color, Some(st_color));
            assert_eq!(*overline, 1);
            assert_eq!(*overline_color, Some(ol_color));
        }
        _ => panic!("Expected Char"),
    }
}

#[test]
fn get_current_bg_returns_current_face_bg() {
    let mut buf = FrameGlyphBuffer::new();
    assert_eq!(buf.get_current_bg(), None);

    let bg = Color::rgb(0.1, 0.2, 0.3);
    buf.set_face(
        1,
        Color::WHITE,
        Some(bg),
        400,
        false,
        0,
        None,
        0,
        None,
        0,
        None,
    );
    assert_eq!(buf.get_current_bg(), Some(bg));
}

// =======================================================================
// set_frame_identity()
// =======================================================================

#[test]
fn set_frame_identity_stores_all_fields() {
    let mut buf = FrameGlyphBuffer::new();
    let border_color = Color::rgb(0.5, 0.5, 0.5);
    buf.set_frame_identity(0x100, 0x200, 50.0, 75.0, 5, 2.0, border_color, true, 0.85);

    assert_eq!(buf.frame_id, 0x100);
    assert_eq!(buf.parent_id, 0x200);
    assert_eq!(buf.parent_x, 50.0);
    assert_eq!(buf.parent_y, 75.0);
    assert_eq!(buf.z_order, 5);
    assert_eq!(buf.border_width, 2.0);
    assert_color_eq(&buf.border_color, &border_color);
    assert!(buf.no_accept_focus);
    assert_eq!(buf.background_alpha, 0.85);
}

#[test]
fn set_frame_identity_root_frame() {
    let mut buf = FrameGlyphBuffer::new();
    buf.set_frame_identity(
        0x100,
        0, // parent_id 0 = root frame
        0.0,
        0.0,
        0,
        0.0,
        Color::BLACK,
        false,
        1.0,
    );

    assert_eq!(buf.frame_id, 0x100);
    assert_eq!(buf.parent_id, 0);
    assert!(!buf.no_accept_focus);
    assert_eq!(buf.background_alpha, 1.0);
}

// =======================================================================
// set_phys_cursor()
// =======================================================================

#[test]
fn set_phys_cursor_stores_info() {
    let mut buf = FrameGlyphBuffer::new();
    let cursor_fg = Color::rgb(0.0, 0.0, 0.0);
    let cursor = PhysCursor {
        window_id: 2,
        charpos: 99,
        row: 3,
        col: 4,
        slot_id: DisplaySlotId::from_pixels(2, 50.0, 100.0, buf.char_width, buf.char_height),
        x: 50.0,
        y: 100.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: Color::rgb(0.9, 0.9, 0.0),
        cursor_fg,
    };
    buf.set_phys_cursor(cursor.clone());

    let stored = buf.phys_cursor.as_ref().unwrap();
    assert_eq!(stored.window_id, cursor.window_id);
    assert_eq!(stored.charpos, cursor.charpos);
    assert_eq!(stored.row, cursor.row);
    assert_eq!(stored.col, cursor.col);
    assert_eq!(stored.slot_id, cursor.slot_id);
    assert_eq!(stored.x, cursor.x);
    assert_eq!(stored.y, cursor.y);
    assert_eq!(stored.width, cursor.width);
    assert_eq!(stored.height, cursor.height);
    assert_color_eq(&stored.color, &cursor.color);
    assert_color_eq(&stored.cursor_fg, &cursor.cursor_fg);
}

// =======================================================================
// font_size() / set_font_size()
// =======================================================================

#[test]
fn font_size_accessors() {
    let mut buf = FrameGlyphBuffer::new();
    assert_eq!(buf.font_size(), 14.0); // default

    buf.set_font_size(20.0);
    assert_eq!(buf.font_size(), 20.0);

    // Affects subsequently added chars
    buf.add_char('X', 0.0, 0.0, 10.0, 20.0, 15.0, false);
    match &buf.glyphs[0] {
        FrameGlyph::Char { font_size, .. } => assert_eq!(*font_size, 20.0),
        _ => panic!("Expected Char"),
    }
}

// =======================================================================
// add_background()
// =======================================================================

#[test]
fn add_background_adds_glyph() {
    let mut buf = FrameGlyphBuffer::new();
    let bg = Color::rgb(0.15, 0.15, 0.15);
    buf.add_background(10.0, 20.0, 780.0, 560.0, bg);

    assert_eq!(buf.len(), 1);

    match &buf.glyphs[0] {
        FrameGlyph::Background { bounds, color } => {
            assert_eq!(*bounds, Rect::new(10.0, 20.0, 780.0, 560.0));
            assert_color_eq(color, &bg);
        }
        other => panic!("Expected Background glyph, got {:?}", other),
    }
}

// =======================================================================
// add_border()
// =======================================================================

#[test]
fn add_border_appends_border_glyph() {
    let mut buf = FrameGlyphBuffer::new();
    let border_color = Color::rgb(0.3, 0.3, 0.3);
    buf.add_border(400.0, 0.0, 1.0, 600.0, border_color);

    assert_eq!(buf.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Border {
            x,
            y,
            width,
            height,
            color,
            ..
        } => {
            assert_eq!(*x, 400.0);
            assert_eq!(*y, 0.0);
            assert_eq!(*width, 1.0);
            assert_eq!(*height, 600.0);
            assert_color_eq(color, &border_color);
        }
        other => panic!("Expected Border glyph, got {:?}", other),
    }
}

#[test]
fn border_glyph_is_not_overlay() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_border(0.0, 0.0, 1.0, 100.0, Color::WHITE);
    assert!(!buf.glyphs[0].is_overlay());
}

// =======================================================================
// add_image() / add_video() / add_webkit()
// =======================================================================

#[test]
fn add_image_appends_image_glyph() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_image(42, 100.0, 200.0, 320.0, 240.0);

    assert_eq!(buf.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::Image {
            slot_id,
            image_id,
            x,
            y,
            width,
            height,
            ..
        } => {
            assert_eq!(
                *slot_id,
                Some(DisplaySlotId::from_pixels(
                    0,
                    100.0,
                    200.0,
                    buf.char_width,
                    buf.char_height
                ))
            );
            assert_eq!(*image_id, 42);
            assert_eq!(*x, 100.0);
            assert_eq!(*y, 200.0);
            assert_eq!(*width, 320.0);
            assert_eq!(*height, 240.0);
        }
        other => panic!("Expected Image glyph, got {:?}", other),
    }
}

#[test]
fn add_video_appends_video_glyph() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_video(7, 0.0, 0.0, 640.0, 480.0, 0, false);

    match &buf.glyphs[0] {
        FrameGlyph::Video { video_id, .. } => assert_eq!(*video_id, 7),
        other => panic!("Expected Video glyph, got {:?}", other),
    }
}

#[test]
fn add_webkit_appends_webkit_glyph() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_webkit(99, 0.0, 0.0, 800.0, 600.0);

    match &buf.glyphs[0] {
        FrameGlyph::WebKit { webkit_id, .. } => assert_eq!(*webkit_id, 99),
        other => panic!("Expected WebKit glyph, got {:?}", other),
    }
}

#[test]
fn slot_glyph_matches_media_slots() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_image(42, 16.0, 32.0, 320.0, 240.0);

    let slot_id = buf.glyphs[0].slot_id().expect("media slot id");
    let slot = buf.slot_glyph(slot_id).expect("slot glyph");
    assert!(matches!(slot, FrameGlyph::Image { image_id: 42, .. }));
}

#[test]
fn set_phys_cursor_normalizes_media_slots_to_hollow() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_image(9, 24.0, 48.0, 128.0, 96.0);
    let slot_id = buf.glyphs[0].slot_id().expect("image slot id");

    buf.set_phys_cursor(PhysCursor {
        window_id: 0,
        charpos: 0,
        row: slot_id.row as usize,
        col: slot_id.col,
        slot_id,
        x: 24.0,
        y: 48.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    });

    let stored = buf.phys_cursor.as_ref().expect("phys cursor");
    assert_eq!(stored.style, CursorStyle::Hollow);
    assert_eq!(stored.x, 24.0);
    assert_eq!(stored.y, 48.0);
    assert_eq!(stored.width, 128.0);
    assert_eq!(stored.height, 96.0);
}

// =======================================================================
// add_scroll_bar()
// =======================================================================

#[test]
fn add_scroll_bar_appends_scrollbar_glyph() {
    let mut buf = FrameGlyphBuffer::new();
    let track = Color::rgb(0.1, 0.1, 0.1);
    let thumb = Color::rgb(0.5, 0.5, 0.5);
    buf.add_scroll_bar(false, 790.0, 0.0, 10.0, 600.0, 50.0, 100.0, track, thumb);

    assert_eq!(buf.len(), 1);
    match &buf.glyphs[0] {
        FrameGlyph::ScrollBar {
            horizontal,
            x,
            y,
            width,
            height,
            thumb_start,
            thumb_size,
            track_color,
            thumb_color,
        } => {
            assert!(!*horizontal);
            assert_eq!(*x, 790.0);
            assert_eq!(*y, 0.0);
            assert_eq!(*width, 10.0);
            assert_eq!(*height, 600.0);
            assert_eq!(*thumb_start, 50.0);
            assert_eq!(*thumb_size, 100.0);
            assert_color_eq(track_color, &track);
            assert_color_eq(thumb_color, &thumb);
        }
        other => panic!("Expected ScrollBar glyph, got {:?}", other),
    }
}

// =======================================================================
// is_overlay() dispatch
// =======================================================================

#[test]
fn is_overlay_returns_false_for_non_char_stretch_types() {
    let mut buf = FrameGlyphBuffer::new();
    buf.add_border(0.0, 0.0, 1.0, 100.0, Color::WHITE);
    buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::FilledBox, Color::WHITE);
    buf.add_image(1, 0.0, 0.0, 100.0, 100.0);

    for glyph in &buf.glyphs {
        assert!(!glyph.is_overlay());
    }
}

// =======================================================================
// Full frame simulation: realistic multi-window frame
// =======================================================================

#[test]
fn full_frame_simulation() {
    let frame_bg = Color::rgb(0.12, 0.12, 0.12);
    let mut buf = FrameGlyphBuffer::with_size(1920.0, 1080.0);
    buf.background = frame_bg;
    buf.set_frame_identity(0x1, 0, 0.0, 0.0, 0, 0.0, Color::BLACK, false, 1.0);

    // Window 1: left pane background
    let win_bg = Color::rgb(0.13, 0.13, 0.13);
    buf.add_background(0.0, 0.0, 960.0, 1060.0, win_bg);

    // Window 1: some text
    let text_fg = Color::rgb(0.87, 0.87, 0.87);
    buf.set_face_with_font(
        0, text_fg, None, "Iosevka", 400, false, 14.0, 0, None, 0, None, 0, None, false,
    );
    for (i, ch) in "Hello, Neomacs!".chars().enumerate() {
        buf.add_char(ch, i as f32 * 8.0, 0.0, 8.0, 16.0, 12.0, false);
    }

    // Window 1: cursor
    buf.add_cursor(
        1,
        15.0 * 8.0,
        0.0,
        2.0,
        16.0,
        CursorStyle::Bar(2.0),
        Color::WHITE,
    );
    buf.set_phys_cursor(PhysCursor {
        window_id: 1,
        charpos: 15,
        row: 0,
        col: 15,
        slot_id: DisplaySlotId::from_pixels(
            1,
            15.0 * 8.0,
            0.0,
            buf.char_width,
            buf.char_height,
        ),
        x: 15.0 * 8.0,
        y: 0.0,
        width: 2.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::Bar(2.0),
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    });

    // Vertical border
    buf.add_border(960.0, 0.0, 1.0, 1060.0, Color::rgb(0.3, 0.3, 0.3));

    // Window 2: right pane background
    buf.add_background(961.0, 0.0, 959.0, 1060.0, win_bg);

    // Mode-line (overlay)
    let ml_bg = Color::rgb(0.2, 0.2, 0.3);
    buf.set_face(
        10,
        Color::WHITE,
        Some(ml_bg),
        700,
        false,
        0,
        None,
        0,
        None,
        0,
        None,
    );
    buf.set_draw_context(1, GlyphRowRole::ModeLine, None);
    buf.add_stretch(0.0, 1060.0, 1920.0, 20.0, ml_bg, 10, true);

    // Window infos
    buf.add_window_info(
        1,
        100,
        0,
        500,
        1000,
        0.0,
        0.0,
        960.0,
        1060.0,
        20.0,
        0.0,
        0.0,
        true,
        false,
        16.0,
        "left.rs".to_string(),
        false,
    );
    buf.add_window_info(
        2,
        200,
        0,
        300,
        800,
        961.0,
        0.0,
        959.0,
        1060.0,
        20.0,
        0.0,
        0.0,
        false,
        false,
        16.0,
        "right.rs".to_string(),
        true,
    );

    // Verify totals
    // 15 chars + 2 backgrounds + 1 border + 1 mode-line stretch = 19 glyphs
    assert_eq!(buf.len(), 19);
    assert_eq!(buf.window_cursors.len(), 1);
    assert_eq!(buf.window_infos.len(), 2);
    assert!(buf.phys_cursor.is_some());
    assert_eq!(buf.frame_id, 0x1);
    assert_eq!(buf.width, 1920.0);
    assert_eq!(buf.height, 1080.0);

    // Verify overlay count
    let overlay_count = buf.glyphs.iter().filter(|g| g.is_overlay()).count();
    assert_eq!(overlay_count, 1); // just the mode-line stretch
}
