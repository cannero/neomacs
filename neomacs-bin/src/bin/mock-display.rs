//! Mock display test — renders fake Emacs frames via TTY or GUI.
//!
//! Usage:
//!   mock-display [OPTIONS] [DEMO]
//!
//! DEMO: single (default), hsplit, vsplit, triple, all
//!
//! OPTIONS:
//!   --gui       Render via wgpu GPU window instead of TTY
//!   --dump      Dump grid as plain text (no terminal setup)
//!
//! Examples:
//!   cargo run -p neomacs --bin mock-display              # TTY single
//!   cargo run -p neomacs --bin mock-display -- vsplit     # TTY vsplit
//!   cargo run -p neomacs --bin mock-display -- --gui      # GUI single
//!   cargo run -p neomacs --bin mock-display -- --gui vsplit
//!   cargo run -p neomacs --bin mock-display -- --dump hsplit

use neomacs_display_protocol::face::{Face, FaceAttributes};
use neomacs_display_protocol::frame_glyphs::{CursorStyle, GlyphRowRole};
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::tty_rif::TtyRif;
use neomacs_display_protocol::types::{Color, Rect};
use std::collections::HashMap;
use std::io::{self, Read, Write};

/// A demo scene: main frame + zero or more child frames.
/// The first element is always the main frame (parent_id == 0);
/// subsequent elements are child frames with parent_id set.
#[derive(Clone)]
struct Scene(Vec<FrameDisplayState>);

impl Scene {
    fn iter(&self) -> impl Iterator<Item = &FrameDisplayState> {
        self.0.iter()
    }
}

impl From<FrameDisplayState> for Scene {
    fn from(s: FrameDisplayState) -> Self { Scene(vec![s]) }
}

impl From<Vec<FrameDisplayState>> for Scene {
    fn from(v: Vec<FrameDisplayState>) -> Self { Scene(v) }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let gui = args.iter().any(|a| a == "--gui");
    let dump = args.iter().any(|a| a == "--dump");
    let demo = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .unwrap_or("default");

    if gui {
        run_gui(demo);
    } else if dump {
        run_dump(demo);
    } else {
        run_tty(demo);
    }
}

// ===================================================================
// TTY backend
// ===================================================================

fn run_tty(demo: &str) {
    let (cols, rows) = query_terminal_size().unwrap_or((80, 24));
    let scene = build_demo(demo, cols, rows, 1.0, 1.0, cols as f32, rows as f32);
    let state = scene_for_tty(scene);

    setup_terminal();

    if demo == "all" {
        for name in &["default", "single", "hsplit", "vsplit", "triple"] {
            let ss = build_demo(name, cols, rows, 1.0, 1.0, cols as f32, rows as f32);
            let s = scene_for_tty(ss);
            let mut tty = TtyRif::new(cols as usize, rows as usize);
            tty.rasterize(&s);
            tty.diff_and_render();
            let out = tty.take_output();
            io::stdout().write_all(&out).unwrap();
            io::stdout().flush().unwrap();
            let label = format!("\x1b[{};1H\x1b[7m [{}] Press key \x1b[0m", rows, name);
            io::stdout().write_all(label.as_bytes()).unwrap();
            io::stdout().flush().unwrap();
            let _ = io::stdin().read(&mut [0u8]);
        }
    } else {
        let mut tty = TtyRif::new(cols as usize, rows as usize);
        tty.rasterize(&state);
        tty.diff_and_render();
        let out = tty.take_output();
        io::stdout().write_all(&out).unwrap();
        io::stdout().flush().unwrap();
        let _ = io::stdin().read(&mut [0u8]);
    }

    restore_terminal();
}

// ===================================================================
// Dump mode (plain text, no ANSI)
// ===================================================================

fn run_dump(demo: &str) {
    let (cols, rows) = query_terminal_size().unwrap_or((80, 24));
    let scene = build_demo(demo, cols, rows, 1.0, 1.0, cols as f32, rows as f32);
    let state = scene_for_tty(scene);
    let mut tty = TtyRif::new(cols as usize, rows as usize);
    tty.rasterize(&state);
    for (i, line) in tty.dump_desired().iter().enumerate() {
        println!("{:>2}: |{}|", i, line.trim_end());
    }
}

// ===================================================================
// GUI backend
// ===================================================================

fn run_gui(demo: &str) {
    use neomacs_display_runtime::render_thread::{
        RenderThread, SharedImageDimensions, SharedMonitorInfo,
    };
    use neomacs_display_runtime::thread_comm::{InputEvent, RenderCommand, ThreadComms};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let _logging_guard = neovm_core::logging::init(neovm_core::logging::LogTarget::Stdout);

    // Use logical pixels (winit handles DPI scaling internally).
    // The render thread's scale_factor from winit converts logical → physical.
    let char_w = 8.0f32;
    let char_h = 16.0f32;
    let cols = 130u16;
    let rows = 50u16;
    let width = (cols as f32 * char_w) as u32;
    let height = (rows as f32 * char_h) as u32;

    let comms = ThreadComms::new().expect("failed to create comms");
    let (emacs_comms, render_comms) = comms.split();

    let image_dims: SharedImageDimensions =
        Arc::new((Mutex::new(HashMap::new()), std::sync::Condvar::new()));
    let shared_monitors: SharedMonitorInfo =
        Arc::new((Mutex::new(Vec::new()), std::sync::Condvar::new()));

    let render_thread = RenderThread::spawn(
        render_comms,
        width,
        height,
        format!("Neomacs Mock — {}", demo),
        Arc::clone(&image_dims),
        Arc::clone(&shared_monitors),
    )
    .unwrap_or_else(|err| {
        eprintln!("Failed to start render thread: {err}");
        std::process::exit(1);
    });

    eprintln!(
        "GUI mock: {}x{} px, {}x{} cells, demo={}",
        width, height, cols, rows, demo
    );

    let scene = build_demo(
        demo,
        cols,
        rows,
        char_w,
        char_h,
        width as f32,
        height as f32,
    );
    for s in scene.iter() {
        let _ = emacs_comms.frame_tx.send(s.clone());
    }

    // Event loop: re-send frames, drain input, quit on 'q'/Escape
    loop {
        std::thread::sleep(Duration::from_millis(100));
        while let Ok(event) = emacs_comms.input_rx.try_recv() {
            if let InputEvent::Key { keysym, .. } = event {
                if keysym == b'q' as u32 || keysym == 0xff1b {
                    let _ = emacs_comms.cmd_tx.send(RenderCommand::Shutdown);
                    render_thread.join();
                    return;
                }
            }
        }
        for s in scene.iter() {
            let _ = emacs_comms.frame_tx.try_send(s.clone());
        }
    }
}

/// Flatten a Scene into a single FrameDisplayState for TTY rasterization.
/// Child frames are merged into the main frame at their parent_x/parent_y offset.
fn scene_for_tty(scene: Scene) -> FrameDisplayState {
    let mut iter = scene.0.into_iter();
    let mut main = iter.next().expect("Scene must have a main frame");
    for child in iter {
        let ox = child.parent_x;
        let oy = child.parent_y;
        for mut entry in child.window_matrices {
            entry.pixel_bounds.x += ox;
            entry.pixel_bounds.y += oy;
            main.window_matrices.push(entry);
        }
        for mut bg in child.backgrounds {
            bg.bounds.x += ox;
            bg.bounds.y += oy;
            main.backgrounds.push(bg);
        }
        for mut border in child.borders {
            border.x += ox;
            border.y += oy;
            main.borders.push(border);
        }
        for (id, face) in &child.faces {
            main.faces.entry(*id).or_insert_with(|| face.clone());
        }
    }
    main
}

// ===================================================================
// Shared mock data builders
// ===================================================================

fn build_demo(
    name: &str,
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    pixel_h: f32,
) -> Scene {
    let faces = build_faces();
    match name {
        "default" => build_default(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces).into(),
        "hsplit" => build_hsplit(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces).into(),
        "vsplit" => build_vsplit(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces).into(),
        "triple" => build_triple(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces).into(),
        _ => build_single(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces).into(),
    }
}

// -------------------------------------------------------------------
// Buffer content
// -------------------------------------------------------------------

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

// -------------------------------------------------------------------
// Layout builders
// -------------------------------------------------------------------

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
        selected: true,
    });
    let ml = build_mode_line_width(c, " -:**-  *scratch*      Top L1     (Lisp Interaction)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: ml,
        pixel_bounds: Rect::new(0.0, text_rows as f32 * char_h, pixel_w, char_h),
        selected: true,
    });
    let mini = build_minibuffer(
        c,
        "For information about GNU Emacs and the GNU system, type C-h C-a.",
    );
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32 * char_h, pixel_w, char_h),
        selected: false,
    });
    state
}

fn build_hsplit(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    _pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameDisplayState {
    let c = cols as usize;
    let r = rows as usize;
    let half = (r - 1) / 2;
    let top_text = half - 1;
    let bot_text = r - 1 - half - 1;
    let mut state = new_state(
        cols,
        rows,
        char_w,
        char_h,
        pixel_w,
        r as f32 * char_h,
        faces,
    );

    let scratch = scratch_buffer_lines();
    let top = build_text_matrix(top_text, c, &scratch, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: top,
        pixel_bounds: Rect::new(0.0, 0.0, pixel_w, top_text as f32 * char_h),
        selected: true,
    });
    let top_ml = build_mode_line_width(c, " -:**-  *scratch*      Top L1     (Lisp Interaction)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: top_ml,
        pixel_bounds: Rect::new(0.0, top_text as f32 * char_h, pixel_w, char_h),
        selected: true,
    });

    let messages = messages_buffer_lines();
    let bot_y = half as f32 * char_h;
    let bot = build_text_matrix(bot_text, c, &messages, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: bot,
        pixel_bounds: Rect::new(0.0, bot_y, pixel_w, bot_text as f32 * char_h),
        selected: true,
    });
    let bot_ml = build_mode_line_width(c, " -:---  *Messages*     Bot L1     (Messages)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 11,
        matrix: bot_ml,
        pixel_bounds: Rect::new(0.0, bot_y + bot_text as f32 * char_h, pixel_w, char_h),
        selected: true,
    });

    let mini = build_minibuffer(c, "");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32 * char_h, pixel_w, char_h),
        selected: false,
    });
    state
}

fn build_vsplit(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    _pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameDisplayState {
    let c = cols as usize;
    let r = rows as usize;
    let left_cols = c / 2;
    let right_cols = c - left_cols - 1;
    let text_rows = r - 2;
    let mut state = new_state(
        cols,
        rows,
        char_w,
        char_h,
        pixel_w,
        r as f32 * char_h,
        faces,
    );

    let scratch = scratch_buffer_lines();
    let left = build_text_matrix(text_rows, left_cols, &scratch, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: left,
        pixel_bounds: Rect::new(
            0.0,
            0.0,
            left_cols as f32 * char_w,
            text_rows as f32 * char_h,
        ),
        selected: true,
    });

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
        selected: true,
    });

    let help = help_buffer_lines();
    let right = build_text_matrix(text_rows, right_cols, &help, 0, false);
    let rx = (left_cols + 1) as f32 * char_w;
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: right,
        pixel_bounds: Rect::new(
            rx,
            0.0,
            right_cols as f32 * char_w,
            text_rows as f32 * char_h,
        ),
        selected: true,
    });

    let ml_left = format!(
        " -:**-  *scratch*{:>w$}",
        "",
        w = left_cols.saturating_sub(17),
    );
    let ml_right = format!(
        " -:---  help.el{:>w$}",
        "",
        w = right_cols.saturating_sub(15),
    );
    let ml = build_mode_line_width(c, &format!("{}|{}", ml_left, ml_right));
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: ml,
        pixel_bounds: Rect::new(0.0, text_rows as f32 * char_h, pixel_w, char_h),
        selected: true,
    });

    let mini = build_minibuffer(c, "C-x 3 ran the command split-window-right");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32 * char_h, pixel_w, char_h),
        selected: false,
    });
    state
}

fn build_triple(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    _pixel_h: f32,
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
    let mut state = new_state(
        cols,
        rows,
        char_w,
        char_h,
        pixel_w,
        r as f32 * char_h,
        faces,
    );

    // Left: *scratch*
    let scratch = scratch_buffer_lines();
    let left = build_text_matrix(left_text, left_cols, &scratch, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: left,
        pixel_bounds: Rect::new(
            0.0,
            0.0,
            left_cols as f32 * char_w,
            left_text as f32 * char_h,
        ),
        selected: true,
    });
    let left_ml = build_mode_line_width(left_cols, " -:**-  *scratch*      (Lisp Interaction)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: left_ml,
        pixel_bounds: Rect::new(
            0.0,
            left_text as f32 * char_h,
            left_cols as f32 * char_w,
            char_h,
        ),
        selected: true,
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
        pixel_bounds: Rect::new(
            left_cols as f32 * char_w,
            0.0,
            char_w,
            (r - 1) as f32 * char_h,
        ),
        selected: false,
    });

    let rx = (left_cols + 1) as f32 * char_w;

    // Top-right: *Messages*
    let messages = messages_buffer_lines();
    let tr = build_text_matrix(top_right_text, right_cols, &messages, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: tr,
        pixel_bounds: Rect::new(
            rx,
            0.0,
            right_cols as f32 * char_w,
            top_right_text as f32 * char_h,
        ),
        selected: true,
    });
    let tr_ml = build_mode_line_width(right_cols, " -:---  *Messages*     (Messages)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 11,
        matrix: tr_ml,
        pixel_bounds: Rect::new(
            rx,
            top_right_text as f32 * char_h,
            right_cols as f32 * char_w,
            char_h,
        ),
        selected: true,
    });

    // Bottom-right: *Help*
    let help = help_buffer_lines();
    let br_y = right_half as f32 * char_h;
    let br = build_text_matrix(bot_right_text, right_cols, &help, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 3,
        matrix: br,
        pixel_bounds: Rect::new(
            rx,
            br_y,
            right_cols as f32 * char_w,
            bot_right_text as f32 * char_h,
        ),
        selected: true,
    });
    let br_ml = build_mode_line_width(right_cols, " -:---  *Help*         (Help)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 12,
        matrix: br_ml,
        pixel_bounds: Rect::new(
            rx,
            br_y + bot_right_text as f32 * char_h,
            right_cols as f32 * char_w,
            char_h,
        ),
        selected: true,
    });

    // Minibuffer
    let mini = build_minibuffer(c, "");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32 * char_h, pixel_w, char_h),
        selected: false,
    });
    state
}

fn build_default(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    _pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> Vec<FrameDisplayState> {
    let c = cols as usize;
    let r = rows as usize;
    let top_half = (r - 1) / 2;
    let bot_text = r - 1 - top_half - 1;
    let left_cols = c / 2;
    let right_cols = c - left_cols - 1;
    let top_text = top_half - 1;
    let mut state = new_state(cols, rows, char_w, char_h, pixel_w, r as f32 * char_h, faces);

    // --- Top-left: *scratch* with rounded-box face (face 8) ---
    let scratch_lines: Vec<(&str, u32)> = scratch_buffer_lines()
        .into_iter()
        .map(|(line, _)| (line, 8u32))
        .collect();
    let tl = build_text_matrix(top_text, left_cols, &scratch_lines, 0, true);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 1,
        matrix: tl,
        pixel_bounds: Rect::new(0.0, 0.0, left_cols as f32 * char_w, top_text as f32 * char_h),
        selected: true,
    });
    let tl_ml = build_mode_line_width(left_cols, " -:**-  *scratch*      (Lisp Interaction)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 10,
        matrix: tl_ml,
        pixel_bounds: Rect::new(0.0, top_text as f32 * char_h, left_cols as f32 * char_w, char_h),
        selected: true,
    });

    // Vertical divider between left and right
    let mut vdiv = GlyphMatrix::new(top_half, 1);
    for row in &mut vdiv.rows {
        row.enabled = true;
        row.glyphs[GlyphArea::Text as usize].push(Glyph::char('|', 7, 0));
    }
    vdiv.ensure_hashes();
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 30,
        matrix: vdiv,
        pixel_bounds: Rect::new(
            left_cols as f32 * char_w, 0.0,
            char_w, top_half as f32 * char_h,
        ),
        selected: false,
    });

    // --- Top-right: *Messages* buffer (child-frame goes on top later) ---
    let rx = (left_cols + 1) as f32 * char_w;
    let messages = messages_buffer_lines();
    let tr = build_text_matrix(top_text, right_cols, &messages, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 2,
        matrix: tr,
        pixel_bounds: Rect::new(
            rx, 0.0,
            right_cols as f32 * char_w, top_text as f32 * char_h,
        ),
        selected: false,
    });
    let tr_ml = build_mode_line_width(right_cols, " -:---  *Messages*     (Messages)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 11,
        matrix: tr_ml,
        pixel_bounds: Rect::new(
            rx, top_text as f32 * char_h,
            right_cols as f32 * char_w, char_h,
        ),
        selected: true,
    });

    // Horizontal divider (full width mode-line between top and bottom halves)
    let top_ml_y = top_half as f32 * char_h;
    let mut hdiv = GlyphMatrix::new(1, c);
    hdiv.rows[0].enabled = true;
    for i in 0..c {
        hdiv.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char('-', 1, 0));
    }
    hdiv.ensure_hashes();
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 31,
        matrix: hdiv,
        pixel_bounds: Rect::new(0.0, top_ml_y, pixel_w, char_h),
        selected: false,
    });

    // --- Bottom: *Help* buffer ---
    let help = help_buffer_lines();
    let bot_y = (top_half + 1) as f32 * char_h;
    let bot = build_text_matrix(bot_text, c, &help, 0, false);
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 3,
        matrix: bot,
        pixel_bounds: Rect::new(0.0, bot_y, pixel_w, bot_text as f32 * char_h),
        selected: false,
    });
    let bot_ml = build_mode_line_width(c, " -:---  *Help*         (Help)");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 12,
        matrix: bot_ml,
        pixel_bounds: Rect::new(0.0, (r - 2) as f32 * char_h, pixel_w, char_h),
        selected: true,
    });

    // Minibuffer
    let mini = build_minibuffer(c, "C-x 2 C-x 3 — split-window-below + split-window-right");
    state.window_matrices.push(WindowMatrixEntry {
        window_id: 20,
        matrix: mini,
        pixel_bounds: Rect::new(0.0, (r - 1) as f32 * char_h, pixel_w, char_h),
        selected: false,
    });
    state.frame_id = 1;

    // --- Child frame: completion popup floating over top-right window ---
    let cf_cols = right_cols.saturating_sub(4);
    let cf_pixel_w = cf_cols as f32 * char_w;
    let cf_pixel_x = rx + 2.0 * char_w;
    let cf_rows = (top_text - 3).min(8);
    let cf_pixel_h = (cf_rows as f32 + 2.0) * char_h; // border + title + items
    let cf_pixel_y = 0.0;

    let mut cf = FrameDisplayState::new(
        cf_cols, cf_rows as usize + 2,
        char_w, char_h,
    );
    cf.frame_id = 100;
    cf.parent_id = 1;
    cf.parent_x = cf_pixel_x;
    cf.parent_y = cf_pixel_y;
    cf.z_order = 1;
    cf.undecorated = true;
    cf.faces = faces.clone();
    cf.frame_pixel_width = cf_pixel_w;
    cf.frame_pixel_height = cf_pixel_h;
    cf.background = Color::new(0.0, 0.0, 0.0, 0.0); // transparent

    // Border row (face 9)
    let mut border = GlyphMatrix::new(1, cf_cols);
    border.rows[0].enabled = true;
    for _ in 0..cf_cols { border.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char(' ', 9, 0)); }
    border.ensure_hashes();
    cf.window_matrices.push(WindowMatrixEntry { window_id: 1, matrix: border,
        pixel_bounds: Rect::new(0.0, 0.0, cf_pixel_w, char_h), selected: false });

    // Title row (face 11: navy bg)
    let mut title = GlyphMatrix::new(1, cf_cols);
    title.rows[0].enabled = true;
    for ch in format!(" {:-<w$}", "Completions ", w = cf_cols - 1).chars() {
        title.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, 11, 0));
    }
    title.ensure_hashes();
    cf.window_matrices.push(WindowMatrixEntry { window_id: 2, matrix: title,
        pixel_bounds: Rect::new(0.0, 1.0 * char_h, cf_pixel_w, char_h), selected: false });

    let items: &[(&str, u32)] = &[
        ("  describe-function     ", 9),
        ("  describe-variable     ", 9),
        ("▸ describe-symbol        ", 10),
        ("  describe-key          ", 9),
        ("  describe-mode         ", 9),
        ("  describe-char         ", 9),
    ];
    for (row_i, (label, face_id)) in items.iter().enumerate() {
        let mut row = GlyphMatrix::new(1, cf_cols);
        row.rows[0].enabled = true;
        for ch in label.chars().take(cf_cols) {
            row.rows[0].glyphs[GlyphArea::Text as usize].push(Glyph::char(ch, *face_id, 0));
        }
        row.ensure_hashes();
        cf.window_matrices.push(WindowMatrixEntry {
            window_id: (3 + row_i) as u64, matrix: row,
            pixel_bounds: Rect::new(0.0, (2 + row_i) as f32 * char_h, cf_pixel_w, char_h),
            selected: false,
        });
    }

    vec![state, cf]
}

// -------------------------------------------------------------------
// GlyphMatrix helpers
// -------------------------------------------------------------------

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
        let lnum = format!("{:>3} ", row_idx + 1);
        for ch in lnum.chars() {
            row.glyphs[GlyphArea::LeftMargin as usize].push(Glyph::char(ch, 2, 0));
        }
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

// -------------------------------------------------------------------
// Face + state helpers
// -------------------------------------------------------------------

fn build_faces() -> HashMap<u32, Face> {
    use neomacs_display_protocol::gradient::{ColorStop, Gradient};

    let mut f = HashMap::new();
    f.insert(0, mk(0, 0.87, 0.87, 0.87, 0.0, 0.0, 0.0, 400, false, None));

    // Face 1: Mode-line with noise gradient (high contrast pink)
    let mode_line_gradient = Some(Box::new(Gradient::Noise {
        scale: 4.0,
        octaves: 4,
        color1: Color::new(1.0, 0.42, 0.62, 1.0), // #FF6B9D hot pink
        color2: Color::new(1.0, 0.95, 0.97, 1.0), // #FFF2F7 near-white pink
    }));
    f.insert(
        1,
        mk(
            1,
            0.0,
            0.0,
            0.0, // black foreground on gradient
            0.0,
            0.0,
            0.0,
            700,
            false,
            mode_line_gradient,
        ),
    );

    f.insert(2, mk(2, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 400, false, None));

    // Face 3: Comments with radial gradient (bright center, dark edges)
    let comment_gradient = Some(Box::new(Gradient::Radial {
        center_x: 0.5,
        center_y: 0.5,
        radius: 0.8,
        stops: vec![
            ColorStop::new(0.0, Color::new(1.0, 1.0, 1.0, 1.0)), // White center
            ColorStop::new(1.0, Color::new(0.0, 0.2, 0.4, 1.0)), // Dark blue edge
        ],
    }));
    f.insert(
        3,
        mk(
            3,
            1.0,
            0.6,
            0.2,
            0.0,
            0.0,
            0.0,
            700,
            false,
            comment_gradient,
        ),
    );

    // Face 4: Strings with conic gradient (rainbow spinner)
    let string_gradient = Some(Box::new(Gradient::Conic {
        center_x: 0.5,
        center_y: 0.5,
        angle_offset: 0.0,
        stops: vec![
            ColorStop::new(0.00, Color::new(1.0, 0.0, 0.0, 1.0)), // Red
            ColorStop::new(0.17, Color::new(1.0, 0.5, 0.0, 1.0)), // Orange
            ColorStop::new(0.33, Color::new(1.0, 1.0, 0.0, 1.0)), // Yellow
            ColorStop::new(0.50, Color::new(0.0, 1.0, 0.0, 1.0)), // Green
            ColorStop::new(0.67, Color::new(0.0, 0.0, 1.0, 1.0)), // Blue
            ColorStop::new(0.83, Color::new(0.3, 0.0, 0.5, 1.0)), // Indigo
            ColorStop::new(1.00, Color::new(1.0, 0.0, 0.0, 1.0)), // Red (wrap)
        ],
    }));
    f.insert(
        4,
        mk(4, 0.4, 0.9, 0.4, 0.0, 0.0, 0.0, 400, false, string_gradient),
    );

    f.insert(5, mk(5, 0.4, 0.7, 0.7, 0.0, 0.0, 0.0, 400, true, None));
    f.insert(
        6,
        mk(6, 0.87, 0.87, 0.87, 0.15, 0.15, 0.15, 400, false, None),
    );
    f.insert(7, mk(7, 0.4, 0.4, 0.4, 0.0, 0.0, 0.0, 400, false, None));

    // Face 8: Rounded box border face (for top-left window text)
    {
        let mut box_face = Face::new(8);
        box_face.foreground = Color::new(0.87, 0.87, 0.87, 1.0);
        box_face.background = Color::new(0.05, 0.05, 0.08, 1.0);
        box_face.font_weight = 400;
        box_face.box_type = neomacs_display_protocol::face::BoxType::Line;
        box_face.box_line_width = 2;
        box_face.box_corner_radius = 8;
        box_face.box_color = Some(Color::new(0.2, 0.8, 0.4, 1.0)); // green border
        box_face.box_border_style = 1; // gradient style
        box_face.box_border_speed = 0.5;
        f.insert(8, box_face);
    }

    // Face 9: Child-frame text (white on dark blue-gray)
    f.insert(9, mk(9, 0.9, 0.9, 0.95, 0.08, 0.08, 0.14, 400, false, None));

    // Face 10: Child-frame selected item (white on highlight blue)
    f.insert(10, mk(10, 0.9, 0.9, 0.95, 0.18, 0.22, 0.38, 400, false, None));

    // Face 11: Child-frame title (white on navy)
    f.insert(11, mk(11, 0.9, 0.9, 0.95, 0.15, 0.20, 0.35, 400, false, None));

    f
}

fn mk(
    id: u32,
    fr: f32,
    fg: f32,
    fb: f32,
    br: f32,
    bg: f32,
    bb: f32,
    weight: u16,
    italic: bool,
    gradient: Option<Box<neomacs_display_protocol::gradient::Gradient>>,
) -> Face {
    let mut attrs = FaceAttributes::empty();
    if italic {
        attrs |= FaceAttributes::ITALIC;
    }
    let mut face = Face::new(id);
    face.foreground = Color::new(fr, fg, fb, 1.0);
    face.background = Color::new(br, bg, bb, 1.0);
    face.font_weight = weight;
    face.attributes = attrs;
    face.background_gradient = gradient;
    face
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
    let mut s = FrameDisplayState::new(cols as usize, rows as usize, char_w, char_h);
    s.frame_pixel_width = pixel_w;
    s.frame_pixel_height = pixel_h;
    s.background = Color::new(0.0, 0.0, 0.0, 1.0);
    s.faces = faces.clone();
    s
}

// -------------------------------------------------------------------
// Terminal helpers
// -------------------------------------------------------------------

#[cfg(unix)]
fn setup_terminal() {
    use std::os::unix::io::AsRawFd;
    unsafe {
        let mut raw = std::mem::zeroed::<libc::termios>();
        libc::tcgetattr(io::stdin().as_raw_fd(), &mut raw);
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(io::stdin().as_raw_fd(), libc::TCSANOW, &raw);
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
        let mut w = std::mem::MaybeUninit::<libc::winsize>::uninit();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, w.as_mut_ptr()) == 0 {
            let w = w.assume_init();
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

/// Read Xft.dpi from xrdb and compute scale factor (96 dpi = 1.0x).
fn detect_dpi_scale() -> f32 {
    if let Ok(output) = std::process::Command::new("xrdb").arg("-query").output() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.starts_with("Xft.dpi:") {
                if let Some(val) = line.split_whitespace().nth(1) {
                    if let Ok(dpi) = val.parse::<f32>() {
                        return (dpi / 96.0).max(1.0);
                    }
                }
            }
        }
    }
    1.0
}
