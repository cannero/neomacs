use super::*;
use crate::core::types::Rect;

// -------------------------------------------------------------------
// ANSI escape sequence generation
// -------------------------------------------------------------------

#[test]
fn test_cursor_goto_1_1() {
    let mut buf = Vec::new();
    ansi::cursor_goto(&mut buf, 1, 1);
    assert_eq!(String::from_utf8(buf).unwrap(), "\x1b[1;1H");
}

#[test]
fn test_cursor_goto_various() {
    let mut buf = Vec::new();
    ansi::cursor_goto(&mut buf, 10, 20);
    assert_eq!(String::from_utf8(buf).unwrap(), "\x1b[10;20H");
}

#[test]
fn test_fg_truecolor() {
    let mut buf = Vec::new();
    ansi::fg_truecolor(&mut buf, 255, 128, 0);
    assert_eq!(String::from_utf8(buf).unwrap(), "\x1b[38;2;255;128;0m");
}

#[test]
fn test_bg_truecolor() {
    let mut buf = Vec::new();
    ansi::bg_truecolor(&mut buf, 0, 64, 128);
    assert_eq!(String::from_utf8(buf).unwrap(), "\x1b[48;2;0;64;128m");
}

#[test]
fn test_fg_256() {
    let mut buf = Vec::new();
    ansi::fg_256(&mut buf, 196);
    assert_eq!(String::from_utf8(buf).unwrap(), "\x1b[38;5;196m");
}

#[test]
fn test_bg_256() {
    let mut buf = Vec::new();
    ansi::bg_256(&mut buf, 27);
    assert_eq!(String::from_utf8(buf).unwrap(), "\x1b[48;5;27m");
}

#[test]
fn test_write_sgr_default_attrs() {
    let mut buf = Vec::new();
    let attrs = ansi::CellAttrs::default();
    ansi::write_sgr(&mut buf, &attrs);
    let s = String::from_utf8(buf).unwrap();
    // Should contain reset
    assert!(s.starts_with("\x1b[0m"));
    // Should contain fg and bg color sequences
    assert!(s.contains("\x1b[38;2;255;255;255m")); // white fg
    assert!(s.contains("\x1b[48;2;0;0;0m")); // black bg
    // Should NOT contain bold/italic/underline
    assert!(!s.contains("\x1b[1m"));
    assert!(!s.contains("\x1b[3m"));
    assert!(!s.contains("\x1b[4m"));
}

#[test]
fn test_write_sgr_bold_italic() {
    let mut buf = Vec::new();
    let attrs = ansi::CellAttrs {
        bold: true,
        italic: true,
        ..Default::default()
    };
    ansi::write_sgr(&mut buf, &attrs);
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains("\x1b[1m")); // bold
    assert!(s.contains("\x1b[3m")); // italic
}

#[test]
fn test_write_sgr_underline_styles() {
    // Single underline
    {
        let mut buf = Vec::new();
        let attrs = ansi::CellAttrs {
            underline: 1,
            ..Default::default()
        };
        ansi::write_sgr(&mut buf, &attrs);
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\x1b[4m"));
    }

    // Wave underline
    {
        let mut buf = Vec::new();
        let attrs = ansi::CellAttrs {
            underline: 2,
            ..Default::default()
        };
        ansi::write_sgr(&mut buf, &attrs);
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\x1b[4:3m"));
    }

    // Double underline
    {
        let mut buf = Vec::new();
        let attrs = ansi::CellAttrs {
            underline: 3,
            ..Default::default()
        };
        ansi::write_sgr(&mut buf, &attrs);
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\x1b[21m"));
    }
}

#[test]
fn test_write_sgr_strikethrough() {
    let mut buf = Vec::new();
    let attrs = ansi::CellAttrs {
        strikethrough: true,
        ..Default::default()
    };
    ansi::write_sgr(&mut buf, &attrs);
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains("\x1b[9m"));
}

#[test]
fn test_write_sgr_inverse() {
    let mut buf = Vec::new();
    let attrs = ansi::CellAttrs {
        inverse: true,
        ..Default::default()
    };
    ansi::write_sgr(&mut buf, &attrs);
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains("\x1b[7m"));
}

#[test]
fn test_write_sgr_custom_colors() {
    let mut buf = Vec::new();
    let attrs = ansi::CellAttrs {
        fg: (128, 64, 32),
        bg: (10, 20, 30),
        ..Default::default()
    };
    ansi::write_sgr(&mut buf, &attrs);
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains("\x1b[38;2;128;64;32m"));
    assert!(s.contains("\x1b[48;2;10;20;30m"));
}

#[test]
fn test_write_sgr_underline_color() {
    let mut buf = Vec::new();
    let attrs = ansi::CellAttrs {
        underline: 1,
        underline_color: Some((255, 0, 0)),
        ..Default::default()
    };
    ansi::write_sgr(&mut buf, &attrs);
    let s = String::from_utf8(buf).unwrap();
    assert!(s.contains("\x1b[58;2;255;0;0m"));
}

#[test]
fn test_write_sgr_all_attributes() {
    let mut buf = Vec::new();
    let attrs = ansi::CellAttrs {
        fg: (200, 100, 50),
        bg: (10, 20, 30),
        bold: true,
        italic: true,
        underline: 1,
        underline_color: Some((0, 255, 0)),
        strikethrough: true,
        inverse: true,
    };
    ansi::write_sgr(&mut buf, &attrs);
    let s = String::from_utf8(buf).unwrap();
    assert!(s.starts_with("\x1b[0m")); // reset first
    assert!(s.contains("\x1b[1m")); // bold
    assert!(s.contains("\x1b[3m")); // italic
    assert!(s.contains("\x1b[4m")); // underline
    assert!(s.contains("\x1b[9m")); // strikethrough
    assert!(s.contains("\x1b[7m")); // inverse
    assert!(s.contains("\x1b[38;2;200;100;50m")); // fg
    assert!(s.contains("\x1b[48;2;10;20;30m")); // bg
    assert!(s.contains("\x1b[58;2;0;255;0m")); // underline color
}

// -------------------------------------------------------------------
// Color conversion
// -------------------------------------------------------------------

#[test]
fn test_color_to_rgb8_black() {
    assert_eq!(color_to_rgb8(&Color::BLACK), (0, 0, 0));
}

#[test]
fn test_color_to_rgb8_white() {
    assert_eq!(color_to_rgb8(&Color::WHITE), (255, 255, 255));
}

#[test]
fn test_color_to_rgb8_red() {
    assert_eq!(color_to_rgb8(&Color::RED), (255, 0, 0));
}

#[test]
fn test_color_to_rgb8_mid_gray() {
    let c = Color::rgb(0.5, 0.5, 0.5);
    let (r, g, b) = color_to_rgb8(&c);
    assert_eq!(r, 128);
    assert_eq!(g, 128);
    assert_eq!(b, 128);
}

#[test]
fn test_color_to_rgb8_clamping() {
    let c = Color::new(1.5, -0.5, 2.0, 1.0);
    assert_eq!(color_to_rgb8(&c), (255, 0, 255));
}

// -------------------------------------------------------------------
// TtyGrid basic operations
// -------------------------------------------------------------------

#[test]
fn test_grid_new() {
    let grid = TtyGrid::new(10, 5);
    assert_eq!(grid.width, 10);
    assert_eq!(grid.height, 5);
    assert_eq!(grid.cells.len(), 50);
}

#[test]
fn test_grid_get_set() {
    let mut grid = TtyGrid::new(10, 5);
    let cell = TtyCell {
        text: "A".to_string(),
        width: 1,
        attrs: ansi::CellAttrs {
            fg: (255, 0, 0),
            ..Default::default()
        },
    };
    grid.set(3, 2, cell.clone());
    assert_eq!(grid.get(3, 2).unwrap(), &cell);
}

#[test]
fn test_grid_get_out_of_bounds() {
    let grid = TtyGrid::new(10, 5);
    assert!(grid.get(10, 0).is_none());
    assert!(grid.get(0, 5).is_none());
    assert!(grid.get(100, 100).is_none());
}

#[test]
fn test_grid_clear() {
    let mut grid = TtyGrid::new(5, 3);
    grid.set(
        2,
        1,
        TtyCell {
            text: "X".to_string(),
            width: 1,
            attrs: ansi::CellAttrs {
                fg: (255, 0, 0),
                ..Default::default()
            },
        },
    );
    assert_eq!(grid.get(2, 1).unwrap().text, "X");

    grid.clear();
    assert_eq!(grid.get(2, 1).unwrap().text, " ");
    assert_eq!(grid.get(2, 1).unwrap().attrs, ansi::CellAttrs::default());
}

#[test]
fn test_grid_resize() {
    let mut grid = TtyGrid::new(5, 3);
    grid.set(
        2,
        1,
        TtyCell {
            text: "A".to_string(),
            ..Default::default()
        },
    );
    grid.resize(10, 8);
    assert_eq!(grid.width, 10);
    assert_eq!(grid.height, 8);
    assert_eq!(grid.cells.len(), 80);
}

// -------------------------------------------------------------------
// Frame diffing
// -------------------------------------------------------------------

#[test]
fn test_diff_identical_grids_produces_no_output() {
    let grid = TtyGrid::new(10, 5);
    let output = diff_grids(&grid, &grid);
    // No cells changed, so no output (no SGR reset needed either)
    assert!(output.is_empty());
}

#[test]
fn test_diff_single_cell_change() {
    let prev = TtyGrid::new(10, 5);
    let mut next = TtyGrid::new(10, 5);
    next.set(
        3,
        2,
        TtyCell {
            text: "X".to_string(),
            width: 1,
            attrs: ansi::CellAttrs {
                fg: (255, 0, 0),
                bg: (0, 0, 0),
                ..Default::default()
            },
        },
    );

    let output = diff_grids(&prev, &next);
    let s = String::from_utf8(output).unwrap();

    // Should contain cursor goto for row 3 (1-based), col 4 (1-based)
    assert!(s.contains("\x1b[3;4H"));
    // Should contain the character
    assert!(s.contains("X"));
    // Should contain fg color
    assert!(s.contains("\x1b[38;2;255;0;0m"));
    // Should end with SGR reset
    assert!(s.ends_with("\x1b[0m"));
}

#[test]
fn test_diff_consecutive_changes_no_redundant_goto() {
    let prev = TtyGrid::new(10, 5);
    let mut next = TtyGrid::new(10, 5);

    let attrs = ansi::CellAttrs {
        fg: (0, 255, 0),
        bg: (0, 0, 0),
        ..Default::default()
    };

    // Set three consecutive cells on row 0
    for col in 0..3 {
        next.set(
            col,
            0,
            TtyCell {
                text: ((b'A' + col as u8) as char).to_string(),
                width: 1,
                attrs,
            },
        );
    }

    let output = diff_grids(&prev, &next);
    let s = String::from_utf8(output).unwrap();

    // Should contain only ONE cursor_goto (for the first cell)
    let goto_count = s.matches("\x1b[1;").count();
    assert_eq!(goto_count, 1, "Expected 1 goto, got {}: {}", goto_count, s);

    // Should contain all three characters
    assert!(s.contains('A'));
    assert!(s.contains('B'));
    assert!(s.contains('C'));
}

#[test]
fn test_diff_non_consecutive_changes_emit_goto() {
    let prev = TtyGrid::new(20, 5);
    let mut next = TtyGrid::new(20, 5);

    let attrs = ansi::CellAttrs::default();

    // Set cell at col 0, row 0
    next.set(
        0,
        0,
        TtyCell {
            text: "A".to_string(),
            width: 1,
            attrs,
        },
    );
    // Set cell at col 10, row 0 (gap)
    next.set(
        10,
        0,
        TtyCell {
            text: "B".to_string(),
            width: 1,
            attrs,
        },
    );

    let output = diff_grids(&prev, &next);
    let s = String::from_utf8(output).unwrap();

    // Should contain two cursor_goto sequences (one for each non-consecutive cell)
    assert!(s.contains("\x1b[1;1H"));
    assert!(s.contains("\x1b[1;11H"));
}

#[test]
fn test_diff_attrs_change_emits_new_sgr() {
    let prev = TtyGrid::new(10, 5);
    let mut next = TtyGrid::new(10, 5);

    // Two cells with different attributes
    next.set(
        0,
        0,
        TtyCell {
            text: "A".to_string(),
            width: 1,
            attrs: ansi::CellAttrs {
                fg: (255, 0, 0),
                bg: (0, 0, 0),
                bold: true,
                ..Default::default()
            },
        },
    );
    next.set(
        1,
        0,
        TtyCell {
            text: "B".to_string(),
            width: 1,
            attrs: ansi::CellAttrs {
                fg: (0, 255, 0),
                bg: (0, 0, 0),
                italic: true,
                ..Default::default()
            },
        },
    );

    let output = diff_grids(&prev, &next);
    let s = String::from_utf8(output).unwrap();

    // Should contain both SGR sequences
    assert!(s.contains("\x1b[1m")); // bold
    assert!(s.contains("\x1b[3m")); // italic
    assert!(s.contains("\x1b[38;2;255;0;0m")); // red fg
    assert!(s.contains("\x1b[38;2;0;255;0m")); // green fg
}

#[test]
fn test_diff_same_attrs_no_redundant_sgr() {
    let prev = TtyGrid::new(10, 5);
    let mut next = TtyGrid::new(10, 5);

    let attrs = ansi::CellAttrs {
        fg: (255, 255, 0),
        bg: (0, 0, 0),
        bold: true,
        ..Default::default()
    };

    // Two consecutive cells with SAME attributes
    next.set(
        0,
        0,
        TtyCell {
            text: "A".to_string(),
            width: 1,
            attrs,
        },
    );
    next.set(
        1,
        0,
        TtyCell {
            text: "B".to_string(),
            width: 1,
            attrs,
        },
    );

    let output = diff_grids(&prev, &next);
    let s = String::from_utf8(output).unwrap();

    // Should contain exactly ONE SGR reset (at start of attribute set) + ONE at end
    // The two cells share attributes so only one write_sgr call should happen
    // Count occurrences of the bold sequence
    let bold_count = s.matches("\x1b[1m").count();
    assert_eq!(bold_count, 1, "Expected 1 bold SGR, got {}", bold_count);
}

// -------------------------------------------------------------------
// Full render
// -------------------------------------------------------------------

#[test]
fn test_render_full_starts_with_home() {
    let grid = TtyGrid::new(3, 2);
    let output = render_full(&grid);
    let s = String::from_utf8(output).unwrap();
    assert!(s.starts_with(ansi::CURSOR_HOME));
}

#[test]
fn test_render_full_contains_all_cells() {
    let mut grid = TtyGrid::new(3, 2);
    // Set specific cells
    grid.set(
        0,
        0,
        TtyCell {
            text: "A".to_string(),
            ..Default::default()
        },
    );
    grid.set(
        1,
        0,
        TtyCell {
            text: "B".to_string(),
            ..Default::default()
        },
    );
    grid.set(
        2,
        0,
        TtyCell {
            text: "C".to_string(),
            ..Default::default()
        },
    );
    grid.set(
        0,
        1,
        TtyCell {
            text: "D".to_string(),
            ..Default::default()
        },
    );

    let output = render_full(&grid);
    let s = String::from_utf8(output).unwrap();
    assert!(s.contains("A"));
    assert!(s.contains("B"));
    assert!(s.contains("C"));
    assert!(s.contains("D"));
}

#[test]
fn test_render_full_ends_with_sgr_reset() {
    let grid = TtyGrid::new(3, 2);
    let output = render_full(&grid);
    let s = String::from_utf8(output).unwrap();
    assert!(s.ends_with(ansi::SGR_RESET));
}

// -------------------------------------------------------------------
// Rasterizer: FrameGlyphBuffer -> TtyGrid
// -------------------------------------------------------------------

#[test]
fn test_rasterize_empty_frame() {
    let frame = FrameGlyphBuffer::new();
    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    // All cells should be spaces with black bg
    for cell in &grid.cells {
        assert_eq!(cell.text, " ");
        assert_eq!(cell.attrs.bg, (0, 0, 0));
    }
}

#[test]
fn test_rasterize_char_glyph() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let fg = Color::rgb(1.0, 0.0, 0.0);
    frame.set_face(0, fg, None, 700, false, 0, None, 0, None, 0, None);
    // Place 'H' at pixel (0, 0) -> col 0, row 0
    frame.add_char('H', 0.0, 0.0, 8.0, 16.0, 12.0, false);
    // Place 'i' at pixel (8, 0) -> col 1, row 0
    frame.add_char('i', 8.0, 0.0, 8.0, 16.0, 12.0, false);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    assert_eq!(grid.get(0, 0).unwrap().text, "H");
    assert_eq!(grid.get(0, 0).unwrap().attrs.fg, (255, 0, 0));
    assert!(grid.get(0, 0).unwrap().attrs.bold);

    assert_eq!(grid.get(1, 0).unwrap().text, "i");
}

#[test]
fn test_rasterize_stretch_glyph() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let bg = Color::rgb(0.0, 0.0, 1.0);
    // Stretch from pixel (0, 16) to (80, 32) -> row 1, cols 0-9
    frame.add_stretch(0.0, 16.0, 80.0, 16.0, bg, 0, false);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    // Row 1 should have blue background
    for col in 0..10 {
        assert_eq!(grid.get(col, 1).unwrap().attrs.bg, (0, 0, 255));
    }
    // Row 0 should still be black
    assert_eq!(grid.get(0, 0).unwrap().attrs.bg, (0, 0, 0));
}

#[test]
fn test_rasterize_border_vertical() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let border_color = Color::rgb(0.5, 0.5, 0.5);
    // Vertical border at pixel x=40 (col 5), spanning full height
    frame.add_border(40.0, 0.0, 1.0, 80.0, border_color);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    // Column 5 should have the vertical border char
    for row in 0..5 {
        let cell = grid.get(5, row).unwrap();
        assert_eq!(cell.text, "\u{2502}"); // │
    }
}

#[test]
fn test_rasterize_cursor_box() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    // First put a character
    let fg = Color::rgb(1.0, 1.0, 1.0);
    frame.set_face(
        0,
        fg,
        Some(Color::BLACK),
        400,
        false,
        0,
        None,
        0,
        None,
        0,
        None,
    );
    frame.add_char('A', 16.0, 0.0, 8.0, 16.0, 12.0, false);

    // Then add a box cursor at same position
    frame.add_cursor(
        1,
        16.0,
        0.0,
        8.0,
        16.0,
        CursorStyle::FilledBox,
        Color::rgb(1.0, 1.0, 1.0),
    );

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    // Cell at col 2, row 0 should have inverse bg (cursor color)
    let cell = grid.get(2, 0).unwrap();
    assert_eq!(cell.attrs.bg, (255, 255, 255)); // cursor color as bg
}

#[test]
fn test_rasterize_cursor_underline() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    frame.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::Hbar(2.0), Color::WHITE);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    let cell = grid.get(0, 0).unwrap();
    assert_eq!(cell.attrs.underline, 1);
}

#[test]
fn test_rasterize_background_glyph() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let bg = Color::rgb(0.2, 0.2, 0.2);
    frame.add_background(0.0, 0.0, 80.0, 48.0, bg);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    let expected_bg = color_to_rgb8(&bg);
    // First 3 rows (48/16=3), all 10 cols should have bg
    for row in 0..3 {
        for col in 0..10 {
            assert_eq!(
                grid.get(col, row).unwrap().attrs.bg,
                expected_bg,
                "Wrong bg at col={}, row={}",
                col,
                row
            );
        }
    }
}

#[test]
fn test_rasterize_glyph_out_of_bounds() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let fg = Color::WHITE;
    frame.set_face(0, fg, None, 400, false, 0, None, 0, None, 0, None);
    // Place char way outside the grid
    frame.add_char('Z', 1000.0, 1000.0, 8.0, 16.0, 12.0, false);

    let mut grid = TtyGrid::new(10, 5);
    // Should not panic
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));
}

#[test]
fn test_rasterize_composed_char() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let fg = Color::WHITE;
    frame.set_face(0, fg, None, 400, false, 0, None, 0, None, 0, None);
    frame.add_composed_char("e\u{0301}", 'e', 0.0, 0.0, 8.0, 16.0, 12.0, false);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    assert_eq!(grid.get(0, 0).unwrap().text, "e\u{0301}");
}

#[test]
fn test_rasterize_bold_face() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let fg = Color::WHITE;
    frame.set_face(0, fg, None, 700, false, 0, None, 0, None, 0, None);
    frame.add_char('B', 0.0, 0.0, 8.0, 16.0, 12.0, false);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    assert!(grid.get(0, 0).unwrap().attrs.bold);
}

#[test]
fn test_rasterize_italic_face() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let fg = Color::WHITE;
    frame.set_face(0, fg, None, 400, true, 0, None, 0, None, 0, None);
    frame.add_char('I', 0.0, 0.0, 8.0, 16.0, 12.0, false);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    assert!(grid.get(0, 0).unwrap().attrs.italic);
}

#[test]
fn test_rasterize_underline_with_color() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let fg = Color::WHITE;
    let ul_color = Color::rgb(1.0, 0.0, 0.0);
    frame.set_face(0, fg, None, 400, false, 1, Some(ul_color), 0, None, 0, None);
    frame.add_char('U', 0.0, 0.0, 8.0, 16.0, 12.0, false);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    let cell = grid.get(0, 0).unwrap();
    assert_eq!(cell.attrs.underline, 1);
    assert_eq!(cell.attrs.underline_color, Some((255, 0, 0)));
}

#[test]
fn test_rasterize_strikethrough() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let fg = Color::WHITE;
    frame.set_face(0, fg, None, 400, false, 0, None, 1, None, 0, None);
    frame.add_char('S', 0.0, 0.0, 8.0, 16.0, 12.0, false);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    assert!(grid.get(0, 0).unwrap().attrs.strikethrough);
}

// -------------------------------------------------------------------
// TtyBackend methods
// -------------------------------------------------------------------

#[test]
fn test_backend_name() {
    let backend = TtyBackend::new();
    assert_eq!(backend.name(), "tty");
}

#[test]
fn test_backend_not_initialized_by_default() {
    let backend = TtyBackend::new();
    assert!(!backend.is_initialized());
}

#[test]
fn test_backend_default_size() {
    let backend = TtyBackend::new();
    assert_eq!(backend.grid_size(), (80, 24));
}

#[test]
fn test_backend_resize() {
    let mut backend = TtyBackend::new();
    backend.resize(120, 40);
    assert_eq!(backend.grid_size(), (120, 40));
    assert!(backend.force_full_render);
}

#[test]
fn test_backend_render_not_initialized_returns_error() {
    let mut backend = TtyBackend::new();
    let scene = Scene::new(800.0, 600.0);
    let result = backend.render(&scene);
    assert!(result.is_err());
}

#[test]
fn test_backend_present_not_initialized_returns_error() {
    let mut backend = TtyBackend::new();
    let result = backend.present();
    assert!(result.is_err());
}

#[test]
fn test_backend_force_redraw() {
    let mut backend = TtyBackend::new();
    backend.force_full_render = false;
    backend.force_redraw();
    assert!(backend.force_full_render);
}

#[test]
fn test_backend_set_vsync_is_noop() {
    let mut backend = TtyBackend::new();
    // Should not panic
    backend.set_vsync(true);
    backend.set_vsync(false);
}

// -------------------------------------------------------------------
// ANSI constant checks
// -------------------------------------------------------------------

#[test]
fn test_ansi_constants() {
    assert_eq!(ansi::ENTER_ALT_SCREEN, "\x1b[?1049h");
    assert_eq!(ansi::LEAVE_ALT_SCREEN, "\x1b[?1049l");
    assert_eq!(ansi::HIDE_CURSOR, "\x1b[?25l");
    assert_eq!(ansi::SHOW_CURSOR, "\x1b[?25h");
    assert_eq!(ansi::SGR_RESET, "\x1b[0m");
    assert_eq!(ansi::CLEAR_SCREEN, "\x1b[2J");
    assert_eq!(ansi::CURSOR_HOME, "\x1b[H");
}

#[test]
fn test_ansi_sgr_attribute_constants() {
    assert_eq!(ansi::SGR_BOLD, "\x1b[1m");
    assert_eq!(ansi::SGR_ITALIC, "\x1b[3m");
    assert_eq!(ansi::SGR_UNDERLINE, "\x1b[4m");
    assert_eq!(ansi::SGR_DOUBLE_UNDERLINE, "\x1b[21m");
    assert_eq!(ansi::SGR_CURLY_UNDERLINE, "\x1b[4:3m");
    assert_eq!(ansi::SGR_DOTTED_UNDERLINE, "\x1b[4:4m");
    assert_eq!(ansi::SGR_DASHED_UNDERLINE, "\x1b[4:5m");
    assert_eq!(ansi::SGR_STRIKETHROUGH, "\x1b[9m");
    assert_eq!(ansi::SGR_INVERSE, "\x1b[7m");
}

// -------------------------------------------------------------------
// CellAttrs equality
// -------------------------------------------------------------------

#[test]
fn test_cell_attrs_equality() {
    let a = ansi::CellAttrs::default();
    let b = ansi::CellAttrs::default();
    assert_eq!(a, b);

    let c = ansi::CellAttrs {
        bold: true,
        ..Default::default()
    };
    assert_ne!(a, c);
}

#[test]
fn test_cell_attrs_default_values() {
    let attrs = ansi::CellAttrs::default();
    assert_eq!(attrs.fg, (255, 255, 255));
    assert_eq!(attrs.bg, (0, 0, 0));
    assert!(!attrs.bold);
    assert!(!attrs.italic);
    assert_eq!(attrs.underline, 0);
    assert!(attrs.underline_color.is_none());
    assert!(!attrs.strikethrough);
    assert!(!attrs.inverse);
}

// -------------------------------------------------------------------
// TtyCell equality
// -------------------------------------------------------------------

#[test]
fn test_tty_cell_equality() {
    let a = TtyCell::default();
    let b = TtyCell::default();
    assert_eq!(a, b);

    let c = TtyCell {
        text: "X".to_string(),
        ..Default::default()
    };
    assert_ne!(a, c);
}

#[test]
fn test_tty_cell_default() {
    let cell = TtyCell::default();
    assert_eq!(cell.text, " ");
    assert_eq!(cell.width, 1);
    assert_eq!(cell.attrs, ansi::CellAttrs::default());
}

// -------------------------------------------------------------------
// Integration: build_output with frame diff
// -------------------------------------------------------------------

#[test]
fn test_build_output_force_full() {
    let mut backend = TtyBackend::new();
    backend.initialized = true;
    backend.width = 5;
    backend.height = 3;
    backend.current = TtyGrid::new(5, 3);
    backend.previous = TtyGrid::new(5, 3);
    backend.force_full_render = true;

    backend.build_output();

    let s = String::from_utf8(backend.output_buf.clone()).unwrap();
    // Full render starts with cursor home
    assert!(s.contains(ansi::CURSOR_HOME));
    assert!(!backend.force_full_render); // should be cleared
}

#[test]
fn test_build_output_diff_mode() {
    let mut backend = TtyBackend::new();
    backend.initialized = true;
    backend.width = 5;
    backend.height = 3;
    backend.current = TtyGrid::new(5, 3);
    backend.previous = TtyGrid::new(5, 3);
    backend.force_full_render = false;

    // Change one cell
    backend.current.set(
        2,
        1,
        TtyCell {
            text: "Q".to_string(),
            width: 1,
            attrs: ansi::CellAttrs {
                fg: (0, 128, 255),
                ..Default::default()
            },
        },
    );

    backend.build_output();

    let s = String::from_utf8(backend.output_buf.clone()).unwrap();
    assert!(s.contains("Q"));
    assert!(s.contains("\x1b[2;3H")); // row 2, col 3 (1-based)
}

#[test]
fn test_build_output_cursor_position() {
    let mut backend = TtyBackend::new();
    backend.initialized = true;
    backend.width = 10;
    backend.height = 5;
    backend.current = TtyGrid::new(10, 5);
    backend.previous = TtyGrid::new(10, 5);
    backend.force_full_render = false;
    backend.cursor_position = Some((5, 3));
    backend.cursor_visible = true;
    backend.cursor_shape = ansi::TerminalCursorShape::Bar;

    // Make a change so build_output produces something
    backend.current.set(
        0,
        0,
        TtyCell {
            text: "X".to_string(),
            ..Default::default()
        },
    );

    backend.build_output();

    let s = String::from_utf8(backend.output_buf.clone()).unwrap();
    // Cursor should be positioned at row 4, col 6 (1-based)
    assert!(s.contains("\x1b[4;6H"));
    assert!(s.contains("\x1b[6 q"));
    assert!(s.contains(ansi::SHOW_CURSOR));
}

#[test]
fn test_build_output_cursor_hidden() {
    let mut backend = TtyBackend::new();
    backend.initialized = true;
    backend.width = 10;
    backend.height = 5;
    backend.current = TtyGrid::new(10, 5);
    backend.previous = TtyGrid::new(10, 5);
    backend.force_full_render = false;
    backend.cursor_position = None;

    // Force at least a diff computation
    backend.current.set(
        0,
        0,
        TtyCell {
            text: "X".to_string(),
            ..Default::default()
        },
    );

    backend.build_output();

    let s = String::from_utf8(backend.output_buf.clone()).unwrap();
    assert!(s.contains(ansi::HIDE_CURSOR));
}

#[test]
fn test_rasterize_frame_glyphs_prefers_phys_cursor_visual() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    frame.set_face(
        0,
        Color::WHITE,
        Some(Color::BLACK),
        400,
        false,
        0,
        None,
        0,
        None,
        0,
        None,
    );
    frame.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);
    frame.add_char('B', 8.0, 0.0, 8.0, 16.0, 12.0, false);
    frame.add_cursor(0, 0.0, 0.0, 8.0, 16.0, CursorStyle::Hollow, Color::GREEN);
    frame.set_phys_cursor(crate::core::frame_glyphs::PhysCursor {
        window_id: 0,
        charpos: 1,
        row: 0,
        col: 1,
        slot_id: DisplaySlotId {
            window_id: 0,
            row: 0,
            col: 1,
        },
        x: 8.0,
        y: 0.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: Color::RED,
        cursor_fg: Color::BLACK,
    });

    let mut grid = TtyGrid::new(4, 2);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    assert_eq!(grid.get(0, 0).unwrap().attrs.bg, (0, 0, 0));
    assert!(grid.get(0, 0).unwrap().attrs.inverse);
    assert_eq!(grid.get(1, 0).unwrap().attrs.bg, (255, 0, 0));
    assert_eq!(grid.get(1, 0).unwrap().attrs.fg, (0, 0, 0));
}

#[test]
fn test_rasterize_preserves_nonselected_hollow_cursor_visual() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    frame.set_face(
        0,
        Color::WHITE,
        Some(Color::BLACK),
        400,
        false,
        0,
        None,
        0,
        None,
        0,
        None,
    );
    frame.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);
    frame.add_char('B', 8.0, 0.0, 8.0, 16.0, 12.0, false);
    frame.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::Hollow, Color::GREEN);
    frame.set_phys_cursor(crate::core::frame_glyphs::PhysCursor {
        window_id: 2,
        charpos: 1,
        row: 0,
        col: 1,
        slot_id: DisplaySlotId {
            window_id: 2,
            row: 0,
            col: 1,
        },
        x: 8.0,
        y: 0.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: Color::RED,
        cursor_fg: Color::BLACK,
    });

    let mut grid = TtyGrid::new(4, 2);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    assert!(grid.get(0, 0).unwrap().attrs.inverse);
    assert_eq!(grid.get(1, 0).unwrap().attrs.bg, (255, 0, 0));
    assert_eq!(grid.get(1, 0).unwrap().attrs.fg, (0, 0, 0));
}

#[test]
fn test_terminal_cursor_state_uses_hardware_bar_shape() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;
    frame.set_phys_cursor(crate::core::frame_glyphs::PhysCursor {
        window_id: 0,
        charpos: 0,
        row: 1,
        col: 2,
        slot_id: DisplaySlotId {
            window_id: 0,
            row: 1,
            col: 2,
        },
        x: 16.0,
        y: 16.0,
        width: 2.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::Bar(2.0),
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    });

    assert_eq!(
        terminal_cursor_state(&frame),
        Some(((2, 1), true, Some(ansi::TerminalCursorShape::Bar),))
    );
}

#[test]
fn test_terminal_cursor_state_uses_hardware_underline_shape() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;
    frame.set_phys_cursor(crate::core::frame_glyphs::PhysCursor {
        window_id: 0,
        charpos: 0,
        row: 0,
        col: 3,
        slot_id: DisplaySlotId {
            window_id: 0,
            row: 0,
            col: 3,
        },
        x: 24.0,
        y: 0.0,
        width: 8.0,
        height: 2.0,
        ascent: 12.0,
        style: CursorStyle::Hbar(2.0),
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    });

    assert_eq!(
        terminal_cursor_state(&frame),
        Some(((3, 0), true, Some(ansi::TerminalCursorShape::Underline),))
    );
}

#[test]
fn test_terminal_cursor_state_keeps_filled_box_software_only() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;
    frame.set_phys_cursor(crate::core::frame_glyphs::PhysCursor {
        window_id: 0,
        charpos: 0,
        row: 0,
        col: 0,
        slot_id: DisplaySlotId {
            window_id: 0,
            row: 0,
            col: 0,
        },
        x: 0.0,
        y: 0.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    });

    assert_eq!(terminal_cursor_state(&frame), Some(((0, 0), false, None)));
}

#[test]
fn test_render_prefers_phys_cursor_position() {
    let mut backend = TtyBackend::new();
    backend.initialized = true;
    backend.width = 10;
    backend.height = 5;
    backend.current = TtyGrid::new(10, 5);
    backend.previous = TtyGrid::new(10, 5);
    backend.force_full_render = false;

    let mut frame = FrameGlyphBuffer::with_size(80.0, 80.0);
    frame.char_width = 8.0;
    frame.char_height = 16.0;
    frame.add_cursor(0, 0.0, 0.0, 8.0, 16.0, CursorStyle::FilledBox, Color::GREEN);
    frame.set_phys_cursor(crate::core::frame_glyphs::PhysCursor {
        window_id: 0,
        charpos: 0,
        row: 2,
        col: 3,
        slot_id: DisplaySlotId {
            window_id: 0,
            row: 2,
            col: 3,
        },
        x: 24.0,
        y: 32.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::Hbar(2.0),
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    });
    backend.set_frame_glyphs(frame);

    let scene = Scene::new(80.0, 80.0);
    backend.render(&scene).unwrap();

    assert_eq!(backend.cursor_position, Some((3, 2)));
    assert!(backend.cursor_visible);
}

#[test]
fn test_render_does_not_derive_terminal_cursor_from_cursor_glyphs() {
    let mut backend = TtyBackend::new();
    backend.initialized = true;
    backend.width = 10;
    backend.height = 5;
    backend.current = TtyGrid::new(10, 5);
    backend.previous = TtyGrid::new(10, 5);
    backend.force_full_render = false;

    let mut frame = FrameGlyphBuffer::with_size(80.0, 80.0);
    frame.char_width = 8.0;
    frame.char_height = 16.0;
    frame.add_cursor(
        0,
        24.0,
        32.0,
        8.0,
        16.0,
        CursorStyle::FilledBox,
        Color::GREEN,
    );
    backend.set_frame_glyphs(frame);

    let scene = Scene::new(80.0, 80.0);
    backend.render(&scene).unwrap();

    assert_eq!(backend.cursor_position, None);
    assert!(!backend.cursor_visible);
}

#[test]
fn test_window_cursor_visual_match_uses_slot_identity() {
    let slot_id = DisplaySlotId {
        window_id: 0,
        row: 1,
        col: 2,
    };
    let phys = crate::core::frame_glyphs::PhysCursor {
        window_id: 0,
        charpos: 0,
        row: 1,
        col: 2,
        slot_id,
        x: 16.0,
        y: 16.0,
        width: 8.0,
        height: 16.0,
        ascent: 12.0,
        style: CursorStyle::FilledBox,
        color: Color::WHITE,
        cursor_fg: Color::BLACK,
    };
    let matching = WindowCursorVisual {
        window_id: 0,
        slot_id,
        x: 0.0,
        y: 0.0,
        width: 40.0,
        height: 24.0,
        style: CursorStyle::Hollow,
        color: Color::GREEN,
    };
    let mismatched = WindowCursorVisual {
        window_id: 0,
        slot_id: DisplaySlotId {
            window_id: 0,
            row: 1,
            col: 3,
        },
        x: 16.0,
        y: 16.0,
        width: 8.0,
        height: 16.0,
        style: CursorStyle::Hollow,
        color: Color::GREEN,
    };

    assert!(window_cursor_visual_matches_phys(&matching, &phys));
    assert!(!window_cursor_visual_matches_phys(&mismatched, &phys));
}

// -------------------------------------------------------------------
// Multi-row rasterization
// -------------------------------------------------------------------

#[test]
fn test_rasterize_multiple_rows() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 8.0;
    frame.char_height = 16.0;

    let fg = Color::WHITE;
    frame.set_face(0, fg, None, 400, false, 0, None, 0, None, 0, None);

    // Row 0: "ABC"
    frame.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);
    frame.add_char('B', 8.0, 0.0, 8.0, 16.0, 12.0, false);
    frame.add_char('C', 16.0, 0.0, 8.0, 16.0, 12.0, false);

    // Row 1: "XY"
    frame.add_char('X', 0.0, 16.0, 8.0, 16.0, 12.0, false);
    frame.add_char('Y', 8.0, 16.0, 8.0, 16.0, 12.0, false);

    let mut grid = TtyGrid::new(10, 5);
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));

    assert_eq!(grid.get(0, 0).unwrap().text, "A");
    assert_eq!(grid.get(1, 0).unwrap().text, "B");
    assert_eq!(grid.get(2, 0).unwrap().text, "C");
    assert_eq!(grid.get(0, 1).unwrap().text, "X");
    assert_eq!(grid.get(1, 1).unwrap().text, "Y");
    // Rest should be spaces
    assert_eq!(grid.get(3, 0).unwrap().text, " ");
    assert_eq!(grid.get(2, 1).unwrap().text, " ");
}

// -------------------------------------------------------------------
// Edge cases
// -------------------------------------------------------------------

#[test]
fn test_diff_empty_grids() {
    let a = TtyGrid::new(0, 0);
    let b = TtyGrid::new(0, 0);
    let output = diff_grids(&a, &b);
    assert!(output.is_empty());
}

#[test]
fn test_render_full_empty_grid() {
    let grid = TtyGrid::new(0, 0);
    let output = render_full(&grid);
    let s = String::from_utf8(output).unwrap();
    // Just cursor home, no content
    assert!(s.starts_with(ansi::CURSOR_HOME));
}

#[test]
fn test_rasterize_zero_char_dimensions() {
    let mut frame = FrameGlyphBuffer::new();
    frame.char_width = 0.0; // Would cause division by zero without max(1.0)
    frame.char_height = 0.0;

    let fg = Color::WHITE;
    frame.set_face(0, fg, None, 400, false, 0, None, 0, None, 0, None);
    frame.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);

    let mut grid = TtyGrid::new(10, 5);
    // Should not panic (char_width/height clamped to 1.0)
    rasterize_frame_glyphs(&frame, &mut grid, (0, 0, 0));
}

#[test]
fn test_diff_wide_char_skips_continuation() {
    let prev = TtyGrid::new(10, 3);
    let mut next = TtyGrid::new(10, 3);

    // Place a wide character (width=2) at col 0
    next.set(
        0,
        0,
        TtyCell {
            text: "\u{4E2D}".to_string(), // 中
            width: 2,
            attrs: ansi::CellAttrs::default(),
        },
    );
    // Continuation cell at col 1
    next.set(
        1,
        0,
        TtyCell {
            text: String::new(),
            width: 0,
            attrs: ansi::CellAttrs::default(),
        },
    );
    // Normal char at col 2
    next.set(
        2,
        0,
        TtyCell {
            text: "A".to_string(),
            width: 1,
            attrs: ansi::CellAttrs::default(),
        },
    );

    let output = diff_grids(&prev, &next);
    let s = String::from_utf8(output).unwrap();

    // Should contain the wide char and 'A'
    assert!(s.contains("\u{4E2D}"));
    assert!(s.contains("A"));
}
