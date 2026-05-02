//! Mock display test — renders fake Emacs frames via TTY or GUI.
//!
//! Usage:
//!   mock-display [OPTIONS] [DEMO]
//!
//! DEMO: default, single, hsplit, vsplit, triple, all
//!
//! OPTIONS:
//!   --gui       Render via wgpu GPU window instead of TTY
//!   --dump      Dump grid as plain text (no terminal setup)

use neomacs_display_protocol::face::{Face, FaceAttributes};
use neomacs_display_protocol::frame_content::{
    ChildFrameContent, FrameContent, StyledLine, WindowContent,
};
use neomacs_display_protocol::glyph_matrix::*;
use neomacs_display_protocol::tty_rif::TtyRif;
use neomacs_display_protocol::types::{Color, Rect};
use neomacs_layout_engine::engine::LayoutEngine;
use std::collections::HashMap;
use std::io::{self, Read, Write};

// ===================================================================
// Scene: Vec<FrameDisplayState> with GUI/TTY fan-out helpers
// ===================================================================

#[derive(Clone)]
struct Scene(Vec<FrameDisplayState>);

impl Scene {
    fn iter(&self) -> impl Iterator<Item = &FrameDisplayState> {
        self.0.iter()
    }
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
// Dump mode
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

// ===================================================================
// Scene utilities
// ===================================================================

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
    let content = match name {
        "default" => build_default(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces),
        "hsplit" => build_hsplit(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces),
        "vsplit" => build_vsplit(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces),
        "triple" => build_triple(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces),
        _ => build_single(cols, rows, char_w, char_h, pixel_w, pixel_h, &faces),
    };
    let mut engine = LayoutEngine::new();
    engine.enable_cosmic_metrics();
    let states = engine.layout_frame_content(&content);
    Scene(states)
}

// ===================================================================
// Buffer content
// ===================================================================

fn scratch_buffer_lines() -> Vec<(&'static str, u32)> {
    vec![
        (";; This is the *scratch* buffer.", 5),
        ("", 0),
        ("(defun hello (name)", 3),
        ("  \"Say hello to NAME.\"", 4),
        ("  (message \"Hello, %s!\" name))", 3),
        ("", 0),
        (";; Type C-x C-e to evaluate", 2),
        ("", 0),
        ("(setq neomacs-version \"0.1.0\")", 0),
        ("(setq display-pipeline 'glyph-matrix)", 0),
        ("", 0),
        (";; GNU Emacs compatible glyph matrix model", 2),
        (";; TTY rendering via TtyRif", 2),
        (";; Single-thread, no channel, matching GNU", 2),
        ("", 0),
        ("", 0),
        ("", 0),
        ("", 0),
        ("", 0),
        ("", 0),
        ("", 0),
        ("", 0),
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
        ("================", 3),
        ("", 0),
        ("  Emacs is the extensible,", 0),
        ("  customizable, self-documenting", 0),
        ("  real-time display editor.", 0),
        ("", 0),
        (";; Key Bindings:", 2),
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

// ===================================================================
// Layout builders — produce FrameContent (the evaluator handoff)
// ===================================================================

fn build_single(
    _cols: u16,
    rows: u16,
    _char_w: f32,
    char_h: f32,
    pixel_w: f32,
    pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameContent {
    let r = rows as usize;
    let text_rows = r - 2;
    let scratch: Vec<StyledLine> = scratch_buffer_lines()
        .into_iter()
        .map(|(t, f)| StyledLine::from_str(t, f))
        .collect();
    FrameContent {
        frame_id: 1,
        faces: faces.values().cloned().collect(),
        windows: vec![WindowContent {
            window_id: 1,
            lines: scratch,
            mode_line_text: " -:**-  *scratch*      Top L1     (Lisp Interaction)".into(),
            pixel_bounds: Rect::new(0.0, 0.0, pixel_w, text_rows as f32 * char_h),
            selected: true,
            truncated_lines: false,
        }],
        child_frames: vec![],
        frame_pixel_width: pixel_w,
        frame_pixel_height: pixel_h,
        background: Color::new(0.0, 0.0, 0.0, 1.0),
        menu_bar: None,
    }
}

fn build_hsplit(
    _cols: u16,
    rows: u16,
    _char_w: f32,
    char_h: f32,
    pixel_w: f32,
    _pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameContent {
    let r = rows as usize;
    let half = (r - 1) / 2;
    let scratch: Vec<StyledLine> = scratch_buffer_lines()
        .into_iter()
        .map(|(t, f)| StyledLine::from_str(t, f))
        .collect();
    let messages: Vec<StyledLine> = messages_buffer_lines()
        .into_iter()
        .map(|(t, f)| StyledLine::from_str(t, f))
        .collect();
    FrameContent {
        frame_id: 1,
        faces: faces.values().cloned().collect(),
        windows: vec![
            WindowContent {
                window_id: 1,
                lines: scratch,
                mode_line_text: " -:**-  *scratch*      Top L1     (Lisp Interaction)".into(),
                pixel_bounds: Rect::new(0., 0., pixel_w, (half - 1) as f32 * char_h),
                selected: true,
                truncated_lines: false,
            },
            WindowContent {
                window_id: 2,
                lines: messages,
                mode_line_text: " -:---  *Messages*     Bot L1     (Messages)".into(),
                pixel_bounds: Rect::new(
                    0.,
                    half as f32 * char_h,
                    pixel_w,
                    (r - 1 - half - 1) as f32 * char_h,
                ),
                selected: true,
                truncated_lines: false,
            },
        ],
        child_frames: vec![],
        frame_pixel_width: pixel_w,
        frame_pixel_height: r as f32 * char_h,
        background: Color::new(0.0, 0.0, 0.0, 1.0),
        menu_bar: None,
    }
}

fn build_vsplit(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    _pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameContent {
    let c = cols as usize;
    let r = rows as usize;
    let left_cols = c / 2;
    let right_cols = c - left_cols - 1;
    let text_rows = r - 2;
    let scratch: Vec<StyledLine> = scratch_buffer_lines()
        .into_iter()
        .map(|(t, f)| StyledLine::from_str(t, f))
        .collect();
    let help: Vec<StyledLine> = help_buffer_lines()
        .into_iter()
        .map(|(t, f)| StyledLine::from_str(t, f))
        .collect();
    let ml_left = format!(
        " -:**-  *scratch*{:>w$}",
        "",
        w = left_cols.saturating_sub(17)
    );
    let ml_right = format!(
        " -:---  help.el{:>w$}",
        "",
        w = right_cols.saturating_sub(15)
    );
    FrameContent {
        frame_id: 1,
        faces: faces.values().cloned().collect(),
        windows: vec![
            WindowContent {
                window_id: 1,
                lines: scratch,
                mode_line_text: format!("{}|{}", ml_left, ml_right),
                pixel_bounds: Rect::new(
                    0.,
                    0.,
                    left_cols as f32 * char_w,
                    text_rows as f32 * char_h,
                ),
                selected: true,
                truncated_lines: false,
            },
            WindowContent {
                window_id: 2,
                lines: help,
                mode_line_text: String::new(),
                pixel_bounds: Rect::new(
                    (left_cols + 1) as f32 * char_w,
                    0.,
                    right_cols as f32 * char_w,
                    text_rows as f32 * char_h,
                ),
                selected: true,
                truncated_lines: false,
            },
        ],
        child_frames: vec![],
        frame_pixel_width: pixel_w,
        frame_pixel_height: r as f32 * char_h,
        background: Color::new(0.0, 0.0, 0.0, 1.0),
        menu_bar: None,
    }
}

fn build_triple(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    _pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameContent {
    let c = cols as usize;
    let r = rows as usize;
    let left_cols = c / 2;
    let right_cols = c - left_cols - 1;
    let right_half = (r - 1) / 2;
    let rx = (left_cols + 1) as f32 * char_w;
    let scratch: Vec<StyledLine> = scratch_buffer_lines()
        .into_iter()
        .map(|(t, f)| StyledLine::from_str(t, f))
        .collect();
    let messages: Vec<StyledLine> = messages_buffer_lines()
        .into_iter()
        .map(|(t, f)| StyledLine::from_str(t, f))
        .collect();
    let help: Vec<StyledLine> = help_buffer_lines()
        .into_iter()
        .map(|(t, f)| StyledLine::from_str(t, f))
        .collect();
    FrameContent {
        frame_id: 1,
        faces: faces.values().cloned().collect(),
        windows: vec![
            WindowContent {
                window_id: 1,
                lines: scratch,
                mode_line_text: " -:**-  *scratch*      (Lisp Interaction)".into(),
                pixel_bounds: Rect::new(0., 0., left_cols as f32 * char_w, (r - 2) as f32 * char_h),
                selected: true,
                truncated_lines: false,
            },
            WindowContent {
                window_id: 2,
                lines: messages,
                mode_line_text: " -:---  *Messages*     (Messages)".into(),
                pixel_bounds: Rect::new(
                    rx,
                    0.,
                    right_cols as f32 * char_w,
                    (right_half - 1) as f32 * char_h,
                ),
                selected: true,
                truncated_lines: false,
            },
            WindowContent {
                window_id: 3,
                lines: help,
                mode_line_text: " -:---  *Help*         (Help)".into(),
                pixel_bounds: Rect::new(
                    rx,
                    right_half as f32 * char_h,
                    right_cols as f32 * char_w,
                    (r - 1 - right_half - 1) as f32 * char_h,
                ),
                selected: true,
                truncated_lines: false,
            },
        ],
        child_frames: vec![],
        frame_pixel_width: pixel_w,
        frame_pixel_height: r as f32 * char_h,
        background: Color::new(0.0, 0.0, 0.0, 1.0),
        menu_bar: None,
    }
}

fn build_default(
    cols: u16,
    rows: u16,
    char_w: f32,
    char_h: f32,
    pixel_w: f32,
    _pixel_h: f32,
    faces: &HashMap<u32, Face>,
) -> FrameContent {
    let c = cols as usize;
    let r = rows as usize;
    let top_half = (r - 1) / 2;
    let bot_text = r - 1 - top_half;
    let left_cols = c / 2;
    let right_cols = c - left_cols - 1;
    let top_text = top_half - 1;
    let rx = (left_cols + 1) as f32 * char_w;

    let scratch: Vec<StyledLine> = scratch_buffer_lines()
        .into_iter()
        .map(|(t, _)| StyledLine::from_str(t, 8))
        .collect();
    let messages: Vec<StyledLine> = messages_buffer_lines()
        .into_iter()
        .map(|(t, f)| StyledLine::from_str(t, f))
        .collect();
    let help: Vec<StyledLine> = help_buffer_lines()
        .into_iter()
        .map(|(t, f)| StyledLine::from_str(t, f))
        .collect();

    // Child-frame: 60% of top-right window, centered
    let cf_cols = ((right_cols as f32 * 0.6) as usize).max(20);
    let cf_w = cf_cols as f32 * char_w;
    let cf_x = rx + (right_cols as f32 - cf_cols as f32) * 0.5 * char_w;
    let cf_rows = ((top_text as f32 * 0.6) as usize).max(6);
    let cf_h = (cf_rows as f32 + 2.0) * char_h;
    let cf_y = (top_text as f32 - (cf_rows as f32 + 2.0)) * 0.5 * char_h;
    let title_str = format!(" {:-<w$}", "Completions ", w = cf_cols.saturating_sub(1));
    let mut cf_lines = vec![StyledLine::from_str(&" ".repeat(cf_cols), 9)];
    cf_lines.push(StyledLine::from_str(&title_str, 11));
    let items = [
        "  describe-function     ",
        "  describe-variable     ",
        "\u{25b8} describe-symbol        ",
        "  describe-key          ",
        "  describe-mode         ",
        "  describe-char         ",
        "  describe-face         ",
        "  describe-coding-system",
        "  describe-bindings     ",
        "  describe-package      ",
    ];
    for (i, item) in items.iter().enumerate() {
        cf_lines.push(StyledLine::from_str(item, if i == 2 { 10 } else { 9 }));
    }

    FrameContent {
        frame_id: 1,
        faces: faces.values().cloned().collect(),
        windows: vec![
            WindowContent {
                window_id: 1,
                lines: scratch,
                mode_line_text: " -:**-  *scratch*      (Lisp Interaction)".into(),
                pixel_bounds: Rect::new(
                    0.,
                    0.,
                    left_cols as f32 * char_w,
                    top_text as f32 * char_h,
                ),
                selected: true,
                truncated_lines: false,
            },
            WindowContent {
                window_id: 2,
                lines: messages,
                mode_line_text: " -:---  *Messages*     (Messages)".into(),
                pixel_bounds: Rect::new(
                    rx,
                    0.,
                    right_cols as f32 * char_w,
                    top_text as f32 * char_h,
                ),
                selected: false,
                truncated_lines: false,
            },
            WindowContent {
                window_id: 3,
                lines: help,
                mode_line_text: " -:---  *Help*         (Help)".into(),
                pixel_bounds: Rect::new(
                    0.,
                    top_half as f32 * char_h,
                    pixel_w,
                    bot_text as f32 * char_h,
                ),
                selected: false,
                truncated_lines: false,
            },
        ],
        child_frames: vec![ChildFrameContent {
            frame_id: 100,
            window: WindowContent {
                window_id: 1,
                lines: cf_lines,
                mode_line_text: String::new(),
                pixel_bounds: Rect::new(0., 0., cf_w, cf_h),
                selected: false,
                truncated_lines: false,
            },
            parent_x: cf_x,
            parent_y: cf_y,
            z_order: 1,
        }],
        frame_pixel_width: pixel_w,
        frame_pixel_height: r as f32 * char_h,
        background: Color::new(0.0, 0.0, 0.0, 1.0),
        menu_bar: None,
    }
}

// ===================================================================
// Faces
// ===================================================================

fn build_faces() -> HashMap<u32, Face> {
    use neomacs_display_protocol::gradient::{ColorStop, Gradient};

    let mut f = HashMap::new();
    f.insert(0, mk(0, 0.87, 0.87, 0.87, 0.0, 0.0, 0.0, 400, false, None));

    // Face 1: Mode-line with noise gradient, black foreground
    let mode_line_gradient = Some(Box::new(Gradient::Noise {
        scale: 4.0,
        octaves: 4,
        color1: Color::new(1.0, 0.42, 0.62, 1.0), // #FF6B9D
        color2: Color::new(1.0, 0.95, 0.97, 1.0), // #FFF2F7
    }));
    f.insert(
        1,
        mk(
            1,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            700,
            false,
            mode_line_gradient,
        ),
    );

    f.insert(2, mk(2, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 400, false, None));

    // Face 3: Comments with radial gradient
    let comment_gradient = Some(Box::new(Gradient::Radial {
        center_x: 0.5,
        center_y: 0.5,
        radius: 0.8,
        stops: vec![
            ColorStop::new(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
            ColorStop::new(1.0, Color::new(0.0, 0.2, 0.4, 1.0)),
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

    // Face 4: Strings with conic gradient
    let string_gradient = Some(Box::new(Gradient::Conic {
        center_x: 0.5,
        center_y: 0.5,
        angle_offset: 0.0,
        stops: vec![
            ColorStop::new(0.00, Color::new(1.0, 0.0, 0.0, 1.0)),
            ColorStop::new(0.17, Color::new(1.0, 0.5, 0.0, 1.0)),
            ColorStop::new(0.33, Color::new(1.0, 1.0, 0.0, 1.0)),
            ColorStop::new(0.50, Color::new(0.0, 1.0, 0.0, 1.0)),
            ColorStop::new(0.67, Color::new(0.0, 0.0, 1.0, 1.0)),
            ColorStop::new(0.83, Color::new(0.3, 0.0, 0.5, 1.0)),
            ColorStop::new(1.00, Color::new(1.0, 0.0, 0.0, 1.0)),
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

    // Face 8: Rounded box border
    {
        let mut box_face = Face::new(8);
        box_face.foreground = Color::new(0.87, 0.87, 0.87, 1.0);
        box_face.background = Color::new(0.05, 0.05, 0.08, 1.0);
        box_face.font_weight = 400;
        box_face.box_type = neomacs_display_protocol::face::BoxType::Line;
        box_face.box_line_width = 2;
        box_face.box_corner_radius = 8;
        box_face.box_color = Some(Color::new(0.2, 0.8, 0.4, 1.0));
        box_face.box_border_style = 1;
        box_face.box_border_speed = 0.5;
        f.insert(8, box_face);
    }

    // Faces 9-11: Child-frame backgrounds
    f.insert(9, mk(9, 0.9, 0.9, 0.95, 0.08, 0.08, 0.14, 400, false, None));
    f.insert(
        10,
        mk(10, 0.9, 0.9, 0.95, 0.18, 0.22, 0.38, 400, false, None),
    );
    f.insert(
        11,
        mk(11, 0.9, 0.9, 0.95, 0.15, 0.20, 0.35, 400, false, None),
    );
    f
}

fn mk(
    id: u32,
    fr: f32,
    fg: f32,
    fb: f32,
    br: f32,
    _bg: f32,
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
    face.background = Color::new(br, _bg, bb, 1.0);
    face.font_weight = weight;
    face.attributes = attrs;
    face.background_gradient = gradient;
    face
}

// ===================================================================
// Terminal helpers
// ===================================================================

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
    use std::os::unix::io::AsRawFd;
    unsafe {
        let mut winsz: libc::winsize = std::mem::zeroed();
        if libc::ioctl(io::stdin().as_raw_fd(), libc::TIOCGWINSZ, &mut winsz) == 0 {
            Some((winsz.ws_col, winsz.ws_row))
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
fn query_terminal_size() -> Option<(u16, u16)> {
    None
}

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
