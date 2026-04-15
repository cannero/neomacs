//! Terminal content extraction — snapshot of terminal state for rendering.
//!
//! Each frame, the render thread extracts a `TerminalContent` from the
//! `alacritty_terminal::Term` and converts cells to rendering primitives.

use super::colors::ansi_to_color;
use crate::core::types::Color;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags as CellFlags;

/// A single cell ready for GPU rendering.
#[derive(Debug, Clone)]
pub struct RenderCell {
    /// Grid column (0-based).
    pub col: usize,
    /// Grid row (0-based, 0 = top of visible area).
    pub row: usize,
    /// Character to display (space for empty cells).
    pub c: char,
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Cell flags (bold, italic, underline, etc.).
    pub flags: CellFlags,
}

/// Cursor state for rendering.
#[derive(Debug, Clone)]
pub struct RenderCursor {
    pub col: usize,
    pub row: usize,
    pub visible: bool,
}

/// Snapshot of terminal state for one frame.
#[derive(Debug, Clone)]
pub struct TerminalContent {
    /// All visible cells.
    pub cells: Vec<RenderCell>,
    /// Grid dimensions (columns x rows).
    pub cols: usize,
    pub rows: usize,
    /// Cursor info.
    pub cursor: RenderCursor,
    /// Default background color.
    pub default_bg: Color,
    /// Default foreground color.
    pub default_fg: Color,
}

impl TerminalContent {
    /// Extract renderable content from an alacritty Term.
    pub fn from_term<T: alacritty_terminal::event::EventListener>(term: &Term<T>) -> Self {
        let grid = term.grid();
        let num_cols = grid.columns();
        let num_lines = grid.screen_lines();

        let default_fg = Color::WHITE;
        let default_bg = Color::BLACK;

        let mut cells = Vec::with_capacity(num_cols * num_lines);

        for row_idx in 0..num_lines {
            let line = Line(row_idx as i32);
            for col_idx in 0..num_cols {
                let point = Point::new(line, Column(col_idx));
                let cell = &grid[point];

                let c = cell.c;
                // Skip wide char spacers (second cell of double-width character)
                if cell.flags.contains(CellFlags::WIDE_CHAR_SPACER) {
                    continue;
                }

                let fg = ansi_to_color(&cell.fg, &default_fg, &default_bg);
                let bg = ansi_to_color(&cell.bg, &default_fg, &default_bg);

                cells.push(RenderCell {
                    col: col_idx,
                    row: row_idx,
                    c,
                    fg,
                    bg,
                    flags: cell.flags,
                });
            }
        }

        let cursor_point = term.grid().cursor.point;
        let cursor = RenderCursor {
            col: cursor_point.column.0,
            row: cursor_point.line.0 as usize,
            visible: term
                .mode()
                .contains(alacritty_terminal::term::TermMode::SHOW_CURSOR),
        };

        TerminalContent {
            cells,
            cols: num_cols,
            rows: num_lines,
            cursor,
            default_bg,
            default_fg,
        }
    }
}

/// Extract text from a terminal grid region as a String.
pub fn extract_text<T: alacritty_terminal::event::EventListener>(
    term: &Term<T>,
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
) -> String {
    let grid = term.grid();
    let num_cols = grid.columns();
    let mut text = String::new();

    for row in start_row..=end_row {
        let line = Line(row as i32);
        let col_start = if row == start_row { start_col } else { 0 };
        let col_end = if row == end_row {
            end_col
        } else {
            num_cols.saturating_sub(1)
        };

        for col in col_start..=col_end {
            let point = Point::new(line, Column(col));
            if line.0 < grid.screen_lines() as i32 && col < num_cols {
                let cell = &grid[point];
                if !cell.flags.contains(CellFlags::WIDE_CHAR_SPACER) {
                    text.push(cell.c);
                }
            }
        }
        if row < end_row {
            text.push('\n');
        }
    }

    // Trim trailing whitespace per line
    text.lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "content_test.rs"]
mod tests;
