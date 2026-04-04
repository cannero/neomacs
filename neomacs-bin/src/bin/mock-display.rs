//! Mock display test — renders a fake Emacs frame via TtyRif.
//!
//! Usage: cargo run -p neomacs-bin --bin mock-display
//!
//! This bypasses the evaluator and bootstrap entirely, feeding
//! a hand-built FrameDisplayState into the TTY pipeline to verify
//! the display refactor works visually.

use neomacs_display_protocol::face::{Face, FaceAttributes};
use neomacs_display_protocol::frame_glyphs::{CursorStyle, GlyphRowRole};
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::tty_rif::TtyRif;
use neomacs_display_protocol::types::{Color, Rect};
use std::collections::HashMap;
use std::io::{self, Read, Write};

fn main() {
    let (cols, rows) = query_terminal_size().unwrap_or((80, 24));
    let char_w = 1.0f32; // TTY: 1 cell = 1 unit
    let char_h = 1.0f32;

    // --- Build faces ---
    let mut faces: HashMap<u32, Face> = HashMap::new();

    // Face 0: default (white on black)
    faces.insert(0, make_face(0,
        Color::new(0.87, 0.87, 0.87, 1.0), // fg: light gray
        Color::new(0.0, 0.0, 0.0, 1.0),    // bg: black
        400, false,
    ));

    // Face 1: mode-line (black on light blue)
    faces.insert(1, make_face(1,
        Color::new(0.0, 0.0, 0.0, 1.0),    // fg: black
        Color::new(0.6, 0.7, 0.9, 1.0),    // bg: light blue
        700, false,
    ));

    // Face 2: line number (dim gray on black)
    faces.insert(2, make_face(2,
        Color::new(0.5, 0.5, 0.5, 1.0),    // fg: gray
        Color::new(0.0, 0.0, 0.0, 1.0),    // bg: black
        400, false,
    ));

    // Face 3: keyword (orange on black, bold)
    faces.insert(3, make_face(3,
        Color::new(1.0, 0.6, 0.2, 1.0),    // fg: orange
        Color::new(0.0, 0.0, 0.0, 1.0),    // bg: black
        700, false,
    ));

    // Face 4: string (green on black)
    faces.insert(4, make_face(4,
        Color::new(0.4, 0.9, 0.4, 1.0),    // fg: green
        Color::new(0.0, 0.0, 0.0, 1.0),    // bg: black
        400, false,
    ));

    // Face 5: comment (dim cyan on black, italic)
    faces.insert(5, make_face(5,
        Color::new(0.4, 0.7, 0.7, 1.0),    // fg: dim cyan
        Color::new(0.0, 0.0, 0.0, 1.0),    // bg: black
        400, true,
    ));

    // Face 6: minibuffer (white on dark gray)
    faces.insert(6, make_face(6,
        Color::new(0.87, 0.87, 0.87, 1.0),
        Color::new(0.15, 0.15, 0.15, 1.0),
        400, false,
    ));

    // --- Build the frame content ---
    // Emulate: *scratch* buffer with some Lisp code
    let text_rows = rows as usize - 2; // reserve 1 for mode-line, 1 for minibuffer
    let text_cols = cols as usize;

    let buffer_lines = vec![
        (";; This is the *scratch* buffer.", 5), // comment face
        ("", 0),
        ("(defun hello (name)", 3),              // keyword face
        ("  \"Say hello to NAME.\"", 4),         // string face
        ("  (message \"Hello, %s!\" name))", 0), // default face
        ("", 0),
        (";; Type C-x C-e to evaluate", 5),      // comment
        ("", 0),
        ("(setq neomacs-version \"0.1.0\")", 3),  // keyword
        ("(setq display-pipeline 'glyph-matrix)", 3),
        ("", 0),
        (";; GNU Emacs compatible glyph matrix model", 5),
        (";; TTY rendering via TtyRif", 5),
        (";; Single-thread, no channel, matching GNU", 5),
    ];

    let mode_line_text = format!(
        " -:**-  *scratch*      Top L1     (Lisp Interaction) {:>width$}",
        "",
        width = text_cols.saturating_sub(55)
    );

    let minibuffer_text = "For information about GNU Emacs and the GNU system, type C-h C-a.";

    // --- Build GlyphMatrix ---
    let mut matrix = GlyphMatrix::new(text_rows, text_cols);
    let lnum_width = 4; // "  1 " = 4 chars

    for (row_idx, row) in matrix.rows.iter_mut().enumerate() {
        row.role = GlyphRowRole::Text;
        row.enabled = true;

        // Line number (left margin)
        let lnum_str = format!("{:>3} ", row_idx + 1);
        for ch in lnum_str.chars() {
            row.glyphs[GlyphArea::LeftMargin as usize].push(Glyph::char(ch, 2, 0));
        }

        // Text content
        if row_idx < buffer_lines.len() {
            let (line, face_id) = buffer_lines[row_idx];
            for ch in line.chars() {
                row.glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, face_id, 0));
            }
            row.displays_text = !line.is_empty();
        }

        // Cursor on row 0, col 0 of text area
        if row_idx == 0 {
            row.cursor_col = Some(0);
            row.cursor_type = Some(CursorStyle::FilledBox);
        }
    }
    matrix.ensure_hashes();

    // Mode-line row
    let mut ml_matrix = GlyphMatrix::new(1, text_cols);
    ml_matrix.rows[0].role = GlyphRowRole::ModeLine;
    ml_matrix.rows[0].enabled = true;
    ml_matrix.rows[0].mode_line = true;
    for ch in mode_line_text.chars().take(text_cols) {
        ml_matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 1, 0));
    }
    // Pad to full width
    while ml_matrix.rows[0].glyphs[GlyphArea::Text as usize].len() < text_cols {
        ml_matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char(' ', 1, 0));
    }
    ml_matrix.ensure_hashes();

    // Minibuffer row
    let mut mini_matrix = GlyphMatrix::new(1, text_cols);
    mini_matrix.rows[0].role = GlyphRowRole::Minibuffer;
    mini_matrix.rows[0].enabled = true;
    for ch in minibuffer_text.chars().take(text_cols) {
        mini_matrix.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 6, 0));
    }
    mini_matrix.ensure_hashes();

    // --- Assemble FrameDisplayState ---
    let pixel_w = cols as f32 * char_w;
    let pixel_h = rows as f32 * char_h;

    let mut state = FrameDisplayState::new(cols as usize, rows as usize, char_w, char_h);
    state.frame_pixel_width = pixel_w;
    state.frame_pixel_height = pixel_h;
    state.background = Color::new(0.0, 0.0, 0.0, 1.0);
    state.faces = faces;

    // Text area window
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, pixel_w, text_rows as f32 * char_h),
    });

    // Mode-line window
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: ml_matrix,
        pixel_bounds: Rect::new(0.0, text_rows as f32 * char_h, pixel_w, char_h),
    });

    // Minibuffer window
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 3,
        matrix: mini_matrix,
        pixel_bounds: Rect::new(0.0, (rows as usize - 1) as f32 * char_h, pixel_w, char_h),
    });

    // Cursor
    state.cursors.push(CursorItem {
        window_id: 1,
        x: lnum_width as f32 * char_w,
        y: 0.0,
        width: char_w,
        height: char_h,
        style: CursorStyle::FilledBox,
        color: Color::new(0.87, 0.87, 0.87, 1.0),
    });

    // --- Render via TtyRif ---
    setup_terminal();

    let mut tty = TtyRif::new(cols as usize, rows as usize);
    tty.rasterize(&state);
    tty.diff_and_render();
    let output = tty.take_output();

    let mut stdout = io::stdout();
    stdout.write_all(&output).unwrap();
    stdout.flush().unwrap();

    // Wait for keypress then exit
    eprintln!("\n\r[Press any key to exit]");
    let _ = io::stdin().read(&mut [0u8]);

    restore_terminal();
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

#[cfg(unix)]
fn setup_terminal() {
    use std::os::unix::io::AsRawFd;
    let fd = io::stdin().as_raw_fd();
    unsafe {
        let mut termios = std::mem::zeroed::<libc::termios>();
        libc::tcgetattr(fd, &mut termios);
        // Save original termios (we'll just reset on exit)
        let mut raw = termios;
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(fd, libc::TCSANOW, &raw);
    }
    // Alternate screen + hide cursor
    print!("\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H");
    io::stdout().flush().unwrap();
}

#[cfg(unix)]
fn restore_terminal() {
    // Show cursor + leave alternate screen
    print!("\x1b[?25h\x1b[?1049l");
    io::stdout().flush().unwrap();
    // Reset terminal (cooked mode restored by OS on exit)
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
