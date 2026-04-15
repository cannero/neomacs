use super::window_cursor_visual_matches_phys;
use neomacs_display_protocol::frame_glyphs::{
    CursorStyle, DisplaySlotId, PhysCursor, WindowCursorVisual,
};
use neomacs_display_protocol::types::Color;

#[test]
fn window_cursor_visual_match_uses_slot_identity() {
    let slot_id = DisplaySlotId::from_pixels(7, 32.0, 16.0, 8.0, 16.0);
    let phys = PhysCursor {
        window_id: 7,
        charpos: 0,
        row: slot_id.row as usize,
        col: slot_id.col,
        slot_id,
        x: 32.0,
        y: 16.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    };
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
