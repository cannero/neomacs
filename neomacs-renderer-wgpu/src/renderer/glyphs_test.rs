use super::{cursor_render_rect, window_cursor_visual_matches_phys};
use neomacs_display_protocol::frame_glyphs::{
    CursorStyle, DisplaySlotId, FrameGlyph, FrameGlyphBuffer, GlyphRowRole, PhysCursor,
    WindowCursorVisual,
};
use neomacs_display_protocol::types::Color;

fn make_cursor(
    slot_id: DisplaySlotId,
    x: f32,
    y: f32,
    width: f32,
    style: CursorStyle,
) -> PhysCursor {
    PhysCursor {
        window_id: slot_id.window_id as i32,
        charpos: 0,
        row: slot_id.row as usize,
        col: slot_id.col,
        slot_id,
        x,
        y,
        width,
        height: 16.0,
        ascent: 12.0,
        style,
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    }
}

#[test]
fn rtl_bar_cursor_uses_right_edge_of_char_slot() {
    let mut frame = FrameGlyphBuffer::new();
    frame.set_draw_context(1, GlyphRowRole::Text, None);
    frame.add_char('א', 10.0, 20.0, 12.0, 16.0, 12.0, false);
    let slot_id = frame.glyphs[0].slot_id().expect("slot id");
    if let FrameGlyph::Char { bidi_level, .. } = &mut frame.glyphs[0] {
        *bidi_level = 1;
    }

    let cursor = make_cursor(slot_id, 10.0, 20.0, 2.0, CursorStyle::Bar(2.0));
    assert_eq!(cursor_render_rect(&frame, &cursor), (20.0, 20.0, 2.0, 16.0));
}

#[test]
fn rtl_hbar_cursor_uses_right_edge_of_stretch_slot() {
    let mut frame = FrameGlyphBuffer::new();
    frame.set_draw_context(2, GlyphRowRole::Text, None);
    frame.add_stretch(30.0, 40.0, 24.0, 16.0, Color::BLACK, 0, false);
    let slot_id = frame.glyphs[0].slot_id().expect("slot id");
    if let FrameGlyph::Stretch { bidi_level, .. } = &mut frame.glyphs[0] {
        *bidi_level = 1;
    }

    let cursor = make_cursor(slot_id, 30.0, 40.0, 8.0, CursorStyle::Hbar(2.0));
    assert_eq!(cursor_render_rect(&frame, &cursor), (46.0, 40.0, 8.0, 16.0));
}

#[test]
fn filled_box_cursor_keeps_slot_origin_in_rtl_runs() {
    let mut frame = FrameGlyphBuffer::new();
    frame.set_draw_context(3, GlyphRowRole::Text, None);
    frame.add_char('א', 50.0, 60.0, 12.0, 16.0, 12.0, false);
    let slot_id = frame.glyphs[0].slot_id().expect("slot id");
    if let FrameGlyph::Char { bidi_level, .. } = &mut frame.glyphs[0] {
        *bidi_level = 1;
    }

    let cursor = make_cursor(slot_id, 50.0, 60.0, 8.0, CursorStyle::FilledBox);
    assert_eq!(cursor_render_rect(&frame, &cursor), (50.0, 60.0, 8.0, 16.0));
}

#[test]
fn window_cursor_visual_match_uses_slot_identity() {
    let slot_id = DisplaySlotId::from_pixels(7, 32.0, 16.0, 8.0, 16.0);
    let phys = make_cursor(slot_id, 32.0, 16.0, 8.0, CursorStyle::FilledBox);
    let matching = WindowCursorVisual {
        window_id: 7,
        slot_id,
        x: 4.0,
        y: 0.0,
        width: 20.0,
        height: 30.0,
        style: CursorStyle::Hollow,
        color: Color::WHITE,
    };
    let mismatched = WindowCursorVisual {
        window_id: 7,
        slot_id: DisplaySlotId::from_pixels(7, 40.0, 16.0, 8.0, 16.0),
        x: 32.0,
        y: 16.0,
        width: 8.0,
        height: 16.0,
        style: CursorStyle::Hollow,
        color: Color::WHITE,
    };

    assert!(window_cursor_visual_matches_phys(&matching, &phys));
    assert!(!window_cursor_visual_matches_phys(&mismatched, &phys));
}
