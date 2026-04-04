//! Mock display test — renders fake Emacs frames via TtyRif.
//!
//! Usage:
//!   cargo run -p neomacs-bin --bin mock-display            # single window (default)
//!   cargo run -p neomacs-bin --bin mock-display -- hsplit   # horizontal split (top/bottom)
//!   cargo run -p neomacs-bin --bin mock-display -- vsplit   # vertical split (left/right)
//!   cargo run -p neomacs-bin --bin mock-display -- triple   # 3-way split
//!   cargo run -p neomacs-bin --bin mock-display -- all      # cycle through all demos

use neomacs_display_protocol::face::{Face, FaceAttributes};
use neomacs_display_protocol::frame_glyphs::{CursorStyle, GlyphRowRole};
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::tty_rif::TtyRif;
use neomacs_display_protocol::types::{Color, Rect};
use std::collections::HashMap;
use std::io::{self, Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("single");

    let (cols, rows) = query_terminal_size().unwrap_or((80, 24));

    setup_terminal();

    if mode == "dump" {
        // Debug: dump desired grid as plain text (no terminal setup, no ANSI)
        let demo = args.get(2).map(|s| s.as_str()).unwrap_or("single");
        let state = build_demo(demo, cols, rows);
        let mut tty = TtyRif::new(cols as usize, rows as usize);
        tty.rasterize(&state);
        for (i, line) in tty.dump_desired().iter().enumerate() {
            println!("{:>2}: |{}|", i, line.trim_end());
        }
        return;
    } else if mode == "all" {
        let demos = ["single", "hsplit", "vsplit", "triple"];
        for demo in demos {
            let state = build_demo(demo, cols, rows);
            let mut tty = TtyRif::new(cols as usize, rows as usize);
            tty.rasterize(&state);
            tty.diff_and_render();
            let output = tty.take_output();
            io::stdout().write_all(&output).unwrap();
            io::stdout().flush().unwrap();

            // Show label and wait
            let label = format!("\x1b[{};1H\x1b[7m [{}] Press any key for next \x1b[0m", rows, demo);
            io::stdout().write_all(label.as_bytes()).unwrap();
            io::stdout().flush().unwrap();
            let _ = io::stdin().read(&mut [0u8]);
        }
    } else {
        let state = build_demo(mode, cols, rows);
        let mut tty = TtyRif::new(cols as usize, rows as usize);
        tty.rasterize(&state);
        tty.diff_and_render();
        let output = tty.take_output();
        io::stdout().write_all(&output).unwrap();
        io::stdout().flush().unwrap();

        let _ = io::stdin().read(&mut [0u8]);
    }

    restore_terminal();
}

// ---------------------------------------------------------------------------
// Demo builders
// ---------------------------------------------------------------------------

fn build_demo(name: &str, cols: u16, rows: u16) -> FrameDisplayState {
    let faces = build_faces();
    match name {
        "hsplit" => build_hsplit(cols, rows, &faces),
        "vsplit" => build_vsplit(cols, rows, &faces),
        "triple" => build_triple(cols, rows, &faces),
        _ => build_single(cols, rows, &faces),
    }
}

/// Single window: *scratch* buffer
fn build_single(cols: u16, rows: u16, faces: &HashMap<u32, Face>) -> FrameDisplayState {
    let c = cols as usize;
    let r = rows as usize;
    let text_rows = r - 2; // mode-line + minibuffer

    let mut state = new_state(cols, rows, faces);

    let scratch_lines = scratch_buffer_lines();
    let matrix = build_text_matrix(text_rows, c, &scratch_lines, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, c as f32, text_rows as f32),
    });

    let ml = build_mode_line(c, " -:**-  *scratch*      Top L1     (Lisp Interaction)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: ml,
        pixel_bounds: Rect::new(0.0, text_rows as f32, c as f32, 1.0),
    });

    let mini = build_minibuffer(c, "For information about GNU Emacs and the GNU system, type C-h C-a.");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32, c as f32, 1.0),
    });

    state
}

/// Horizontal split: *scratch* on top, *Messages* on bottom
fn build_hsplit(cols: u16, rows: u16, faces: &HashMap<u32, Face>) -> FrameDisplayState {
    let c = cols as usize;
    let r = rows as usize;
    let half = (r - 1) / 2; // -1 for minibuffer
    let top_text = half - 1; // -1 for top mode-line
    let bot_text = r - 1 - half - 1; // -1 for bottom mode-line, -1 for minibuffer

    let mut state = new_state(cols, rows, faces);

    // Top window: *scratch*
    let scratch = scratch_buffer_lines();
    let top_matrix = build_text_matrix(top_text, c, &scratch, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: top_matrix,
        pixel_bounds: Rect::new(0.0, 0.0, c as f32, top_text as f32),
    });

    let top_ml = build_mode_line(c, " -:**-  *scratch*      Top L1     (Lisp Interaction)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: top_ml,
        pixel_bounds: Rect::new(0.0, top_text as f32, c as f32, 1.0),
    });

    // Bottom window: *Messages*
    let messages = messages_buffer_lines();
    let bot_y = half;
    let bot_matrix = build_text_matrix(bot_text, c, &messages, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: bot_matrix,
        pixel_bounds: Rect::new(0.0, bot_y as f32, c as f32, bot_text as f32),
    });

    let bot_ml = build_mode_line(c, " -:---  *Messages*     Bot L1     (Messages)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 11,
        matrix: bot_ml,
        pixel_bounds: Rect::new(0.0, (bot_y + bot_text) as f32, c as f32, 1.0),
    });

    // Minibuffer
    let mini = build_minibuffer(c, "");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32, c as f32, 1.0),
    });

    state
}

/// Vertical split: *scratch* on left, help.el on right
fn build_vsplit(cols: u16, rows: u16, faces: &HashMap<u32, Face>) -> FrameDisplayState {
    let c = cols as usize;
    let r = rows as usize;
    let left_w = c / 2;
    let right_w = c - left_w;
    let text_rows = r - 2;

    let mut state = new_state(cols, rows, faces);

    // Left window: *scratch*
    let scratch = scratch_buffer_lines();
    let left_matrix = build_text_matrix(text_rows, left_w, &scratch, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: left_matrix,
        pixel_bounds: Rect::new(0.0, 0.0, left_w as f32, text_rows as f32),
    });

    // Vertical divider: fill column left_w with '|' face 7
    // (handled by adding a border to the right edge of left window)

    // Right window: help.el
    let help = help_buffer_lines();
    let right_matrix = build_text_matrix(text_rows, right_w - 1, &help, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: right_matrix,
        pixel_bounds: Rect::new((left_w + 1) as f32, 0.0, (right_w - 1) as f32, text_rows as f32),
    });

    // Vertical divider column
    let mut divider = GlyphMatrix::new(text_rows, 1);
    for row in &mut divider.rows {
        row.enabled = true;
        row.glyphs[GlyphArea::Text as usize].push(Glyph::char('|', 7, 0));
    }
    divider.ensure_hashes();
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 30,
        matrix: divider,
        pixel_bounds: Rect::new(left_w as f32, 0.0, 1.0, text_rows as f32),
    });

    // Mode-line (spans full width)
    let left_ml_text = format!(" -:**-  *scratch*{:>w$}", "", w = left_w.saturating_sub(17));
    let right_ml_text = format!(" -:---  help.el{:>w$}", "", w = right_w.saturating_sub(15));
    let mut ml_text = left_ml_text;
    ml_text.push('|');
    ml_text.push_str(&right_ml_text);
    let ml = build_mode_line(c, &ml_text);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: ml,
        pixel_bounds: Rect::new(0.0, text_rows as f32, c as f32, 1.0),
    });

    // Minibuffer
    let mini = build_minibuffer(c, "C-x 3 ran the command split-window-right");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32, c as f32, 1.0),
    });

    state
}

/// Triple split: *scratch* left, *Messages* top-right, *Help* bottom-right
fn build_triple(cols: u16, rows: u16, faces: &HashMap<u32, Face>) -> FrameDisplayState {
    let c = cols as usize;
    let r = rows as usize;
    let left_w = c / 2;
    let right_w = c - left_w - 1; // -1 for vertical divider
    let left_text = r - 2;
    let right_half = (r - 1) / 2;
    let top_right_text = right_half - 1;
    let bot_right_text = r - 1 - right_half - 1;

    let mut state = new_state(cols, rows, faces);

    // Left: *scratch* (full height)
    let scratch = scratch_buffer_lines();
    let left_matrix = build_text_matrix(left_text, left_w, &scratch, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: left_matrix,
        pixel_bounds: Rect::new(0.0, 0.0, left_w as f32, left_text as f32),
    });

    // Left mode-line
    let left_ml = build_mode_line_width(left_w, " -:**-  *scratch*      (Lisp Interaction)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: left_ml,
        pixel_bounds: Rect::new(0.0, left_text as f32, left_w as f32, 1.0),
    });

    // Vertical divider
    let mut divider = GlyphMatrix::new(r - 1, 1);
    for row in &mut divider.rows {
        row.enabled = true;
        row.glyphs[GlyphArea::Text as usize].push(Glyph::char('|', 7, 0));
    }
    divider.ensure_hashes();
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 30,
        matrix: divider,
        pixel_bounds: Rect::new(left_w as f32, 0.0, 1.0, (r - 1) as f32),
    });

    // Top-right: *Messages*
    let messages = messages_buffer_lines();
    let tr_matrix = build_text_matrix(top_right_text, right_w, &messages, 0, false);
    let rx = (left_w + 1) as f32;
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: tr_matrix,
        pixel_bounds: Rect::new(rx, 0.0, right_w as f32, top_right_text as f32),
    });

    // Top-right mode-line
    let tr_ml = build_mode_line_width(right_w, " -:---  *Messages*     (Messages)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 11,
        matrix: tr_ml,
        pixel_bounds: Rect::new(rx, top_right_text as f32, right_w as f32, 1.0),
    });

    // Bottom-right: *Help*
    let help = help_buffer_lines();
    let br_y = right_half as f32;
    let br_matrix = build_text_matrix(bot_right_text, right_w, &help, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 3,
        matrix: br_matrix,
        pixel_bounds: Rect::new(rx, br_y, right_w as f32, bot_right_text as f32),
    });

    // Bottom-right mode-line
    let br_ml = build_mode_line_width(right_w, " -:---  *Help*         (Help)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 12,
        matrix: br_ml,
        pixel_bounds: Rect::new(rx, br_y + bot_right_text as f32, right_w as f32, 1.0),
    });

    // Minibuffer
    let mini = build_minibuffer(c, "");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32, c as f32, 1.0),
    });

    state
}

// ---------------------------------------------------------------------------
// Buffer content generators
// ---------------------------------------------------------------------------

fn scratch_buffer_lines() -> Vec<(&'static str, u32)> {
    vec![
        (";; This is the *scratch* buffer.", 5),
        ("", 0),
        ("(defun hello (name)", 3),
        ("  \"Say hello to NAME.\"", 4),
        ("  (message \"Hello, %s!\" name))", 0),
        ("", 0),
        (";; Type C-x C-e to evaluate", 5),
        ("", 0),
        ("(setq neomacs-version \"0.1.0\")", 3),
        ("(setq display-pipeline 'glyph-matrix)", 3),
        ("", 0),
        (";; GNU Emacs compatible glyph matrix model", 5),
        (";; TTY rendering via TtyRif", 5),
        (";; Single-thread, no channel, matching GNU", 5),
    ]
}

fn messages_buffer_lines() -> Vec<(&'static str, u32)> {
    vec![
        ("Loading /usr/share/emacs/site-lisp/...", 0),
        ("For information about GNU Emacs, type C-h C-a.", 0),
        ("Starting new Emacs daemon...", 0),
        ("Loaded custom theme 'modus-vivendi'", 4),
        ("Loading org-mode...done", 0),
        ("Mark set", 5),
        ("Quit", 3),
        ("Buffer is read-only: *Messages*", 3),
        ("", 0),
    ]
}

fn help_buffer_lines() -> Vec<(&'static str, u32)> {
    vec![
        ("GNU Emacs Manual", 3),
        ("================", 0),
        ("", 0),
        ("  Emacs is the extensible,", 0),
        ("  customizable, self-documenting", 0),
        ("  real-time display editor.", 0),
        ("", 0),
        (";; Key Bindings:", 5),
        ("  C-x C-f  Find file", 0),
        ("  C-x C-s  Save file", 0),
        ("  C-x b    Switch buffer", 0),
        ("  C-x 2    Split horizontal", 0),
        ("  C-x 3    Split vertical", 0),
        ("  C-x 0    Delete window", 0),
        ("  C-x 1    Delete other windows", 0),
        ("  C-g      Keyboard quit", 0),
    ]
}

// ---------------------------------------------------------------------------
// GlyphMatrix builders
// ---------------------------------------------------------------------------

fn build_text_matrix(
    nrows: usize,
    ncols: usize,
    lines: &[(&str, u32)],
    cursor_row: usize,
    show_cursor: bool,
) -> GlyphMatrix {
    let lnum_width = 4;
    let mut matrix = GlyphMatrix::new(nrows, ncols);

    for (row_idx, row) in matrix.rows.iter_mut().enumerate() {
        row.role = GlyphRowRole::Text;
        row.enabled = true;

        // Line number
        let lnum_str = format!("{:>3} ", row_idx + 1);
        for ch in lnum_str.chars() {
            row.glyphs[GlyphArea::LeftMargin as usize].push(Glyph::char(ch, 2, 0));
        }

        // Text
        if row_idx < lines.len() {
            let (line, face_id) = lines[row_idx];
            for (i, ch) in line.chars().enumerate() {
                if lnum_width + i >= ncols {
                    break;
                }
                row.glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, face_id, 0));
            }
            row.displays_text = !line.is_empty();
        }

        if show_cursor && row_idx == cursor_row {
            row.cursor_col = Some(0);
            row.cursor_type = Some(CursorStyle::FilledBox);
        }
    }
    matrix.ensure_hashes();
    matrix
}

fn build_mode_line(ncols: usize, text: &str) -> GlyphMatrix {
    build_mode_line_width(ncols, text)
}

fn build_mode_line_width(ncols: usize, text: &str) -> GlyphMatrix {
    let mut ml = GlyphMatrix::new(1, ncols);
    ml.rows[0].role = GlyphRowRole::ModeLine;
    ml.rows[0].enabled = true;
    ml.rows[0].mode_line = true;
    for ch in text.chars().take(ncols) {
        ml.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 1, 0));
    }
    while ml.rows[0].glyphs[GlyphArea::Text as usize].len() < ncols {
        ml.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char(' ', 1, 0));
    }
    ml.ensure_hashes();
    ml
}

fn build_minibuffer(ncols: usize, text: &str) -> GlyphMatrix {
    let mut mini = GlyphMatrix::new(1, ncols);
    mini.rows[0].role = GlyphRowRole::Minibuffer;
    mini.rows[0].enabled = true;
    for ch in text.chars().take(ncols) {
        mini.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 6, 0));
    }
    mini.ensure_hashes();
    mini
}

// ---------------------------------------------------------------------------
// Face + state helpers
// ---------------------------------------------------------------------------

fn build_faces() -> HashMap<u32, Face> {
    let mut faces = HashMap::new();
    // 0: default (white on black)
    faces.insert(0, make_face(0, Color::new(0.87, 0.87, 0.87, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 400, false));
    // 1: mode-line (black on light blue, bold)
    faces.insert(1, make_face(1, Color::new(0.0, 0.0, 0.0, 1.0), Color::new(0.6, 0.7, 0.9, 1.0), 700, false));
    // 2: line-number (gray on black)
    faces.insert(2, make_face(2, Color::new(0.5, 0.5, 0.5, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 400, false));
    // 3: keyword (orange, bold)
    faces.insert(3, make_face(3, Color::new(1.0, 0.6, 0.2, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 700, false));
    // 4: string (green)
    faces.insert(4, make_face(4, Color::new(0.4, 0.9, 0.4, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 400, false));
    // 5: comment (dim cyan, italic)
    faces.insert(5, make_face(5, Color::new(0.4, 0.7, 0.7, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 400, true));
    // 6: minibuffer (white on dark gray)
    faces.insert(6, make_face(6, Color::new(0.87, 0.87, 0.87, 1.0), Color::new(0.15, 0.15, 0.15, 1.0), 400, false));
    // 7: vertical border (gray on black)
    faces.insert(7, make_face(7, Color::new(0.4, 0.4, 0.4, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 400, false));
    faces
}

fn new_state(cols: u16, rows: u16, faces: &HashMap<u32, Face>) -> FrameDisplayState {
    let mut state = FrameDisplayState::new(cols as usize, rows as usize, 1.0, 1.0);
    state.frame_pixel_width = cols as f32;
    state.frame_pixel_height = rows as f32;
    state.background = Color::new(0.0, 0.0, 0.0, 1.0);
    state.faces = faces.clone();
    state
}

fn make_face(id: u32, fg: Color, bg: Color, weight: u16, italic: bool) -> Face {
    let mut attrs = FaceAttributes::empty();
    if italic {
        attrs |= FaceAttributes::ITALIC;
    }
    let mut face = Face::new(id);
    face.foreground = fg;
    face.background = bg;
    face.font_weight = weight;
    face.attributes = attrs;
    face
}

// ---------------------------------------------------------------------------
// Terminal helpers
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn setup_terminal() {
    use std::os::unix::io::AsRawFd;
    let fd = io::stdin().as_raw_fd();
    unsafe {
        let mut raw = std::mem::zeroed::<libc::termios>();
        libc::tcgetattr(fd, &mut raw);
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(fd, libc::TCSANOW, &raw);
    }
    print!("\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H");
    io::stdout().flush().unwrap();
}

#[cfg(unix)]
fn restore_terminal() {
    print!("\x1b[?25h\x1b[?1049l");
    io::stdout().flush().unwrap();
}

#[cfg(not(unix))]
fn setup_terminal() {
    print!("\x1b[2J\x1b[H");
    io::stdout().flush().unwrap();
}

#[cfg(not(unix))]
fn restore_terminal() {}

#[cfg(unix)]
fn query_terminal_size() -> Option<(u16, u16)> {
    unsafe {
        let mut winsize = std::mem::MaybeUninit::<libc::winsize>::uninit();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, winsize.as_mut_ptr()) == 0 {
            let w = winsize.assume_init();
            if w.ws_col > 0 && w.ws_row > 0 {
                return Some((w.ws_col, w.ws_row));
            }
        }
    }
    None
}

#[cfg(not(unix))]
fn query_terminal_size() -> Option<(u16, u16)> {
    None
}
