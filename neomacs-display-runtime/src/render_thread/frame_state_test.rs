use super::*;
use crate::core::frame_glyphs::{CursorStyle, FrameGlyphBuffer, GlyphRowRole};
use neomacs_display_protocol::types::Color;

#[test]
fn apply_extra_spacing_remaps_cursor_by_slot_id() {
    let mut frame = FrameGlyphBuffer::with_size(80.0, 32.0);
    frame.set_draw_context(1, GlyphRowRole::Text, None);
    frame.add_char('a', 0.0, 0.0, 8.0, 16.0, 12.0, false);
    frame.add_char('b', 8.0, 0.0, 8.0, 16.0, 12.0, false);
    let target_slot = frame.glyphs[1].slot_id().expect("slot id");

    frame.add_cursor(1, 2.0, 0.0, 2.0, 16.0, CursorStyle::Bar(2.0), Color::WHITE);
    frame.window_cursors[0].slot_id = target_slot;

    frame.set_phys_cursor(PhysCursor {
        window_id: 1,
        charpos: 1,
        row: 0,
        col: 1,
        slot_id: target_slot,
        x: 2.0,
        y: 0.0,
        width: 2.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::Bar(2.0),
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    });

    RenderApp::apply_extra_spacing(
        &mut frame.glyphs,
        &mut frame.window_cursors,
        &mut frame.phys_cursor,
        0.0,
        1.0,
    );

    match &frame.glyphs[1] {
        FrameGlyph::Char { x, .. } => assert_eq!(*x, 9.0),
        other => panic!("expected char glyph, got {:?}", other),
    }
    assert_eq!(frame.window_cursors[0].x, 9.0);
    assert_eq!(frame.window_cursors[0].y, 0.0);
    let cursor = frame.phys_cursor.as_ref().expect("phys cursor");
    assert_eq!(cursor.x, 9.0);
    assert_eq!(cursor.y, 0.0);
}
