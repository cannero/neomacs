//! Mock GUI display test — renders a fake Emacs frame via the wgpu render thread.
//!
//! Usage: cargo run -p neomacs-bin --bin mock-display-gui
//!        cargo run -p neomacs-bin --bin mock-display-gui -- vsplit
//!        cargo run -p neomacs-bin --bin mock-display-gui -- hsplit
//!        cargo run -p neomacs-bin --bin mock-display-gui -- triple
//!
//! This bypasses the evaluator and bootstrap, feeding a hand-built
//! FrameDisplayState through the real GPU render pipeline.

use neomacs_display_protocol::face::{Face, FaceAttributes};
use neomacs_display_protocol::frame_glyphs::{CursorStyle, GlyphRowRole};
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::types::{Color, Rect};
use neomacs_display_runtime::render_thread::{RenderThread, SharedImageDimensions, SharedMonitorInfo};
use neomacs_display_runtime::thread_comm::ThreadComms;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("warn".parse().unwrap()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("single");

    let width = 800u32;
    let height = 600u32;
    let char_w = 8.0f32;
    let char_h = 16.0f32;
    let cols = (width as f32 / char_w) as u16;  // 100
    let rows = (height as f32 / char_h) as u16; // 37

    // Create communication channels
    let comms = ThreadComms::new().expect("failed to create thread comms");
    let (emacs_comms, render_comms) = comms.split();

    // Spawn the GPU render thread
    let image_dimensions: SharedImageDimensions = Arc::new(Mutex::new(HashMap::new()));
    let shared_monitors: SharedMonitorInfo =
        Arc::new((Mutex::new(Vec::new()), std::sync::Condvar::new()));

    let render_thread = RenderThread::spawn(
        render_comms,
        width,
        height,
        format!("Neomacs Mock — {}", mode),
        Arc::clone(&image_dimensions),
        Arc::clone(&shared_monitors),
    )
    .unwrap_or_else(|err| {
        eprintln!("Failed to start render thread: {err}");
        std::process::exit(1);
    });

    eprintln!("Render thread spawned ({}x{}, {}x{} cells)", width, height, cols, rows);

    // Build the mock frame
    let state = build_demo(mode, cols, rows, char_w, char_h, width as f32, height as f32);

    // Send it to the render thread
    if let Err(err) = emacs_comms.frame_tx.send(state.clone()) {
        eprintln!("Failed to send frame: {err}");
    }
    eprintln!("Frame sent to render thread");

    // Keep sending the same frame periodically (the render thread might need
    // it after initialization completes) and drain input events
    loop {
        // Re-send frame every 100ms to ensure it gets picked up
        std::thread::sleep(Duration::from_millis(100));

        // Drain input events (so the channel doesn't fill up)
        while let Ok(event) = emacs_comms.input_rx.try_recv() {
            match event {
                neomacs_display_runtime::thread_comm::InputEvent::Key { keysym, .. } => {
                    // 'q' or Escape to quit
                    if keysym == b'q' as u32 || keysym == 0xff1b {
                        eprintln!("Quit key pressed, shutting down...");
                        let _ = emacs_comms.cmd_tx.send(
                            neomacs_display_runtime::thread_comm::RenderCommand::Shutdown,
                        );
                        render_thread.join();
                        return;
                    }
                }
                _ => {}
            }
        }

        // Re-send frame
        let _ = emacs_comms.frame_tx.try_send(state.clone());
    }
}

// ---------------------------------------------------------------------------
// Demo builders (same as mock-display TTY but with pixel coordinates)
// ---------------------------------------------------------------------------

fn build_demo(
    name: &str,
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    pixel_h: f32,
) -> FrameDisplayState {
    let faces = build_faces();
    match name {
        "hsplit" => build_hsplit(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces),
        "vsplit" => build_vsplit(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces),
        "triple" => build_triple(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces),
        _ => build_single(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces),
    }
}

fn build_single(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameDisplayState {
    let c = cols as usize;
    let r = rows as usize;
    let text_rows = r - 2;

    let mut state = new_state(cols, rows, char_w, char_h, pixel_w, pixel_h, faces);

    let scratch = scratch_buffer_lines();
    let matrix = build_text_matrix(text_rows, c, &scratch, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix,
        pixel_bounds: Rect::new(0.0, 0.0, pixel_w, text_rows as f32 * char_h),
    });

    let ml = build_mode_line(c, " -:**-  *scratch*      Top L1     (Lisp Interaction)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: ml,
        pixel_bounds: Rect::new(0.0, text_rows as f32 * char_h, pixel_w, char_h),
    });

    let mini = build_minibuffer(c, "For information about GNU Emacs and the GNU system, type C-h C-a.");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32 * char_h, pixel_w, char_h),
    });

    state
}

fn build_hsplit(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameDisplayState {
    let c = cols as usize;
    let r = rows as usize;
    let half = (r - 1) / 2;
    let top_text = half - 1;
    let bot_text = r - 1 - half - 1;

    let mut state = new_state(cols, rows, char_w, char_h, pixel_w, pixel_h, faces);

    // Top: *scratch*
    let scratch = scratch_buffer_lines();
    let top_matrix = build_text_matrix(top_text, c, &scratch, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: top_matrix,
        pixel_bounds: Rect::new(0.0, 0.0, pixel_w, top_text as f32 * char_h),
    });
    let top_ml = build_mode_line(c, " -:**-  *scratch*      Top L1     (Lisp Interaction)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: top_ml,
        pixel_bounds: Rect::new(0.0, top_text as f32 * char_h, pixel_w, char_h),
    });

    // Bottom: *Messages*
    let messages = messages_buffer_lines();
    let bot_y = half as f32 * char_h;
    let bot_matrix = build_text_matrix(bot_text, c, &messages, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: bot_matrix,
        pixel_bounds: Rect::new(0.0, bot_y, pixel_w, bot_text as f32 * char_h),
    });
    let bot_ml = build_mode_line(c, " -:---  *Messages*     Bot L1     (Messages)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 11,
        matrix: bot_ml,
        pixel_bounds: Rect::new(0.0, bot_y + bot_text as f32 * char_h, pixel_w, char_h),
    });

    // Minibuffer
    let mini = build_minibuffer(c, "");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32 * char_h, pixel_w, char_h),
    });

    state
}

fn build_vsplit(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameDisplayState {
    let c = cols as usize;
    let r = rows as usize;
    let left_cols = c / 2;
    let right_cols = c - left_cols - 1;
    let text_rows = r - 2;

    let mut state = new_state(cols, rows, char_w, char_h, pixel_w, pixel_h, faces);

    // Left: *scratch*
    let scratch = scratch_buffer_lines();
    let left_matrix = build_text_matrix(text_rows, left_cols, &scratch, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: left_matrix,
        pixel_bounds: Rect::new(0.0, 0.0, left_cols as f32 * char_w, text_rows as f32 * char_h),
    });

    // Vertical divider
    let mut divider = GlyphMatrix::new(text_rows, 1);
    for row in &mut divider.rows {
        row.enabled = true;
        row.glyphs[GlyphArea::Text as usize].push(Glyph::char('|', 7, 0));
    }
    divider.ensure_hashes();
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 30,
        matrix: divider,
        pixel_bounds: Rect::new(
            left_cols as f32 * char_w,
            0.0,
            char_w,
            text_rows as f32 * char_h,
        ),
    });

    // Right: help.el
    let help = help_buffer_lines();
    let right_matrix = build_text_matrix(text_rows, right_cols, &help, 0, false);
    let right_x = (left_cols + 1) as f32 * char_w;
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: right_matrix,
        pixel_bounds: Rect::new(right_x, 0.0, right_cols as f32 * char_w, text_rows as f32 * char_h),
    });

    // Mode-line
    let left_ml_text = format!(" -:**-  *scratch*{:>w$}", "", w = left_cols.saturating_sub(17));
    let right_ml_text = format!(" -:---  help.el{:>w$}", "", w = right_cols.saturating_sub(15));
    let ml_text = format!("{}|{}", left_ml_text, right_ml_text);
    let ml = build_mode_line(c, &ml_text);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: ml,
        pixel_bounds: Rect::new(0.0, text_rows as f32 * char_h, pixel_w, char_h),
    });

    // Minibuffer
    let mini = build_minibuffer(c, "C-x 3 ran the command split-window-right");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32 * char_h, pixel_w, char_h),
    });

    state
}

fn build_triple(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameDisplayState {
    let c = cols as usize;
    let r = rows as usize;
    let left_cols = c / 2;
    let right_cols = c - left_cols - 1;
    let left_text = r - 2;
    let right_half = (r - 1) / 2;
    let top_right_text = right_half - 1;
    let bot_right_text = r - 1 - right_half - 1;

    let mut state = new_state(cols, rows, char_w, char_h, pixel_w, pixel_h, faces);

    // Left: *scratch*
    let scratch = scratch_buffer_lines();
    let left = build_text_matrix(left_text, left_cols, &scratch, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: left,
        pixel_bounds: Rect::new(0.0, 0.0, left_cols as f32 * char_w, left_text as f32 * char_h),
    });
    let left_ml = build_mode_line_width(left_cols, " -:**-  *scratch*      (Lisp Interaction)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: left_ml,
        pixel_bounds: Rect::new(0.0, left_text as f32 * char_h, left_cols as f32 * char_w, char_h),
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
        pixel_bounds: Rect::new(left_cols as f32 * char_w, 0.0, char_w, (r - 1) as f32 * char_h),
    });

    let rx = (left_cols + 1) as f32 * char_w;

    // Top-right: *Messages*
    let messages = messages_buffer_lines();
    let tr = build_text_matrix(top_right_text, right_cols, &messages, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: tr,
        pixel_bounds: Rect::new(rx, 0.0, right_cols as f32 * char_w, top_right_text as f32 * char_h),
    });
    let tr_ml = build_mode_line_width(right_cols, " -:---  *Messages*     (Messages)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 11,
        matrix: tr_ml,
        pixel_bounds: Rect::new(rx, top_right_text as f32 * char_h, right_cols as f32 * char_w, char_h),
    });

    // Bottom-right: *Help*
    let help = help_buffer_lines();
    let br_y = right_half as f32 * char_h;
    let br = build_text_matrix(bot_right_text, right_cols, &help, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 3,
        matrix: br,
        pixel_bounds: Rect::new(rx, br_y, right_cols as f32 * char_w, bot_right_text as f32 * char_h),
    });
    let br_ml = build_mode_line_width(right_cols, " -:---  *Help*         (Help)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 12,
        matrix: br_ml,
        pixel_bounds: Rect::new(rx, br_y + bot_right_text as f32 * char_h, right_cols as f32 * char_w, char_h),
    });

    // Minibuffer
    let mini = build_minibuffer(c, "");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32 * char_h, pixel_w, char_h),
    });

    state
}

// ---------------------------------------------------------------------------
// Shared builders (identical logic to mock-display.rs)
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
        let lnum_str = format!("{:>3} ", row_idx + 1);
        for ch in lnum_str.chars() {
            row.glyphs[GlyphArea::LeftMargin as usize].push(Glyph::char(ch, 2, 0));
        }
        if row_idx < lines.len() {
            let (line, face_id) = lines[row_idx];
            for (i, ch) in line.chars().enumerate() {
                if lnum_width + i >= ncols { break; }
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

fn build_faces() -> HashMap<u32, Face> {
    let mut faces = HashMap::new();
    faces.insert(0, make_face(0, Color::new(0.87, 0.87, 0.87, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 400, false));
    faces.insert(1, make_face(1, Color::new(0.0, 0.0, 0.0, 1.0), Color::new(0.6, 0.7, 0.9, 1.0), 700, false));
    faces.insert(2, make_face(2, Color::new(0.5, 0.5, 0.5, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 400, false));
    faces.insert(3, make_face(3, Color::new(1.0, 0.6, 0.2, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 700, false));
    faces.insert(4, make_face(4, Color::new(0.4, 0.9, 0.4, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 400, false));
    faces.insert(5, make_face(5, Color::new(0.4, 0.7, 0.7, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 400, true));
    faces.insert(6, make_face(6, Color::new(0.87, 0.87, 0.87, 1.0), Color::new(0.15, 0.15, 0.15, 1.0), 400, false));
    faces.insert(7, make_face(7, Color::new(0.4, 0.4, 0.4, 1.0), Color::new(0.0, 0.0, 0.0, 1.0), 400, false));
    faces
}

fn new_state(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameDisplayState {
    let mut state = FrameDisplayState::new(cols as usize, rows as usize, char_w, char_h);
    state.frame_pixel_width = pixel_w;
    state.frame_pixel_height = pixel_h;
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
