use super::*;
use crate::face::{Face, FaceAttributes, UnderlineStyle};
use crate::frame_glyphs::{CursorStyle, GlyphRowRole, PhysCursor};
use crate::glyph_matrix::{
    FrameDisplayState, Glyph, GlyphArea, GlyphMatrix, GlyphRow, WindowMatrixEntry,
};
use crate::types::{Color, Rect};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// TtyRif::new
// ---------------------------------------------------------------------------

#[test]
fn new_creates_correct_grid_dimensions() {
    let rif = TtyRif::new(80, 24);
    assert_eq!(rif.width(), 80);
    assert_eq!(rif.height(), 24);
    assert_eq!(rif.current.cells.len(), 80 * 24);
    assert_eq!(rif.desired.cells.len(), 80 * 24);
}

#[test]
fn new_grids_are_blank_spaces() {
    let rif = TtyRif::new(10, 5);
    for cell in &rif.current.cells {
        assert_eq!(cell.ch, ' ');
        assert!(!cell.padding);
    }
}

// ---------------------------------------------------------------------------
// resize
// ---------------------------------------------------------------------------

#[test]
fn resize_updates_dimensions() {
    let mut rif = TtyRif::new(80, 24);
    rif.resize(120, 40);
    assert_eq!(rif.width(), 120);
    assert_eq!(rif.height(), 40);
    assert_eq!(rif.current.cells.len(), 120 * 40);
    assert_eq!(rif.desired.cells.len(), 120 * 40);
}

#[test]
fn resize_clears_grids() {
    let mut rif = TtyRif::new(10, 5);
    // Dirty a cell in current.
    rif.current.set(0, 0, 'X', CellAttrs::default(), false);
    rif.resize(10, 5);
    // After resize, the cell should be blank again.
    assert_eq!(rif.current.cells[0].ch, ' ');
}

// ---------------------------------------------------------------------------
// Face resolution
// ---------------------------------------------------------------------------

#[test]
fn resolve_attrs_uses_face_table() {
    let mut rif = TtyRif::new(80, 24);
    let mut face = Face::new(1);
    face.foreground = Color::rgb(1.0, 0.0, 0.0);
    face.background = Color::rgb(0.0, 1.0, 0.0);
    face.font_weight = 700;
    face.attributes |= FaceAttributes::ITALIC;
    face.underline_style = UnderlineStyle::Wave;
    face.attributes |= FaceAttributes::STRIKE_THROUGH;

    let mut faces = HashMap::new();
    faces.insert(1, face);
    rif.set_faces(faces);

    let attrs = rif.resolve_attrs(1);
    assert_eq!(attrs.fg, (255, 0, 0));
    assert_eq!(attrs.bg, (0, 255, 0));
    assert!(attrs.bold);
    assert!(attrs.italic);
    assert_eq!(attrs.underline, 2); // Wave
    assert!(attrs.strikethrough);
}

#[test]
fn resolve_attrs_falls_back_to_defaults_for_unknown_face() {
    let rif = TtyRif::new(80, 24);
    let attrs = rif.resolve_attrs(999);
    // Should get default fg/bg.
    assert_eq!(attrs.fg, (255, 255, 255));
    assert_eq!(attrs.bg, (0, 0, 0));
    assert!(!attrs.bold);
    assert!(!attrs.italic);
}

// ---------------------------------------------------------------------------
// glyph_to_char
// ---------------------------------------------------------------------------

#[test]
fn glyph_to_char_returns_char_for_char_glyph() {
    let g = Glyph::char('Z', 0, 0);
    assert_eq!(glyph_to_char(&g), 'Z');
}

#[test]
fn glyph_to_char_returns_first_char_for_composite() {
    let g = Glyph {
        glyph_type: GlyphType::Composite { text: "ab".into() },
        face_id: 0,
        charpos: 0,
        bidi_level: 0,
        wide: false,
        padding: false,
    };
    assert_eq!(glyph_to_char(&g), 'a');
}

#[test]
fn glyph_to_char_returns_space_for_stretch() {
    let g = Glyph::stretch(4, 0);
    assert_eq!(glyph_to_char(&g), ' ');
}

// ---------------------------------------------------------------------------
// color_to_rgb8
// ---------------------------------------------------------------------------

/// `color_to_rgb8` applies `linear_to_srgb` before quantizing, so
/// a linear input of 0.5 becomes sRGB ~0.735 → 188 (not 127).
#[test]
fn color_to_rgb8_converts_correctly() {
    let c = Color::rgb(1.0, 0.5, 0.0);
    let (r, g, b) = color_to_rgb8(&c);
    assert_eq!(r, 255);
    // linear 0.5 → sRGB: 1.055 * 0.5^(1/2.4) - 0.055 ≈ 0.735 → 188
    assert_eq!(g, 188);
    assert_eq!(b, 0);
}

#[test]
fn color_to_rgb8_clamps_out_of_range() {
    let c = Color::rgb(2.0, -1.0, 0.5);
    let (r, g, b) = color_to_rgb8(&c);
    assert_eq!(r, 255);
    assert_eq!(g, 0);
    // linear 0.5 → sRGB ≈ 188
    assert_eq!(b, 188);
}

/// Round-trip: an sRGB pixel value → Color::from_pixel (srgb→linear)
/// → color_to_rgb8 (linear→srgb) should recover the original byte
/// values. This is the contract that makes TTY face colors match
/// GNU Emacs exactly.
#[test]
fn color_to_rgb8_round_trips_srgb_pixel() {
    // grey75 = sRGB 191 = 0xbfbfbf (GNU mode-line bg)
    let pixel = 0x00bfbfbf_u32;
    let linear = Color::from_pixel(pixel);
    let (r, g, b) = color_to_rgb8(&linear);
    assert_eq!(r, 191, "grey75 round-trip red channel");
    assert_eq!(g, 191, "grey75 round-trip green channel");
    assert_eq!(b, 191, "grey75 round-trip blue channel");

    // grey30 = sRGB 77 = 0x4d4d4d (GNU mode-line-inactive bg, dark)
    let pixel2 = 0x004d4d4d_u32;
    let linear2 = Color::from_pixel(pixel2);
    let (r2, g2, b2) = color_to_rgb8(&linear2);
    assert_eq!(r2, 77, "grey30 round-trip red channel");
    assert_eq!(g2, 77, "grey30 round-trip green channel");
    assert_eq!(b2, 77, "grey30 round-trip blue channel");
}

// ---------------------------------------------------------------------------
// rasterize
// ---------------------------------------------------------------------------

/// Helper: build a simple FrameDisplayState with one window containing
/// the given text on a single row.
fn make_simple_state(text: &str) -> FrameDisplayState {
    let cols = text.len().max(10);
    let mut state = FrameDisplayState::new(cols, 5, 8.0, 16.0);
    state.background = Color::rgb(0.0, 0.0, 0.0);

    let mut matrix = GlyphMatrix::new(5, cols);
    let mut row = GlyphRow::new(GlyphRowRole::Text);
    for (i, ch) in text.chars().enumerate() {
        row.glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 0, i));
    }
    matrix.rows[0] = row;

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, cols as f32 * 8.0, 5.0 * 16.0),
        selected: true,
    });
    state
}

#[test]
fn rasterize_simple_text() {
    let mut rif = TtyRif::new(10, 5);
    let state = make_simple_state("Hello");
    rif.rasterize(&state);

    // First row should have "Hello" followed by spaces.
    assert_eq!(rif.desired.cells[0].ch, 'H');
    assert_eq!(rif.desired.cells[1].ch, 'e');
    assert_eq!(rif.desired.cells[2].ch, 'l');
    assert_eq!(rif.desired.cells[3].ch, 'l');
    assert_eq!(rif.desired.cells[4].ch, 'o');
    assert_eq!(rif.desired.cells[5].ch, ' '); // cleared to space
}

#[test]
fn rasterize_respects_matrix_position() {
    let mut state = FrameDisplayState::new(20, 10, 8.0, 16.0);
    state.background = Color::rgb(0.0, 0.0, 0.0);

    let mut matrix = GlyphMatrix::new(3, 10);
    matrix.matrix_x = 5;
    matrix.matrix_y = 2;
    let mut row = GlyphRow::new(GlyphRowRole::Text);
    row.glyphs[GlyphArea::Text as usize].push(Glyph::char('A', 0, 0));
    matrix.rows[0] = row;

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(40.0, 32.0, 80.0, 48.0),
        selected: true,
    });

    let mut rif = TtyRif::new(20, 10);
    rif.rasterize(&state);

    // 'A' should be at row=2, col=5.
    let idx = 2 * 20 + 5;
    assert_eq!(rif.desired.cells[idx].ch, 'A');
    // row=0 col=0 should still be blank.
    assert_eq!(rif.desired.cells[0].ch, ' ');
}

#[test]
fn rasterize_disabled_rows_are_skipped() {
    let mut state = FrameDisplayState::new(10, 5, 8.0, 16.0);
    state.background = Color::rgb(0.0, 0.0, 0.0);

    let mut matrix = GlyphMatrix::new(5, 10);
    let mut row = GlyphRow::new(GlyphRowRole::Text);
    row.glyphs[GlyphArea::Text as usize].push(Glyph::char('X', 0, 0));
    row.enabled = false;
    matrix.rows[0] = row;

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, 80.0, 80.0),
        selected: true,
    });

    let mut rif = TtyRif::new(10, 5);
    rif.rasterize(&state);

    // Row 0 should be blank because the glyph row is disabled.
    assert_eq!(rif.desired.cells[0].ch, ' ');
}

// ---------------------------------------------------------------------------
// Wide character handling
// ---------------------------------------------------------------------------

#[test]
fn rasterize_wide_char_creates_padding() {
    let mut state = FrameDisplayState::new(10, 5, 8.0, 16.0);
    state.background = Color::BLACK;

    let mut matrix = GlyphMatrix::new(5, 10);
    let mut row = GlyphRow::new(GlyphRowRole::Text);
    // CJK character, wide=true.
    let mut g = Glyph::char('\u{4e16}', 0, 0); // Unicode: "world" in Chinese
    g.wide = true;
    row.glyphs[GlyphArea::Text as usize].push(g);
    // Followed by a normal char.
    row.glyphs[GlyphArea::Text as usize].push(Glyph::char('!', 0, 1));
    matrix.rows[0] = row;

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, 80.0, 80.0),
        selected: true,
    });

    let mut rif = TtyRif::new(10, 5);
    rif.rasterize(&state);

    // Col 0: the wide char.
    assert_eq!(rif.desired.cells[0].ch, '\u{4e16}');
    assert!(!rif.desired.cells[0].padding);
    // Col 1: padding cell.
    assert!(rif.desired.cells[1].padding);
    // Col 2: '!'
    assert_eq!(rif.desired.cells[2].ch, '!');
    assert!(!rif.desired.cells[2].padding);
}

// ---------------------------------------------------------------------------
// Cursor tracking
// ---------------------------------------------------------------------------

#[test]
fn rasterize_tracks_cursor_position() {
    let mut state = FrameDisplayState::new(10, 5, 8.0, 16.0);
    state.background = Color::BLACK;

    let mut matrix = GlyphMatrix::new(5, 10);
    matrix.matrix_x = 0;
    matrix.matrix_y = 0;
    let mut row = GlyphRow::new(GlyphRowRole::Text);
    row.glyphs[GlyphArea::Text as usize].push(Glyph::char('a', 0, 0));
    row.glyphs[GlyphArea::Text as usize].push(Glyph::char('b', 0, 1));
    row.cursor_col = Some(1);
    row.cursor_type = Some(CursorStyle::FilledBox);
    matrix.rows[0] = row;

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, 80.0, 80.0),
        selected: true,
    });

    let mut rif = TtyRif::new(10, 5);
    rif.rasterize(&state);

    assert!(rif.cursor_visible);
    assert_eq!(rif.cursor_row, 0);
    assert_eq!(rif.cursor_col, 1);
}

#[test]
fn rasterize_prefers_phys_cursor_over_matrix_cursor_columns() {
    let mut state = FrameDisplayState::new(10, 5, 8.0, 16.0);
    state.background = Color::BLACK;

    let mut matrix = GlyphMatrix::new(5, 10);
    let mut row0 = GlyphRow::new(GlyphRowRole::Text);
    row0.glyphs[GlyphArea::Text as usize].push(Glyph::char('a', 0, 0));
    row0.cursor_col = Some(1);
    row0.cursor_type = Some(CursorStyle::FilledBox);
    matrix.rows[0] = row0;

    let mut row1 = GlyphRow::new(GlyphRowRole::Text);
    row1.glyphs[GlyphArea::Text as usize].push(Glyph::char('b', 0, 1));
    matrix.rows[1] = row1;

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, 80.0, 80.0),
        selected: true,
    });
    state.phys_cursor = Some(PhysCursor {
        window_id: 1,
        charpos: 1,
        row: 1,
        col: 4,
        x: 32.0,
        y: 16.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: Color::WHITE,
    });

    let mut rif = TtyRif::new(10, 5);
    rif.rasterize(&state);

    assert!(rif.cursor_visible);
    assert_eq!(rif.cursor_row, 1);
    assert_eq!(rif.cursor_col, 4);
}

/// Regression test for a bug observed after `C-x 2` in an
/// interactive `neomacs -nw -Q` session: the physical terminal
/// cursor ended up inside the newly-created (non-selected)
/// bottom window because the TTY RIF iterated both windows'
/// glyph matrices and let the LAST `cursor_col` it saw win,
/// clobbering the selected window's cursor with the hollow
/// cursor hint drawn for the non-selected window.
///
/// GNU Emacs has a dedicated `tty_set_cursor` in
/// `src/dispnew.c:5670-5751` that explicitly uses
/// `FRAME_SELECTED_WINDOW (f)` and only calls `cursor_to` once,
/// with this comment:
///
///   /* We have only one cursor on terminal frames. Use it to
///      display the cursor of the selected window of the
///      frame.  */
///   struct window *w = XWINDOW (FRAME_SELECTED_WINDOW (f));
///   ...
///   cursor_to (f, y, x);
///
/// The `selected: bool` field on `WindowMatrixEntry` is the
/// per-frame-state equivalent of GNU's `FRAME_SELECTED_WINDOW`
/// check: only the selected window contributes `cursor_col` to
/// the terminal cursor position. Non-selected windows may still
/// mark `cursor_col` to draw a hollow cursor glyph (via
/// `cursor-in-non-selected-windows`), but that stays a visual
/// cue in the cell, not a terminal cursor move.
#[test]
fn rasterize_terminal_cursor_comes_from_selected_window_only() {
    // Two vertically stacked 2-row windows at screen cols 0..10.
    // Top window (w1) is selected; its cursor is in row 0, col 3.
    // Bottom window (w2) is NOT selected but still draws a
    // hollow cursor in its row 0, col 7 (the non-selected
    // hint). The terminal cursor MUST come from w1.
    let mut state = FrameDisplayState::new(10, 5, 8.0, 16.0);
    state.background = Color::BLACK;

    let mut top_matrix = GlyphMatrix::new(2, 10);
    let mut top_row = GlyphRow::new(GlyphRowRole::Text);
    for (i, ch) in "TOP-BUFFER".chars().enumerate() {
        top_row.glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 0, i));
    }
    top_row.cursor_col = Some(3);
    top_row.cursor_type = Some(CursorStyle::FilledBox);
    top_matrix.rows[0] = top_row;

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: top_matrix,
        pixel_bounds: Rect::new(0.0, 0.0, 80.0, 32.0),
        selected: true,
    });

    let mut bot_matrix = GlyphMatrix::new(2, 10);
    let mut bot_row = GlyphRow::new(GlyphRowRole::Text);
    for (i, ch) in "BOT-BUFFER".chars().enumerate() {
        bot_row.glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 0, i));
    }
    // Non-selected window still marks a hollow cursor column via
    // the same `cursor_col` slot, reflecting the `Hollow` style
    // chosen by `cursor_style_for_window` for windows where
    // `cursor-in-non-selected-windows` is non-nil.
    bot_row.cursor_col = Some(7);
    bot_row.cursor_type = Some(CursorStyle::Hollow);
    bot_matrix.rows[0] = bot_row;

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: bot_matrix,
        // Bottom half of the screen.
        pixel_bounds: Rect::new(0.0, 32.0, 80.0, 32.0),
        selected: false,
    });

    let mut rif = TtyRif::new(10, 5);
    rif.rasterize(&state);

    assert!(rif.cursor_visible, "terminal cursor should be visible");
    assert_eq!(
        rif.cursor_row, 0,
        "cursor row must come from selected (top) window"
    );
    assert_eq!(
        rif.cursor_col, 3,
        "cursor column must come from selected (top) window — \
         the non-selected bottom window's hollow cursor at col 7 \
         must NOT move the physical terminal cursor"
    );
}

/// Complementary test: when the frame layout lists the selected
/// window AFTER a non-selected window, the terminal cursor must
/// still come from the selected window. Without the
/// `entry.selected` guard this case happens to succeed by
/// accident (last-writer-wins lands on the selected window), so
/// we verify it explicitly to pin the intent rather than the
/// iteration order.
#[test]
fn rasterize_terminal_cursor_comes_from_selected_window_regardless_of_order() {
    let mut state = FrameDisplayState::new(10, 5, 8.0, 16.0);
    state.background = Color::BLACK;

    // First entry: non-selected window with a hollow cursor.
    let mut w1_matrix = GlyphMatrix::new(2, 10);
    let mut w1_row = GlyphRow::new(GlyphRowRole::Text);
    for (i, ch) in "FIRST-WIN".chars().enumerate() {
        w1_row.glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 0, i));
    }
    w1_row.cursor_col = Some(9);
    w1_row.cursor_type = Some(CursorStyle::Hollow);
    w1_matrix.rows[0] = w1_row;

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: w1_matrix,
        pixel_bounds: Rect::new(0.0, 0.0, 80.0, 32.0),
        selected: false,
    });

    // Second entry: the selected window with its real cursor.
    let mut w2_matrix = GlyphMatrix::new(2, 10);
    let mut w2_row = GlyphRow::new(GlyphRowRole::Text);
    for (i, ch) in "SECND-WIN".chars().enumerate() {
        w2_row.glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 0, i));
    }
    w2_row.cursor_col = Some(2);
    w2_row.cursor_type = Some(CursorStyle::FilledBox);
    w2_matrix.rows[0] = w2_row;

    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: w2_matrix,
        pixel_bounds: Rect::new(0.0, 32.0, 80.0, 32.0),
        selected: true,
    });

    let mut rif = TtyRif::new(10, 5);
    rif.rasterize(&state);

    assert!(rif.cursor_visible);
    assert_eq!(rif.cursor_row, 2, "selected window starts at screen row 2");
    assert_eq!(rif.cursor_col, 2, "cursor col from selected window only");
}

// ---------------------------------------------------------------------------
// diff_and_render
// ---------------------------------------------------------------------------

#[test]
fn diff_no_changes_produces_minimal_output() {
    let mut rif = TtyRif::new(10, 5);
    // Both grids are identical (blank). Diff should produce only:
    // hide cursor + reset attrs (+ maybe show cursor).
    rif.diff_and_render();
    let output = rif.take_output();

    let s = String::from_utf8_lossy(&output);
    // Should contain hide cursor and reset, but no CUP positioning for cells.
    assert!(s.contains("\x1b[?25l")); // hide cursor
    assert!(s.contains("\x1b[0m")); // reset
    // No cell was changed, so no ";H" cursor moves for cells.
    // The only H would be in the hide-cursor prefix. Count occurrences of "H".
    let cup_count = s.matches("H").count();
    // At most 0 CUP sequences if cursor is not visible.
    assert!(
        cup_count == 0,
        "Expected 0 CUP moves for no-change diff, got {}",
        cup_count
    );
}

#[test]
fn diff_with_changes_produces_ansi_sequences() {
    let mut rif = TtyRif::new(10, 5);
    // Write something into the desired grid.
    rif.desired.set(
        0,
        0,
        'A',
        CellAttrs {
            fg: (255, 0, 0),
            ..CellAttrs::default()
        },
        false,
    );
    rif.diff_and_render();
    let output = rif.take_output();
    let s = String::from_utf8_lossy(&output);

    // Should contain CUP to row 1, col 1 (1-based).
    assert!(s.contains("\x1b[1;1H"), "Missing CUP: {}", s);
    // Should contain the character 'A'.
    assert!(s.contains('A'), "Missing character A: {}", s);
    // Should contain true-color foreground sequence for red.
    assert!(s.contains("\x1b[38;2;255;0;0m"), "Missing fg color: {}", s);
}

#[test]
fn diff_swaps_current_and_desired() {
    let mut rif = TtyRif::new(10, 5);
    rif.desired.set(0, 0, 'X', CellAttrs::default(), false);
    rif.diff_and_render();

    // After diff, current should have 'X' at (0,0).
    assert_eq!(rif.current.cells[0].ch, 'X');
}

#[test]
fn second_diff_with_same_content_is_minimal() {
    let mut rif = TtyRif::new(10, 5);
    rif.desired.set(0, 0, 'Q', CellAttrs::default(), false);
    rif.diff_and_render();

    // Set the desired to the same content again.
    rif.desired.set(0, 0, 'Q', CellAttrs::default(), false);
    rif.diff_and_render();
    let output = rif.take_output();
    let s = String::from_utf8_lossy(&output);

    // Since desired == current, no cell CUP moves.
    // Only hide cursor + reset + possibly show cursor.
    let cup_count = s.matches("H").count();
    assert!(
        cup_count == 0,
        "Expected 0 CUP for identical frames, got {}",
        cup_count
    );
}

// ---------------------------------------------------------------------------
// Cursor visibility in output
// ---------------------------------------------------------------------------

#[test]
fn cursor_visible_emits_show_cursor_sequence() {
    let mut rif = TtyRif::new(10, 5);
    rif.cursor_visible = true;
    rif.cursor_row = 3;
    rif.cursor_col = 7;
    rif.diff_and_render();
    let output = rif.take_output();
    let s = String::from_utf8_lossy(&output);

    // Should show cursor.
    assert!(s.contains("\x1b[?25h"), "Missing show cursor: {}", s);
    // Should position cursor at (4, 8) (1-based).
    assert!(s.contains("\x1b[4;8H"), "Missing cursor position: {}", s);
}

#[test]
fn cursor_not_visible_omits_show_cursor_sequence() {
    let mut rif = TtyRif::new(10, 5);
    rif.cursor_visible = false;
    rif.diff_and_render();
    let output = rif.take_output();
    let s = String::from_utf8_lossy(&output);

    assert!(
        !s.contains("\x1b[?25h"),
        "Show cursor should not appear: {}",
        s
    );
}

// ---------------------------------------------------------------------------
// SGR sequences
// ---------------------------------------------------------------------------

#[test]
fn write_sgr_bold_italic_underline() {
    let attrs = CellAttrs {
        fg: (0, 0, 0),
        bg: (255, 255, 255),
        bold: true,
        italic: true,
        underline: 1,
        strikethrough: false,
        inverse: false,
    };
    let mut buf = Vec::new();
    write_sgr(&mut buf, &attrs);
    let s = String::from_utf8_lossy(&buf);

    assert!(s.contains("\x1b[0m"), "Missing reset");
    assert!(s.contains("\x1b[1m"), "Missing bold");
    assert!(s.contains("\x1b[3m"), "Missing italic");
    assert!(s.contains("\x1b[4m"), "Missing underline");
}

#[test]
fn write_sgr_strikethrough_inverse() {
    let attrs = CellAttrs {
        fg: (0, 0, 0),
        bg: (0, 0, 0),
        bold: false,
        italic: false,
        underline: 0,
        strikethrough: true,
        inverse: true,
    };
    let mut buf = Vec::new();
    write_sgr(&mut buf, &attrs);
    let s = String::from_utf8_lossy(&buf);

    assert!(s.contains("\x1b[9m"), "Missing strikethrough");
    assert!(s.contains("\x1b[7m"), "Missing inverse");
}

// ---------------------------------------------------------------------------
// TtyGrid
// ---------------------------------------------------------------------------

#[test]
fn grid_clear_sets_background() {
    let mut grid = TtyGrid::new(5, 3);
    grid.clear((100, 50, 25));
    for cell in &grid.cells {
        assert_eq!(cell.ch, ' ');
        assert_eq!(cell.attrs.bg, (100, 50, 25));
    }
}

#[test]
fn grid_set_out_of_bounds_is_noop() {
    let mut grid = TtyGrid::new(5, 3);
    // Should not panic.
    grid.set(100, 100, 'X', CellAttrs::default(), false);
    // All cells still blank.
    for cell in &grid.cells {
        assert_eq!(cell.ch, ' ');
    }
}

// ---------------------------------------------------------------------------
// take_output
// ---------------------------------------------------------------------------

#[test]
fn take_output_clears_buffer() {
    let mut rif = TtyRif::new(10, 5);
    rif.desired.set(0, 0, 'A', CellAttrs::default(), false);
    rif.diff_and_render();

    let first = rif.take_output();
    assert!(!first.is_empty());

    let second = rif.take_output();
    assert!(second.is_empty());
}

// ---------------------------------------------------------------------------
// Full round-trip: rasterize + diff_and_render
// ---------------------------------------------------------------------------

#[test]
fn full_round_trip_simple_text() {
    let mut rif = TtyRif::new(10, 5);
    let state = make_simple_state("Hi");
    rif.rasterize(&state);
    rif.diff_and_render();
    let output = rif.take_output();
    let s = String::from_utf8_lossy(&output);

    // Should contain 'H' and 'i' somewhere in the output.
    assert!(s.contains('H'), "Missing H in output");
    assert!(s.contains('i'), "Missing i in output");
}
