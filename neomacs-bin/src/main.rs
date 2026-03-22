//! Neomacs — standalone Rust binary
//!
//! Uses the neovm-core Elisp evaluator with a GNU Emacs-compatible command
//! loop.  The evaluator's `recursive_edit()` drives the main event loop:
//!
//!   read_char() → key-binding → command-execute → redisplay
//!
//! All editing commands, keybindings, and user customizations come from Elisp
//! (loaded .el files), just like GNU Emacs.  Only the core command loop and
//! low-level primitives are implemented in Rust.

mod input_bridge;
mod tty_frontend;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use neomacs_display_runtime::FrameGlyphBuffer;
use neomacs_display_runtime::render_thread::{
    RenderThread, SharedImageDimensions, SharedMonitorInfo,
};
use neomacs_display_runtime::thread_comm::{
    InputEvent as DisplayInputEvent, RenderCommand, ThreadComms,
};
use neomacs_layout_engine::font_metrics::FontMetricsService;
use neomacs_layout_engine::fontconfig::face_height_to_pixels;

use neovm_core::buffer::BufferId;
use neovm_core::emacs_core::Value;
use neovm_core::emacs_core::display::gui_window_system_symbol;
use neovm_core::emacs_core::error::EvalError;
use neovm_core::emacs_core::eval::{FontResolveRequest, GuiFrameHostSize, ResolvedFontMatch};
use neovm_core::emacs_core::intern::resolve_sym;
use neovm_core::emacs_core::print_value_with_eval;
use neovm_core::emacs_core::terminal::pure::{
    TerminalRuntimeConfig, configure_terminal_runtime, reset_terminal_runtime,
};
use neovm_core::emacs_core::{DisplayHost, Evaluator, GuiFrameHostRequest};
use neovm_core::face::{FaceHeight, FontSlant, FontWeight, FontWidth};
use neovm_core::window::{FrameId, Window};

#[derive(Debug, Clone, PartialEq, Eq)]
enum EarlyCliAction {
    PrintHelp { program: String },
    PrintVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrontendKind {
    Gui,
    Tty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartupOptions {
    frontend: FrontendKind,
    forwarded_args: Vec<String>,
    terminal_device: Option<String>,
    noninteractive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BootstrapDisplayConfig {
    frontend: FrontendKind,
    color_cells: i64,
    background_mode: &'static str,
}

const EARLY_HELP_BODY: &str = concat!(
    "Run Neomacs, the extensible, customizable, self-documenting real-time\n",
    "display editor.  The recommended way to start Neomacs for normal editing\n",
    "is with no options at all.\n",
    "\n",
    "Run M-x info RET m emacs RET m emacs invocation RET inside Emacs to\n",
    "read the main documentation for these command-line arguments.\n",
    "\n",
    "Initialization options:\n",
    "\n",
    "--batch                     do not do interactive display; implies -q\n",
    "--chdir DIR                 change to directory DIR\n",
    "--daemon, --bg-daemon[=NAME] start a (named) server in the background\n",
    "--fg-daemon[=NAME]          start a (named) server in the foreground\n",
    "--debug-init                enable Emacs Lisp debugger for init file\n",
    "--display, -d DISPLAY       use X server DISPLAY\n",
    "--no-build-details          do not add build details such as time stamps\n",
    "--no-desktop                do not load a saved desktop\n",
    "--no-init-file, -q          load neither ~/.emacs nor default.el\n",
    "--no-loadup, -nl            do not load loadup.el into bare Emacs\n",
    "--no-site-file              do not load site-start.el\n",
    "--no-x-resources            do not load X resources\n",
    "--no-site-lisp, -nsl        do not add site-lisp directories to load-path\n",
    "--no-splash                 do not display a splash screen on startup\n",
    "--no-window-system, -nw     do not communicate with X, ignoring $DISPLAY\n",
    "--init-directory=DIR        use DIR when looking for the Emacs init files.\n",
    "--quick, -Q                 equivalent to:\n",
    "                              -q --no-site-file --no-site-lisp --no-splash\n",
    "                              --no-x-resources\n",
    "--script FILE               run FILE as an Emacs Lisp script\n",
    "-x                          to be used in #!/usr/bin/emacs -x\n",
    "                              and has approximately the same meaning\n",
    "                              as -Q --script\n",
    "--terminal, -t DEVICE       use DEVICE for terminal I/O\n",
    "--user, -u USER             load ~USER/.emacs instead of your own\n",
    "\n",
    "Action options:\n",
    "\n",
    "FILE                    visit FILE\n",
    "+LINE                   go to line LINE in next FILE\n",
    "+LINE:COLUMN            go to line LINE, column COLUMN, in next FILE\n",
    "--directory, -L DIR     prepend DIR to load-path (with :DIR, append DIR)\n",
    "--eval EXPR             evaluate Emacs Lisp expression EXPR\n",
    "--execute EXPR          evaluate Emacs Lisp expression EXPR\n",
    "--file FILE             visit FILE\n",
    "--find-file FILE        visit FILE\n",
    "--funcall, -f FUNC      call Emacs Lisp function FUNC with no arguments\n",
    "--insert FILE           insert contents of FILE into current buffer\n",
    "--kill                  exit without asking for confirmation\n",
    "--load, -l FILE         load Emacs Lisp FILE using the load function\n",
    "--visit FILE            visit FILE\n",
    "\n",
    "Display options:\n",
    "\n",
    "--background-color, -bg COLOR   window background color\n",
    "--basic-display, -D             disable many display features;\n",
    "                                  used for debugging Emacs\n",
    "--border-color, -bd COLOR       main border color\n",
    "--border-width, -bw WIDTH       width of main border\n",
    "--cursor-color, -cr COLOR       color of the Emacs cursor indicating point\n",
    "--font, -fn FONT                default font; must be fixed-width\n",
    "--foreground-color, -fg COLOR   window foreground color\n",
    "--fullheight, -fh               make the first frame high as the screen\n",
    "--fullscreen, -fs               make the first frame fullscreen\n",
    "--fullwidth, -fw                make the first frame wide as the screen\n",
    "--maximized, -mm                make the first frame maximized\n",
    "--geometry, -g GEOMETRY         window geometry\n",
    "--iconic                        start Neomacs in iconified state\n",
    "--internal-border, -ib WIDTH    width between text and main border\n",
    "--line-spacing, -lsp PIXELS     additional space to put between lines\n",
    "--mouse-color, -ms COLOR        mouse cursor color in Neomacs window\n",
    "--name NAME                     title for initial Neomacs frame\n",
    "--no-blinking-cursor, -nbc      disable blinking cursor\n",
    "--reverse-video, -r, -rv        switch foreground and background\n",
    "--title, -T TITLE               title for initial Neomacs frame\n",
    "--vertical-scroll-bars, -vb     enable vertical scroll bars\n",
    "--xrm XRESOURCES                set additional X resources\n",
    "--parent-id XID                 set parent window\n",
    "--help                          display this help and exit\n",
    "--version                       output version information and exit\n",
    "\n",
    "You can generally also specify long option names with a single -; for\n",
    "example, -batch as well as --batch.  You can use any unambiguous\n",
    "abbreviation for a --option.\n",
    "\n",
    "Various environment variables and window system resources also affect\n",
    "the operation of Neomacs.  See the main documentation.\n",
    "\n",
    "Report bugs to https://github.com/eval-exec/neomacs-windows/issues.\n",
);

const BOOTSTRAP_CORE_FEATURES: &[&str] = &["neomacs", "x"];

fn classify_early_cli_action(args: impl IntoIterator<Item = String>) -> Option<EarlyCliAction> {
    let mut args = args.into_iter();
    let program = args.next().unwrap_or_else(|| "neomacs".to_string());
    for arg in args {
        if arg == "--" {
            break;
        }
        match arg.as_str() {
            "--help" | "-help" => {
                return Some(EarlyCliAction::PrintHelp { program });
            }
            "--version" | "-version" => {
                return Some(EarlyCliAction::PrintVersion);
            }
            _ => {}
        }
    }
    None
}

fn render_help_text(program: &str) -> String {
    let mut out = String::new();
    let _ = write!(&mut out, "Usage: {program} [OPTION-OR-FILENAME]...\n\n");
    out.push_str(EARLY_HELP_BODY);
    out
}

fn render_version_text() -> String {
    format!(
        "Neomacs {}\nStandalone Rust binary for Neomacs (no C dependency)\n",
        neomacs_display_runtime::VERSION
    )
}

fn parse_startup_options(args: impl IntoIterator<Item = String>) -> Result<StartupOptions, String> {
    let mut iter = args.into_iter();
    let program = iter.next().unwrap_or_else(|| "neomacs".to_string());
    let args = iter.collect::<Vec<_>>();
    let mut forwarded_args = vec![program];
    let mut frontend = FrontendKind::Gui;
    let mut terminal_device = None;
    let mut noninteractive = false;
    let mut index = 0usize;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            forwarded_args.extend(args[index..].iter().cloned());
            break;
        }

        if matches!(arg.as_str(), "-nw" | "--no-window-system" | "--no-windows") {
            frontend = FrontendKind::Tty;
            index += 1;
            continue;
        }

        if matches!(arg.as_str(), "--batch" | "-batch") {
            noninteractive = true;
            frontend = FrontendKind::Tty;
            index += 1;
            continue;
        }

        if arg == "-t" || arg == "--terminal" {
            let Some(device) = args.get(index + 1) else {
                return Err(format!("neomacs: option `{arg}` requires an argument"));
            };
            frontend = FrontendKind::Tty;
            terminal_device = Some(device.clone());
            index += 2;
            continue;
        }

        if let Some(device) = arg.strip_prefix("--terminal=") {
            frontend = FrontendKind::Tty;
            terminal_device = Some(device.to_string());
            index += 1;
            continue;
        }

        if arg == "-d" || arg == "--display" {
            if args.get(index + 1).is_none() {
                return Err(format!("neomacs: option `{arg}` requires an argument"));
            }
            index += 2;
            continue;
        }

        if arg.starts_with("--display=") {
            index += 1;
            continue;
        }

        forwarded_args.push(arg.clone());
        index += 1;
    }

    Ok(StartupOptions {
        frontend,
        forwarded_args,
        terminal_device,
        noninteractive,
    })
}

fn bootstrap_display_config(frontend: FrontendKind) -> BootstrapDisplayConfig {
    match frontend {
        FrontendKind::Gui => BootstrapDisplayConfig {
            frontend,
            color_cells: 16777216,
            background_mode: "light",
        },
        FrontendKind::Tty => BootstrapDisplayConfig {
            frontend,
            color_cells: detect_tty_color_cells(),
            background_mode: detect_tty_background_mode(),
        },
    }
}

impl BootstrapDisplayConfig {
    fn window_system_symbol(self) -> Option<&'static str> {
        match self.frontend {
            FrontendKind::Gui => Some(gui_window_system_symbol()),
            FrontendKind::Tty => None,
        }
    }

    fn display_type_symbol(self) -> &'static str {
        if self.color_cells > 0 {
            "color"
        } else {
            "mono"
        }
    }
}

fn detect_tty_runtime() -> TerminalRuntimeConfig {
    let tty_type = std::env::var("TERM").ok().filter(|value| !value.is_empty());
    TerminalRuntimeConfig::interactive(tty_type, detect_tty_color_cells())
}

fn detect_tty_color_cells() -> i64 {
    let colorterm = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return 16777216;
    }

    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if term.is_empty() || term == "dumb" {
        return 0;
    }
    if term.contains("256color") {
        return 256;
    }
    8
}

fn detect_tty_background_mode() -> &'static str {
    let Some(colorfgbg) = std::env::var("COLORFGBG").ok() else {
        return "dark";
    };
    let Some(background) = colorfgbg
        .split(';')
        .next_back()
        .and_then(|value| value.parse::<i32>().ok())
    else {
        return "dark";
    };

    if (7..=15).contains(&background) {
        "light"
    } else {
        "dark"
    }
}

fn startup_dimensions(frontend: FrontendKind, frame_metrics: BootstrapFrameMetrics) -> (u32, u32) {
    match frontend {
        FrontendKind::Gui => (960, 640),
        FrontendKind::Tty => {
            let (cols, rows) = query_terminal_size_cells().unwrap_or((80, 25));
            let width = (cols as f32 * frame_metrics.char_width)
                .round()
                .max(frame_metrics.char_width) as u32;
            let height = (rows as f32 * frame_metrics.char_height)
                .round()
                .max(frame_metrics.char_height * 2.0) as u32;
            (width, height)
        }
    }
}

#[cfg(unix)]
fn query_terminal_size_cells() -> Option<(u16, u16)> {
    use std::mem::MaybeUninit;

    unsafe {
        let mut winsize = MaybeUninit::<libc::winsize>::uninit();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, winsize.as_mut_ptr()) == 0 {
            let winsize = winsize.assume_init();
            if winsize.ws_col > 0 && winsize.ws_row > 0 {
                return Some((winsize.ws_col, winsize.ws_row));
            }
        }
    }
    None
}

#[cfg(not(unix))]
fn query_terminal_size_cells() -> Option<(u16, u16)> {
    None
}

enum FrontendHandle {
    Gui(RenderThread),
    Tty(tty_frontend::TtyFrontend),
}

impl FrontendHandle {
    fn join(self) {
        match self {
            Self::Gui(handle) => handle.join(),
            Self::Tty(handle) => handle.join(),
        }
    }
}

struct PrimaryWindowDisplayHost {
    cmd_tx: crossbeam_channel::Sender<RenderCommand>,
    primary_window_adopted: bool,
    primary_frame_id: Option<neovm_core::window::FrameId>,
    font_metrics: Option<FontMetricsService>,
    primary_window_size: SharedPrimaryWindowSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrimaryWindowSize {
    width: u32,
    height: u32,
}

type SharedPrimaryWindowSize = Arc<Mutex<PrimaryWindowSize>>;

fn record_primary_window_resize(shared: &SharedPrimaryWindowSize, event: &DisplayInputEvent) {
    let DisplayInputEvent::WindowResize {
        width,
        height,
        emacs_frame_id,
    } = event
    else {
        return;
    };

    if *emacs_frame_id != 0 || *width == 0 || *height == 0 {
        return;
    }

    match shared.lock() {
        Ok(mut state) => {
            state.width = *width;
            state.height = *height;
        }
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            state.width = *width;
            state.height = *height;
        }
    }
}

impl DisplayHost for PrimaryWindowDisplayHost {
    fn realize_gui_frame(&mut self, request: GuiFrameHostRequest) -> Result<(), String> {
        tracing::debug!(
            "PrimaryWindowDisplayHost::realize_gui_frame fid=0x{:x} adopted={} size={}x{} title={}",
            request.frame_id.0,
            self.primary_window_adopted,
            request.width,
            request.height,
            request.title
        );
        if !self.primary_window_adopted {
            self.cmd_tx
                .send(RenderCommand::SetWindowTitle {
                    title: request.title,
                })
                .map_err(|err| format!("failed to update primary window title: {err}"))?;
            // The opening GUI frame adopts the already-existing primary host
            // window. Do not push stale Lisp bootstrap dimensions back into
            // that window during adoption; host resize events remain the
            // source of truth until the window is fully realized.
            self.primary_window_adopted = true;
            self.primary_frame_id = Some(request.frame_id);
        } else {
            self.cmd_tx
                .send(RenderCommand::CreateWindow {
                    emacs_frame_id: request.frame_id.0,
                    width: request.width,
                    height: request.height,
                    title: request.title,
                })
                .map_err(|err| format!("failed to create additional GUI window: {err}"))?;
        }
        Ok(())
    }

    fn opening_gui_frame_pending(&self) -> bool {
        !self.primary_window_adopted
    }

    fn resize_gui_frame(&mut self, request: GuiFrameHostRequest) -> Result<(), String> {
        let emacs_frame_id = if self.primary_frame_id == Some(request.frame_id) {
            0
        } else {
            request.frame_id.0
        };
        tracing::debug!(
            "PrimaryWindowDisplayHost::resize_gui_frame fid=0x{:x} route=0x{:x} size={}x{}",
            request.frame_id.0,
            emacs_frame_id,
            request.width,
            request.height
        );
        self.cmd_tx
            .send(RenderCommand::ResizeWindow {
                emacs_frame_id,
                width: request.width,
                height: request.height,
            })
            .map_err(|err| format!("failed to resize GUI frame: {err}"))?;
        Ok(())
    }

    fn current_primary_window_size(&self) -> Option<GuiFrameHostSize> {
        if self.primary_window_adopted {
            return None;
        }
        let state = match self.primary_window_size.lock() {
            Ok(state) => *state,
            Err(poisoned) => *poisoned.into_inner(),
        };
        Some(GuiFrameHostSize {
            width: state.width,
            height: state.height,
        })
    }

    fn resolve_font_for_char(
        &mut self,
        request: FontResolveRequest,
    ) -> Result<Option<ResolvedFontMatch>, String> {
        let requested_family = request.face.family.as_deref().unwrap_or("Monospace");
        let requested_weight = request.face.weight.unwrap_or(FontWeight::NORMAL).0;
        let requested_italic = request
            .face
            .slant
            .map(|slant| slant.is_italic())
            .unwrap_or(false);
        let font_size = font_size_px_for_face(&request.face);
        let selected = self
            .font_metrics
            .get_or_insert_with(FontMetricsService::new)
            .select_font_for_char(
                request.character,
                requested_family,
                requested_weight,
                requested_italic,
                font_size,
            );
        tracing::debug!(
            target: "neomacs::font_at",
            character = %request.character,
            requested_family,
            requested_weight,
            requested_italic,
            font_size,
            request_face = ?request.face,
            selected = ?selected,
            "display host resolved font-at request"
        );
        Ok(selected.map(|font| ResolvedFontMatch {
            family: font.family,
            foundry: None,
            weight: font.weight,
            slant: font.slant,
            width: font.width,
            postscript_name: font.postscript_name,
        }))
    }
}

fn font_size_px_for_face(face: &neovm_core::face::Face) -> f32 {
    let default_font_size = face_height_to_pixels(100);
    match &face.height {
        Some(FaceHeight::Absolute(tenths)) => face_height_to_pixels(*tenths),
        Some(FaceHeight::Relative(scale)) => default_font_size * (*scale as f32),
        None => default_font_size,
    }
}

fn main() {
    if let Some(action) = classify_early_cli_action(std::env::args()) {
        match action {
            EarlyCliAction::PrintHelp { program } => {
                print!("{}", render_help_text(&program));
            }
            EarlyCliAction::PrintVersion => {
                print!("{}", render_version_text());
            }
        }
        return;
    }

    let startup = parse_startup_options(std::env::args()).unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(1);
    });

    // 1. Initialize logging
    neomacs_display_runtime::init_logging();

    tracing::info!(
        "Neomacs {} starting (pure Rust, backend={}, pid={})",
        neomacs_display_runtime::VERSION,
        neomacs_display_runtime::CORE_BACKEND,
        std::process::id()
    );
    tracing::info!("Startup frontend: {:?}", startup.frontend);
    if let Some(device) = startup.terminal_device.as_deref() {
        tracing::warn!(
            "terminal device {:?} requested; using current tty until explicit device handoff lands",
            device
        );
    }

    match startup.frontend {
        FrontendKind::Gui => reset_terminal_runtime(),
        FrontendKind::Tty => configure_terminal_runtime(detect_tty_runtime()),
    }

    let bootstrap_display = bootstrap_display_config(startup.frontend);
    let (width, height) = startup_dimensions(startup.frontend, bootstrap_frame_metrics());
    // 2. Initialize the evaluator from the canonical core bootstrap.
    let mut evaluator =
        neovm_core::emacs_core::load::create_bootstrap_evaluator_cached_with_features(
            BOOTSTRAP_CORE_FEATURES,
        )
        .expect("core bootstrap should succeed");
    evaluator.setup_thread_locals();
    evaluator.set_max_depth(1600);
    // Disable GC during startup — the bytecode VM's specpdl is not yet
    // scanned by the GC, so collections during VM execution can free
    // values that are still referenced by unwind-protect cleanup forms.
    evaluator.set_gc_threshold(usize::MAX);
    evaluator.set_variable("dump-mode", Value::Nil);
    tracing::info!("Evaluator initialized");

    // 3. Bootstrap the host-side initial frame/buffers.
    let _bootstrap = bootstrap_buffers(&mut evaluator, width, height, bootstrap_display);
    neovm_core::emacs_core::load::apply_runtime_startup_state(&mut evaluator)
        .expect("runtime startup state should succeed");
    let frame_id = evaluator
        .frame_manager()
        .selected_frame()
        .expect("No selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut evaluator, frame_id, &startup);

    if startup.frontend == FrontendKind::Gui {
        // Recalculate all face specs on the new GUI frame.
        // The pdump was built with a TTY-like frame (no color), so defface
        // specs fell through to (t :inverse-video t). Now that the frame has
        // display-type=color, re-evaluate to get the correct graphical attrs.
        recalc_faces_for_gui_frame(&mut evaluator);
    }

    maybe_install_startup_phase_trace(&mut evaluator);

    // 4. Create communication channels before entering GNU's outer
    //    recursive-edit command loop. GNU evaluates `top-level` from that
    //    outer loop, not directly from `main`.
    let comms = ThreadComms::new().expect("Failed to create thread comms");
    let (emacs_comms, render_comms) = comms.split();
    let primary_window_size: SharedPrimaryWindowSize =
        Arc::new(Mutex::new(PrimaryWindowSize { width, height }));
    if startup.frontend == FrontendKind::Gui {
        evaluator.set_display_host(Box::new(PrimaryWindowDisplayHost {
            cmd_tx: emacs_comms.cmd_tx.clone(),
            primary_window_adopted: false,
            primary_frame_id: None,
            font_metrics: None,
            primary_window_size: Arc::clone(&primary_window_size),
        }));
    }

    // 5. Spawn the frontend loop matching the requested startup mode.
    let frontend = match startup.frontend {
        FrontendKind::Gui => {
            let image_dimensions: SharedImageDimensions = Arc::new(Mutex::new(HashMap::new()));
            let shared_monitors: SharedMonitorInfo =
                Arc::new((Mutex::new(Vec::new()), std::sync::Condvar::new()));
            let render_thread = RenderThread::spawn(
                render_comms,
                width,
                height,
                "Neomacs".to_string(),
                Arc::clone(&image_dimensions),
                Arc::clone(&shared_monitors),
                #[cfg(feature = "neo-term")]
                Arc::new(Mutex::new(HashMap::new())),
            )
            .unwrap_or_else(|err| {
                eprintln!("neomacs: failed to start GUI frontend: {err}");
                std::process::exit(1);
            });
            tracing::info!("GUI render thread spawned ({}x{})", width, height);
            FrontendHandle::Gui(render_thread)
        }
        FrontendKind::Tty => {
            let tty_thread = tty_frontend::TtyFrontend::spawn(render_comms);
            tracing::info!("TTY frontend spawned");
            FrontendHandle::Tty(tty_thread)
        }
    };

    // 6. Run initial layout and send first frame
    let mut frame_glyphs = FrameGlyphBuffer::with_size(width as f32, height as f32);
    run_layout(&mut evaluator, &mut frame_glyphs);
    let _ = emacs_comms.frame_tx.try_send(frame_glyphs.clone());
    tracing::info!("Initial frame sent ({} glyphs)", frame_glyphs.glyphs.len());

    // 7. Create input bridge: convert display runtime events → keyboard events
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    let display_input_rx = emacs_comms.input_rx;
    let primary_window_size_for_input = Arc::clone(&primary_window_size);
    std::thread::Builder::new()
        .name("input-bridge".to_string())
        .spawn(move || {
            while let Ok(event) = display_input_rx.recv() {
                record_primary_window_resize(&primary_window_size_for_input, &event);
                if let Some(kb_event) = input_bridge::convert_display_event(event) {
                    if input_tx.send(kb_event).is_err() {
                        break; // Evaluator dropped
                    }
                }
            }
        })
        .expect("Failed to spawn input bridge thread");

    // 8. Connect evaluator to input system
    let wakeup_fd = emacs_comms.wakeup_read_fd;
    evaluator.init_input_system(input_rx, wakeup_fd);

    // 9. Set up redisplay callback (layout engine + send frame)
    let frame_tx = emacs_comms.frame_tx;
    evaluator.redisplay_fn = Some(Box::new(move |eval: &mut Evaluator| {
        eval.setup_thread_locals();
        run_layout(eval, &mut frame_glyphs);
        let _ = frame_tx.try_send(frame_glyphs.clone());
    }));

    // Add undo boundary after startup so initial content isn't undoable
    if let Some(buf) = evaluator.buffer_manager_mut().current_buffer_mut() {
        let mut ul = buf.get_undo_list();
        neovm_core::buffer::undo_list_boundary(&mut ul);
        buf.set_undo_list(ul);
    }

    // 10. Enter GNU's outer command loop. This mirrors src/emacs.c, which
    //     enters recursive-edit and lets the outer command loop evaluate the
    //     `top-level` startup form before reading interactive input.
    tracing::info!("Entering GNU command loop (recursive-edit)...");
    let exit_status = evaluator.recursive_edit();
    if exit_status.is_ok() {
        tracing::info!("Command loop exited normally");
    } else {
        tracing::warn!("Command loop exited with error");
    }

    // 11. Shutdown
    tracing::info!("Shutting down...");
    let _ = emacs_comms
        .cmd_tx
        .try_send(neomacs_display_runtime::thread_comm::RenderCommand::Shutdown);
    frontend.join();
    tracing::info!("Neomacs exited cleanly");

    if let Some(request) = evaluator.shutdown_request() {
        if request.restart {
            tracing::warn!("restart requested via kill-emacs, but restart is not implemented yet");
        }
        if request.exit_code != 0 {
            std::process::exit(request.exit_code);
        }
    }
}

// ---------------------------------------------------------------------------
// Bootstrap helpers
// ---------------------------------------------------------------------------

struct BootstrapResult {
    #[allow(dead_code)]
    scratch_id: BufferId,
    #[allow(dead_code)]
    minibuf_id: BufferId,
}

#[derive(Clone, Copy, Debug)]
struct BootstrapFrameMetrics {
    char_width: f32,
    char_height: f32,
    font_pixel_size: f32,
}

fn font_weight_symbol(weight: FontWeight) -> &'static str {
    match weight.0 {
        0..=150 => "thin",
        151..=250 => "extra-light",
        251..=350 => "light",
        351..=450 => "normal",
        451..=550 => "medium",
        551..=650 => "semi-bold",
        651..=750 => "bold",
        751..=850 => "extra-bold",
        _ => "black",
    }
}

fn font_slant_symbol(slant: FontSlant) -> &'static str {
    match slant {
        FontSlant::Normal => "normal",
        FontSlant::Italic => "italic",
        FontSlant::Oblique => "oblique",
        FontSlant::ReverseItalic => "reverse-italic",
        FontSlant::ReverseOblique => "reverse-oblique",
    }
}

fn font_width_symbol(width: FontWidth) -> &'static str {
    match width {
        FontWidth::UltraCondensed => "ultra-condensed",
        FontWidth::ExtraCondensed => "extra-condensed",
        FontWidth::Condensed => "condensed",
        FontWidth::SemiCondensed => "semi-condensed",
        FontWidth::Normal => "normal",
        FontWidth::SemiExpanded => "semi-expanded",
        FontWidth::Expanded => "expanded",
        FontWidth::ExtraExpanded => "extra-expanded",
        FontWidth::UltraExpanded => "ultra-expanded",
    }
}

fn bootstrap_default_font_parameter(font_pixel_size: f32) -> Value {
    let mut metrics_svc = FontMetricsService::new();
    let selected = metrics_svc.select_font_for_char('M', "Monospace", 400, false, font_pixel_size);

    let family = selected
        .as_ref()
        .map(|font| font.family.as_str())
        .unwrap_or("Monospace");
    let weight = selected
        .as_ref()
        .map(|font| font_weight_symbol(font.weight))
        .unwrap_or("normal");
    let slant = selected
        .as_ref()
        .map(|font| font_slant_symbol(font.slant))
        .unwrap_or("normal");
    let width = selected
        .as_ref()
        .map(|font| font_width_symbol(font.width))
        .unwrap_or("normal");

    Value::vector(vec![
        Value::keyword("font-object"),
        Value::keyword("family"),
        Value::string(family),
        Value::keyword("weight"),
        Value::symbol(weight),
        Value::keyword("slant"),
        Value::symbol(slant),
        Value::keyword("width"),
        Value::symbol(width),
        // GNU stores default-face absolute size in 1/10pt.  Use 100 as the
        // neutral startup default so faces.el can realize the selected font
        // without falling back to the bootstrap `height=1` placeholder.
        Value::keyword("size"),
        Value::Int(100),
        Value::keyword("height"),
        Value::Int(100),
    ])
}

fn bootstrap_frame_metrics() -> BootstrapFrameMetrics {
    let font_pixel_size = 16.0;
    let mut metrics_svc = FontMetricsService::new();
    let metrics = metrics_svc.font_metrics("Monospace", 400, false, font_pixel_size);
    BootstrapFrameMetrics {
        char_width: metrics.char_width.max(1.0),
        char_height: metrics.line_height.max(1.0),
        font_pixel_size,
    }
}

fn bootstrap_buffers(
    eval: &mut Evaluator,
    width: u32,
    height: u32,
    display: BootstrapDisplayConfig,
) -> BootstrapResult {
    let frame_metrics = bootstrap_frame_metrics();
    let find_or_create_buffer = |eval: &mut Evaluator, name: &str| {
        eval.buffer_manager()
            .find_buffer_by_name(name)
            .unwrap_or_else(|| eval.buffer_manager_mut().create_buffer(name))
    };

    // Reuse GNU startup buffers instead of creating duplicate names on top of
    // cached bootstrap state.
    let scratch_id = find_or_create_buffer(eval, "*scratch*");
    let _ = eval
        .buffer_manager_mut()
        .clear_buffer_labeled_restrictions(scratch_id);
    if let Some(buf) = eval.buffer_manager_mut().get_mut(scratch_id) {
        buf.widen();
        let content = ";; This buffer is for text that is not saved, and for Lisp evaluation.\n\
                       ;; To create a file, visit it with C-x C-f and enter text in its buffer.\n\n";
        if buf.text.len() == 0 {
            buf.goto_byte(0);
            buf.insert(content);
            buf.set_modified(false);
        }
        buf.goto_byte(buf.point_max());
    }

    // Set *scratch* as the current buffer
    eval.buffer_manager_mut().set_current(scratch_id);

    let msg_id = find_or_create_buffer(eval, "*Messages*");
    let _ = eval
        .buffer_manager_mut()
        .clear_buffer_labeled_restrictions(msg_id);
    if let Some(buf) = eval.buffer_manager_mut().get_mut(msg_id) {
        buf.widen();
        buf.goto_byte(0);
    }

    let mini_id = find_or_create_buffer(eval, " *Minibuf-0*");
    let _ = eval
        .buffer_manager_mut()
        .clear_buffer_labeled_restrictions(mini_id);
    if let Some(buf) = eval.buffer_manager_mut().get_mut(mini_id) {
        buf.widen();
        buf.goto_byte(0);
    }

    let frame_id = {
        let frame_manager = eval.frame_manager();
        let selected = frame_manager.selected_frame().map(|frame| frame.id);
        let should_reuse_existing = selected.is_some() && frame_manager.frame_list().len() == 1;
        (selected, should_reuse_existing)
    };
    let frame_id = if frame_id.1 {
        let frame_id = frame_id.0.expect("selected startup frame");
        tracing::info!(
            "Reusing existing startup frame {:?} as bootstrap frame ({}x{})",
            frame_id,
            width,
            height
        );
        frame_id
    } else {
        let frame_id = eval
            .frame_manager_mut()
            .create_frame("F1", width, height, scratch_id);
        tracing::info!(
            "Created frame {:?} ({}x{}) with *scratch*={:?}",
            frame_id,
            width,
            height,
            scratch_id
        );
        frame_id
    };
    let _ = eval.frame_manager_mut().select_frame(frame_id);

    // Seed frame parameters so GNU Lisp startup sees the correct host surface.
    if let Some(frame) = eval.frame_manager_mut().get_mut(frame_id) {
        let default_font = bootstrap_default_font_parameter(frame_metrics.font_pixel_size);
        frame.width = width;
        frame.height = height;
        frame.visible = true;
        if let Some(window_system) = display.window_system_symbol() {
            frame.set_window_system(Some(Value::symbol(window_system)));
        } else {
            frame.set_window_system(None);
        }
        frame.parameters.insert(
            "display-type".to_string(),
            Value::symbol(display.display_type_symbol()),
        );
        frame.parameters.insert(
            "background-mode".to_string(),
            Value::symbol(display.background_mode),
        );
        frame
            .parameters
            .insert("font".to_string(), default_font.clone());
        frame
            .parameters
            .insert("font-parameter".to_string(), default_font);
        frame.title = "Neomacs".to_string();
        frame.font_pixel_size = frame_metrics.font_pixel_size;
        frame.char_width = frame_metrics.char_width;
        frame.char_height = frame_metrics.char_height;
        frame.sync_tab_bar_height_from_parameters();
        if let Window::Leaf {
            buffer_id,
            window_start,
            point,
            ..
        } = &mut frame.root_window
        {
            *buffer_id = scratch_id;
            *window_start = 0;
            *point = 0;
        }
    }

    // Fix window geometry: root window takes frame height minus minibuffer.
    if let Some(frame) = eval.frame_manager_mut().get_mut(frame_id) {
        let mini_h = frame.char_height.max(1.0);
        let mini_y = height as f32 - mini_h;
        if let Window::Leaf { bounds, .. } = &mut frame.root_window {
            bounds.height = mini_y;
        }
        if let Some(mini_leaf) = &mut frame.minibuffer_leaf {
            if let Window::Leaf {
                buffer_id,
                window_start,
                point,
                bounds,
                ..
            } = mini_leaf
            {
                *buffer_id = mini_id;
                *window_start = 0;
                *point = 0;
                bounds.y = mini_y;
                bounds.height = mini_h;
                bounds.width = width as f32;
            }
        }
    }

    BootstrapResult {
        scratch_id,
        minibuf_id: mini_id,
    }
}

fn configure_gnu_startup_state(eval: &mut Evaluator, frame_id: FrameId, startup: &StartupOptions) {
    let argv_strings = startup.forwarded_args.iter().cloned().collect::<Vec<_>>();
    let argv = argv_strings
        .iter()
        .cloned()
        .map(Value::string)
        .collect::<Vec<_>>();
    let argv_left = argv_strings
        .iter()
        .skip(1)
        .cloned()
        .map(Value::string)
        .collect::<Vec<_>>();
    let invocation_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("/"));
    let invocation_name = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "neomacs".to_string());
    let invocation_directory = ensure_dir_string(&invocation_directory);

    eval.set_variable("command-line-args", Value::list(argv));
    eval.set_variable("command-line-args-left", Value::list(argv_left));
    eval.set_variable("command-line-processed", Value::Nil);
    eval.set_variable(
        "noninteractive",
        if startup.noninteractive {
            Value::True
        } else {
            Value::Nil
        },
    );
    let (terminal_frame, frame_initial_frame, default_minibuffer_frame) = match startup.frontend {
        FrontendKind::Gui => {
            let window_system = Value::symbol(gui_window_system_symbol());
            eval.set_variable("window-system", window_system);
            eval.set_variable("initial-window-system", window_system);
            (
                Value::Nil,
                Value::Frame(frame_id.0),
                Value::Frame(frame_id.0),
            )
        }
        FrontendKind::Tty => {
            eval.set_variable("window-system", Value::Nil);
            eval.set_variable("initial-window-system", Value::Nil);
            (Value::Frame(frame_id.0), Value::Nil, Value::Nil)
        }
    };
    eval.set_variable("invocation-name", Value::string(invocation_name));
    eval.set_variable("invocation-directory", Value::string(invocation_directory));
    let cwd = std::env::current_dir()
        .map(|p| ensure_dir_string(&p))
        .unwrap_or_else(|_| "/".to_string());
    eval.set_variable("default-directory", Value::string(&cwd));
    eval.set_variable("terminal-frame", terminal_frame);
    eval.set_variable("frame-initial-frame", frame_initial_frame);
    eval.set_variable("default-minibuffer-frame", default_minibuffer_frame);
    // Skip the splash screen — its fill-region is extremely slow through
    // with_mirrored_evaluator.  Users who want it can set this to nil in
    // their init file.
    eval.set_variable("inhibit-startup-screen", Value::True);
}

/// Re-run GNU face initialization for the initial GUI frame.
///
/// During pdump bootstrap, faces are evaluated on a TTY-like frame
/// (no color support), so GNU's frame-local face setup has to run again
/// once we have a real GUI frame and its frame parameters.
fn recalc_faces_for_gui_frame(eval: &mut Evaluator) {
    let elisp = r#"
(when (fboundp 'face-set-after-frame-default)
  (face-set-after-frame-default
   (selected-frame)
   (frame-parameters (selected-frame))))
"#;
    eval.setup_thread_locals();
    match neovm_core::emacs_core::parse_forms(elisp) {
        Ok(forms) => {
            for form in &forms {
                let _ = eval.eval_expr(form);
            }
            tracing::info!("Recalculated face specs for GUI frame");
        }
        Err(e) => {
            tracing::warn!("recalc_faces parse error: {:?}", e);
        }
    }
}

fn run_gnu_startup(eval: &mut Evaluator) {
    eval.setup_thread_locals();
    let top_level = eval.obarray().symbol_value("top-level").cloned();
    tracing::info!("top-level variable before startup: {:?}", top_level);
    let forms =
        neovm_core::emacs_core::parse_forms("(eval top-level)").expect("top-level form parses");
    let result = match eval.eval_expr(&forms[0]) {
        Ok(value) => value,
        Err(EvalError::Signal { symbol, data }) => {
            let decoded = data
                .iter()
                .map(|value| print_value_with_eval(eval, value))
                .collect::<Vec<_>>();
            let last_phase = eval
                .obarray()
                .symbol_value("neomacs--startup-last-phase")
                .cloned()
                .map(|value| print_value_with_eval(eval, &value));
            tracing::warn!(
                "GNU top-level startup signaled: {} {:?} last-phase={:?} (continuing anyway)",
                resolve_sym(symbol),
                decoded,
                last_phase
            );
            Value::Nil
        }
        Err(other) => {
            tracing::warn!("GNU top-level startup error: {other:?} (continuing anyway)");
            Value::Nil
        }
    };
    tracing::info!("top-level startup returned: {:?}", result);
}

fn maybe_install_startup_phase_trace(eval: &mut Evaluator) {
    if std::env::var("NEOMACS_TRACE_STARTUP_PHASES").unwrap_or_default() != "1" {
        return;
    }
    let source = r#"
        (progn
          (defvar neomacs--startup-last-phase nil)
          (defun neomacs--startup-trace-around (name orig &rest args)
            (setq neomacs--startup-last-phase name)
            (apply orig args))
          (dolist (fn '(set-locale-environment
                        command-line
                        frame-initialize
                        display-graphic-p
                        tab-bar-height
                        tool-bar-height
                        tab-bar-mode
                        tool-bar-mode
                        frame-parameters
                        frame-parameter
                        modify-frame-parameters
                        make-frame
                        frame-set-background-mode
                        startup--setup-quote-display
                        frame-notice-user-settings
                        tty-run-terminal-initialization
                        face-set-after-frame-default))
            (when (fboundp fn)
              (advice-add fn :around
                          (apply-partially #'neomacs--startup-trace-around fn)))))
    "#;
    let forms =
        neovm_core::emacs_core::parse_forms(source).expect("startup trace helper should parse");
    for form in &forms {
        eval.eval_expr(form)
            .expect("startup trace helper should install");
    }
}

fn ensure_dir_string(path: &Path) -> String {
    let mut dir = path.to_string_lossy().to_string();
    if !dir.ends_with('/') {
        dir.push('/');
    }
    dir
}

fn current_layout_frame_id(evaluator: &Evaluator) -> Option<FrameId> {
    evaluator
        .frame_manager()
        .selected_frame()
        .map(|frame| frame.id)
}

/// Run the layout engine on the selected live frame.
fn run_layout(evaluator: &mut Evaluator, frame_glyphs: &mut FrameGlyphBuffer) {
    use neomacs_display_runtime::layout::LayoutEngine;

    let Some(frame_id) = current_layout_frame_id(evaluator) else {
        tracing::warn!("run_layout: no selected live frame");
        return;
    };

    thread_local! {
        static ENGINE: std::cell::RefCell<LayoutEngine> = std::cell::RefCell::new(LayoutEngine::new());
    }

    ENGINE.with(|engine| {
        engine
            .borrow_mut()
            .layout_frame_rust(evaluator, frame_id, frame_glyphs);
    });
}

#[cfg(test)]
mod tests {
    use super::{
        BOOTSTRAP_CORE_FEATURES, BootstrapDisplayConfig, EarlyCliAction, FrontendKind,
        PrimaryWindowDisplayHost, PrimaryWindowSize, StartupOptions, bootstrap_buffers,
        bootstrap_display_config, bootstrap_frame_metrics, classify_early_cli_action,
        configure_gnu_startup_state, current_layout_frame_id, parse_startup_options,
        render_help_text, render_version_text, run_gnu_startup,
    };
    use neomacs_display_runtime::thread_comm::RenderCommand;
    use neovm_core::emacs_core::Evaluator;
    use neovm_core::emacs_core::GuiFrameHostRequest;
    use neovm_core::emacs_core::Value;
    use neovm_core::emacs_core::load::{
        apply_runtime_startup_state, create_bootstrap_evaluator_cached_with_features,
        create_bootstrap_evaluator_with_features,
    };
    use neovm_core::emacs_core::parse_forms;
    use neovm_core::emacs_core::print_value_with_eval;
    use neovm_core::emacs_core::value::list_to_vec;
    use neovm_core::window::FrameId;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    fn gui_display() -> BootstrapDisplayConfig {
        bootstrap_display_config(FrontendKind::Gui)
    }

    fn gui_startup() -> StartupOptions {
        StartupOptions {
            frontend: FrontendKind::Gui,
            forwarded_args: vec!["neomacs".to_string()],
            terminal_device: None,
            noninteractive: false,
        }
    }

    fn gui_startup_with_args(args: &[&str]) -> StartupOptions {
        let mut forwarded_args = vec!["neomacs".to_string()];
        forwarded_args.extend(args.iter().map(|arg| (*arg).to_string()));
        StartupOptions {
            frontend: FrontendKind::Gui,
            forwarded_args,
            terminal_device: None,
            noninteractive: false,
        }
    }

    fn bootstrap_runtime_gui_startup(eval: &mut Evaluator) -> FrameId {
        let _bootstrap = bootstrap_buffers(eval, 960, 640, gui_display());
        apply_runtime_startup_state(eval).expect("runtime startup state should succeed");
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(eval, frame_id, &gui_startup());
        frame_id
    }

    #[test]
    fn opening_gui_frame_adoption_does_not_push_stale_window_size() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let mut host = PrimaryWindowDisplayHost {
            cmd_tx,
            primary_window_adopted: false,
            primary_frame_id: None,
            font_metrics: None,
            primary_window_size: Arc::new(Mutex::new(PrimaryWindowSize {
                width: 1600,
                height: 1800,
            })),
        };

        neovm_core::emacs_core::DisplayHost::realize_gui_frame(
            &mut host,
            GuiFrameHostRequest {
                frame_id: FrameId(0x100000001),
                width: 960,
                height: 640,
                title: "Neomacs".to_string(),
            },
        )
        .expect("adopt opening gui frame");

        let commands: Vec<_> = cmd_rx.try_iter().collect();
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            RenderCommand::SetWindowTitle { title } => assert_eq!(title, "Neomacs"),
            other => panic!("expected SetWindowTitle, got {other:?}"),
        }
        assert!(host.primary_window_adopted);
        assert_eq!(host.primary_frame_id, Some(FrameId(0x100000001)));
    }

    #[test]
    fn current_layout_frame_follows_selected_frame() {
        let mut eval = Evaluator::new();
        let b1 = eval.buffer_manager_mut().create_buffer("*one*");
        let b2 = eval.buffer_manager_mut().create_buffer("*two*");
        let f1 = eval.frame_manager_mut().create_frame("F1", 80, 24, b1);
        let f2 = eval.frame_manager_mut().create_frame("F2", 80, 24, b2);

        assert_eq!(current_layout_frame_id(&eval), Some(f1));
        assert!(eval.frame_manager_mut().select_frame(f2));
        assert_eq!(current_layout_frame_id(&eval), Some(f2));
    }

    #[test]
    fn current_layout_frame_tracks_surrogate_after_bootstrap_frame_deletion() {
        let mut eval = Evaluator::new();
        let b1 = eval.buffer_manager_mut().create_buffer("*one*");
        let b2 = eval.buffer_manager_mut().create_buffer("*two*");
        let f1 = eval.frame_manager_mut().create_frame("F1", 80, 24, b1);
        let f2 = eval.frame_manager_mut().create_frame("F2", 80, 24, b2);

        assert_eq!(current_layout_frame_id(&eval), Some(f1));
        assert!(eval.frame_manager_mut().delete_frame(f1));
        assert_eq!(current_layout_frame_id(&eval), Some(f2));
    }

    #[test]
    fn early_cli_handles_gnu_c_owned_help_and_version_options() {
        assert_eq!(
            classify_early_cli_action(
                ["./target/release/neomacs", "--help"]
                    .into_iter()
                    .map(str::to_string)
            ),
            Some(EarlyCliAction::PrintHelp {
                program: "./target/release/neomacs".to_string()
            })
        );
        assert_eq!(
            classify_early_cli_action(
                ["./target/release/neomacs", "-version"]
                    .into_iter()
                    .map(str::to_string)
            ),
            Some(EarlyCliAction::PrintVersion)
        );
        assert_eq!(
            classify_early_cli_action(
                ["./target/release/neomacs", "--", "--help"]
                    .into_iter()
                    .map(str::to_string)
            ),
            None
        );
    }

    #[test]
    fn early_cli_help_uses_invoked_program_name_and_gnu_style_usage() {
        let help = render_help_text("/tmp/neomacs");
        assert!(help.starts_with("Usage: /tmp/neomacs [OPTION-OR-FILENAME]...\n\n"));
        assert!(help.contains("--help                          display this help and exit"));
        assert!(help.contains("--quick, -Q                 equivalent to:"));
    }

    #[test]
    fn early_cli_version_reports_neomacs_identity() {
        let version = render_version_text();
        assert!(version.starts_with("Neomacs "));
        assert!(version.contains("Standalone Rust binary for Neomacs"));
    }

    #[test]
    fn startup_option_parser_promotes_nw_and_strips_c_owned_display_flags() {
        let parsed = parse_startup_options(
            [
                "neomacs",
                "-nw",
                "--display",
                ":1",
                "--terminal=/dev/pts/7",
                "README.md",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .expect("startup options should parse");

        assert_eq!(parsed.frontend, FrontendKind::Tty);
        assert!(!parsed.noninteractive);
        assert_eq!(parsed.terminal_device.as_deref(), Some("/dev/pts/7"));
        assert_eq!(
            parsed.forwarded_args,
            vec!["neomacs".to_string(), "README.md".to_string()]
        );
    }

    #[test]
    fn startup_option_parser_promotes_batch_to_noninteractive_and_strips_batch_flag() {
        let parsed = parse_startup_options(
            ["neomacs", "--batch", "-Q", "--eval", "(princ 1)"]
                .into_iter()
                .map(str::to_string),
        )
        .expect("startup options should parse");

        assert_eq!(parsed.frontend, FrontendKind::Tty);
        assert!(parsed.noninteractive);
        assert_eq!(
            parsed.forwarded_args,
            vec![
                "neomacs".to_string(),
                "-Q".to_string(),
                "--eval".to_string(),
                "(princ 1)".to_string()
            ]
        );
    }

    #[test]
    fn configure_gnu_startup_state_marks_bootstrap_gui_frame_as_initial_frame() {
        let mut eval = Evaluator::new();
        configure_gnu_startup_state(&mut eval, FrameId(42), &gui_startup());

        assert_eq!(
            eval.obarray().symbol_value("terminal-frame"),
            Some(&Value::Nil)
        );
        assert_eq!(
            eval.obarray().symbol_value("frame-initial-frame"),
            Some(&Value::Frame(42))
        );
        assert_eq!(
            eval.obarray().symbol_value("default-minibuffer-frame"),
            Some(&Value::Frame(42))
        );
    }

    #[test]
    fn configure_gnu_startup_state_reports_neomacs_window_system_for_gui_boots() {
        let mut eval = Evaluator::new();
        configure_gnu_startup_state(&mut eval, FrameId(42), &gui_startup());

        assert_eq!(
            eval.obarray().symbol_value("window-system"),
            Some(&Value::symbol("neomacs"))
        );
        assert_eq!(
            eval.obarray().symbol_value("initial-window-system"),
            Some(&Value::symbol("neomacs"))
        );
    }

    #[test]
    fn configure_gnu_startup_state_clears_window_system_for_tty_boots() {
        let mut eval = Evaluator::new();
        let startup = StartupOptions {
            frontend: FrontendKind::Tty,
            forwarded_args: vec!["neomacs".to_string(), "-q".to_string()],
            terminal_device: Some("/dev/tty".to_string()),
            noninteractive: false,
        };
        configure_gnu_startup_state(&mut eval, FrameId(7), &startup);

        assert_eq!(
            eval.obarray().symbol_value("window-system"),
            Some(&Value::Nil)
        );
        assert_eq!(
            eval.obarray().symbol_value("initial-window-system"),
            Some(&Value::Nil)
        );
        assert_eq!(
            eval.obarray().symbol_value("command-line-args"),
            Some(&Value::list(vec![
                Value::string("neomacs"),
                Value::string("-q")
            ]))
        );
        assert_eq!(
            eval.obarray().symbol_value("command-line-args-left"),
            Some(&Value::list(vec![Value::string("-q")]))
        );
    }

    #[test]
    fn configure_gnu_startup_state_marks_batch_mode_noninteractive() {
        let mut eval = Evaluator::new();
        let startup = StartupOptions {
            frontend: FrontendKind::Tty,
            forwarded_args: vec![
                "neomacs".to_string(),
                "-Q".to_string(),
                "--eval".to_string(),
                "(princ 1)".to_string(),
            ],
            terminal_device: None,
            noninteractive: true,
        };
        configure_gnu_startup_state(&mut eval, FrameId(9), &startup);

        assert_eq!(
            eval.obarray().symbol_value("noninteractive"),
            Some(&Value::True)
        );
        assert_eq!(
            eval.obarray().symbol_value("command-line-args"),
            Some(&Value::list(vec![
                Value::string("neomacs"),
                Value::string("-Q"),
                Value::string("--eval"),
                Value::string("(princ 1)"),
            ]))
        );
    }

    #[test]
    fn configure_gnu_startup_state_seeds_command_line_args_left_for_gnu_startup() {
        let mut eval = Evaluator::new();
        let startup = gui_startup_with_args(&["-Q", "-l", "/tmp/demo.el"]);
        configure_gnu_startup_state(&mut eval, FrameId(42), &startup);

        assert_eq!(
            eval.obarray().symbol_value("command-line-args-left"),
            Some(&Value::list(vec![
                Value::string("-Q"),
                Value::string("-l"),
                Value::string("/tmp/demo.el")
            ]))
        );
    }

    #[test]
    fn bootstrap_buffers_seed_frame_with_renderer_metrics() {
        let metrics = bootstrap_frame_metrics();
        let mut eval = Evaluator::new();
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap");
        assert_eq!(frame.char_width, metrics.char_width);
        assert_eq!(frame.char_height, metrics.char_height);
        assert_eq!(frame.font_pixel_size, metrics.font_pixel_size);
        let font_param = frame
            .parameters
            .get("font")
            .expect("bootstrap GUI frame should seed a font frame parameter");
        assert!(matches!(font_param, Value::Vector(_)));
        let minibuffer_height = frame
            .minibuffer_leaf
            .as_ref()
            .expect("minibuffer leaf")
            .bounds()
            .height;
        assert_eq!(minibuffer_height, metrics.char_height);
    }

    #[test]
    fn bootstrap_buffers_reuses_selected_startup_frame_when_one_already_exists() {
        let metrics = bootstrap_frame_metrics();
        let mut eval = Evaluator::new();
        let old_buffer = eval.buffer_manager_mut().create_buffer("*old*");
        let old_frame = eval
            .frame_manager_mut()
            .create_frame("old", 320, 200, old_buffer);
        {
            let frame = eval
                .frame_manager_mut()
                .get_mut(old_frame)
                .expect("old frame should exist");
            frame.title = "old".to_string();
        }

        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());

        assert_eq!(eval.frame_manager().frame_list().len(), 1);
        let selected = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap");
        assert_eq!(selected.id, old_frame);
        assert_eq!(selected.width, 960);
        assert_eq!(selected.height, 640);
        assert_eq!(
            selected.effective_window_system(),
            Some(Value::symbol("neomacs"))
        );
        assert_eq!(selected.title, "Neomacs");
        assert_eq!(selected.char_width, metrics.char_width);
        assert_eq!(selected.char_height, metrics.char_height);
        let minibuffer_height = selected
            .minibuffer_leaf
            .as_ref()
            .expect("minibuffer leaf")
            .bounds()
            .height;
        assert_eq!(minibuffer_height, metrics.char_height);
    }

    #[test]
    fn bootstrap_buffers_reuses_cached_surrogate_frame_when_it_is_the_only_selected_frame() {
        let metrics = bootstrap_frame_metrics();
        let mut eval = Evaluator::new();
        let old_buffer = eval.buffer_manager_mut().create_buffer("*old*");
        let surrogate = eval
            .frame_manager_mut()
            .create_frame("F1", 80, 25, old_buffer);

        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());

        assert_eq!(eval.frame_manager().frame_list().len(), 1);
        let selected = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap");
        assert_eq!(selected.id, surrogate);
        assert_eq!(selected.width, 960);
        assert_eq!(selected.height, 640);
        assert_eq!(
            selected.effective_window_system(),
            Some(Value::symbol("neomacs"))
        );
        assert_eq!(selected.char_width, metrics.char_width);
        assert_eq!(selected.char_height, metrics.char_height);
    }

    #[test]
    fn bootstrap_buffers_reuses_existing_named_buffers_in_cached_bootstrap() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let original_scratch = eval
            .buffer_manager()
            .find_buffer_by_name("*scratch*")
            .expect("bootstrap scratch");

        let bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());

        assert_eq!(bootstrap.scratch_id, original_scratch);
        let scratch_count = eval
            .buffer_manager()
            .buffer_list()
            .into_iter()
            .filter(|id| {
                eval.buffer_manager()
                    .get(*id)
                    .is_some_and(|buffer| buffer.name == "*scratch*")
            })
            .count();
        assert_eq!(scratch_count, 1);
    }

    #[test]
    fn gnu_startup_keeps_scratch_selected_under_q_startup() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _frame_id = bootstrap_runtime_gui_startup(&mut eval);

        run_gnu_startup(&mut eval);

        let current = eval
            .buffer_manager()
            .current_buffer()
            .expect("current buffer after startup");
        assert_eq!(current.name, "*scratch*");
    }

    #[test]
    fn gnu_startup_keeps_bootstrap_gui_frame_instead_of_creating_replacement_frame() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let frame_id = bootstrap_runtime_gui_startup(&mut eval);

        run_gnu_startup(&mut eval);

        let frame_ids: Vec<_> = eval.frame_manager().frame_list().into_iter().collect();
        assert_eq!(frame_ids, vec![frame_id]);
        assert_eq!(
            eval.frame_manager()
                .selected_frame()
                .expect("selected frame after startup")
                .id,
            frame_id
        );
    }

    #[test]
    fn gnu_startup_keeps_scratch_text_accessible_under_q_startup() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _frame_id = bootstrap_runtime_gui_startup(&mut eval);

        run_gnu_startup(&mut eval);

        let forms = parse_forms(
            r#"(with-current-buffer (current-buffer)
                 (list (buffer-name)
                       major-mode
                       (> (point-max) 1)
                       (> (buffer-size) 0)
                       (> (length
                           (buffer-substring-no-properties
                            (point-min)
                            (min (point-max) (+ (point-min) 16))))
                          0)))"#,
        )
        .expect("parse scratch accessibility probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("scratch accessibility probe should evaluate");
        assert_eq!(
            print_value_with_eval(&mut eval, &result),
            "(\"*scratch*\" lisp-interaction-mode t t t)"
        );
    }

    #[test]
    fn gnu_startup_preserves_default_fontset_alias() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let forms =
            parse_forms("(query-fontset \"fontset-default\")").expect("parse fontset query");
        let result = eval
            .eval_expr(&forms[0])
            .expect("fontset query should evaluate");
        assert_eq!(
            result,
            Value::string("-*-*-*-*-*-*-*-*-*-*-*-*-fontset-default")
        );
    }

    #[test]
    fn gnu_startup_posts_echo_area_message() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let forms = parse_forms(
            "(list (current-message)
                   (substring-no-properties (startup-echo-area-message)))",
        )
        .expect("parse startup echo probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup echo probe should evaluate");
        assert_eq!(
            print_value_with_eval(&mut eval, &result),
            "(nil \"For information about GNU Emacs and the GNU system, type C-h C-a.\")"
        );
    }

    #[test]
    fn gnu_startup_keeps_single_row_minibuffer() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let forms = parse_forms("(window-total-height (minibuffer-window))")
            .expect("parse minibuffer height probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("minibuffer height probe should evaluate");
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn gnu_startup_runtime_load_path_finds_mail_rfc6068() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let forms = parse_forms("(locate-library \"rfc6068\")")
            .expect("parse locate-library startup probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("locate-library startup probe should evaluate");
        let path = result
            .as_str()
            .expect("locate-library should return a resolved path string after startup");
        assert!(
            path.ends_with("/mail/rfc6068.el"),
            "expected GNU mail runtime path, got {path}"
        );
    }

    #[test]
    fn gnu_startup_where_is_internal_finds_about_emacs_on_help_prefix() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let forms = parse_forms(
            "(list
               (lookup-key help-map [1])
               (lookup-key (symbol-function 'help-command) [1])
               (lookup-key (current-global-map) [8])
               (lookup-key (current-global-map) [8 1]))",
        )
        .expect("parse startup help-prefix probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup help-prefix probe should evaluate");
        assert_eq!(
            print_value_with_eval(&mut eval, &result),
            "(about-emacs about-emacs help-command about-emacs)"
        );
    }

    #[test]
    #[ignore = "startup echo helper blocks in this harness; message redisplay is covered in neovm-core"]
    fn gnu_startup_requests_redisplay_for_echo_area_message() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        let redisplay_rows = Arc::new(Mutex::new(Vec::<String>::new()));
        let redisplay_rows_capture = Arc::clone(&redisplay_rows);
        eval.redisplay_fn = Some(Box::new(move |eval: &mut Evaluator| {
            redisplay_rows_capture
                .lock()
                .expect("redisplay row buffer")
                .push(eval.current_message_text().unwrap_or_default().to_string());
        }));

        let forms = parse_forms("(display-startup-echo-area-message)")
            .expect("parse startup echo-area display form");
        let result = eval
            .eval_expr(&forms[0])
            .expect("display-startup-echo-area-message should evaluate");
        assert_eq!(
            result,
            Value::string("For information about GNU Emacs and the GNU system, type C-h C-a.")
        );

        let rendered_rows = redisplay_rows.lock().expect("captured redisplay rows");

        assert!(
            rendered_rows
                .iter()
                .any(|row| row.contains("For information about GNU Emacs and the GNU system")),
            "expected startup echo message during redisplay, got: {rendered_rows:?}"
        );
    }

    #[test]
    fn gnu_startup_restores_meta_and_ctl_x_bindings() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let forms = parse_forms(
            r#"(list
                 (key-binding (kbd "M-x"))
                 (lookup-key (current-global-map) (kbd "M-x"))
                 (key-binding (kbd "C-x 2"))
                 (lookup-key (current-global-map) (kbd "C-x 2"))
                 (key-binding (kbd "C-x 3"))
                 (lookup-key (current-global-map) (kbd "C-x 3")))"#,
        )
        .expect("parse startup keybinding probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup keybinding probe should evaluate");
        assert_eq!(
            print_value_with_eval(&mut eval, &result),
            "(execute-extended-command execute-extended-command split-window-below split-window-below split-window-right split-window-right)"
        );
    }

    #[test]
    fn gnu_startup_formats_mode_line_for_target_window_buffer() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let forms = parse_forms(
            r#"(let* ((w (selected-window))
                      (buf (window-buffer w))
                      (mini (minibuffer-window)))
                 (with-current-buffer (window-buffer mini)
                   (format-mode-line "%b" nil w buf)))"#,
        )
        .expect("parse startup mode-line probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup mode-line probe should evaluate");
        assert_eq!(result, Value::string("*scratch*"));
    }

    #[test]
    fn gnu_startup_split_window_right_succeeds_on_opening_frame() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let (expected_width, expected_height) = {
            let frame = eval
                .frame_manager()
                .selected_frame()
                .expect("selected frame after startup");
            let selected = frame
                .selected_window()
                .expect("selected window after startup");
            let bounds = selected.bounds();
            (
                (bounds.width / frame.char_width) as i64,
                (bounds.height / frame.char_height) as i64,
            )
        };

        let forms = parse_forms(
            r#"(list
                 (window-total-width)
                 (window-total-height)
                 (window-min-size nil t)
                 (window-min-size nil nil)
                 (window-size-fixed-p (selected-window))
                 (window-size-fixed-p (selected-window) t)
                 (condition-case err
                     (progn (split-window-right) 'ok)
                   (error (list 'error (error-message-string err)))))"#,
        )
        .expect("parse startup split-window probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup split-window probe should evaluate");
        let items = list_to_vec(&result).expect("split-window result list");
        assert_eq!(items[0], Value::Int(expected_width));
        assert_eq!(items[1], Value::Int(expected_height));
        assert_eq!(items[2], Value::Int(10));
        assert_eq!(items[3], Value::Int(4));
        assert!(items[4].is_nil());
        assert!(items[5].is_nil());
        assert_eq!(items[6], Value::symbol("ok"));
    }

    #[test]
    fn gnu_startup_split_window_below_succeeds_on_opening_frame() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let (expected_width, expected_height) = {
            let frame = eval
                .frame_manager()
                .selected_frame()
                .expect("selected frame after startup");
            let selected = frame
                .selected_window()
                .expect("selected window after startup");
            let bounds = selected.bounds();
            (
                (bounds.width / frame.char_width) as i64,
                (bounds.height / frame.char_height) as i64,
            )
        };

        let forms = parse_forms(
            r#"(list
                 (window-total-width)
                 (window-total-height)
                 (window-min-size nil t)
                 (window-min-size nil nil)
                 (window-size-fixed-p (selected-window))
                 (window-size-fixed-p (selected-window) t)
                 (condition-case err
                     (progn (split-window-below) 'ok)
                   (error (list 'error (error-message-string err)))))"#,
        )
        .expect("parse startup split-window probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup split-window probe should evaluate");
        let items = list_to_vec(&result).expect("split-window result list");
        assert_eq!(items[0], Value::Int(expected_width));
        assert_eq!(items[1], Value::Int(expected_height));
        assert_eq!(items[2], Value::Int(10));
        assert_eq!(items[3], Value::Int(4));
        assert!(items[4].is_nil());
        assert!(items[5].is_nil());
        assert_eq!(items[6], Value::symbol("ok"));
    }

    #[test]
    fn gnu_startup_window_pixel_queries_use_live_frame_pixels() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let forms = parse_forms(
            r#"(list
                 (window-pixel-width)
                 (window-pixel-height)
                 (window-body-width nil t)
                 (window-body-height nil t)
                 (window-text-width nil t)
                 (window-text-height nil t)
                 (window-fringes)
                 (window-edges nil nil nil t)
                 (window-edges nil t nil t))"#,
        )
        .expect("parse startup pixel probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup pixel probe should evaluate");
        let items = list_to_vec(&result).expect("pixel query result list");
        let pixel_width = items[0].as_int().expect("window-pixel-width");
        let pixel_height = items[1].as_int().expect("window-pixel-height");
        let body_width = items[2].as_int().expect("window-body-width");
        let body_height = items[3].as_int().expect("window-body-height");
        let text_width = items[4].as_int().expect("window-text-width");
        let text_height = items[5].as_int().expect("window-text-height");
        let fringes = list_to_vec(&items[6]).expect("window fringes");
        let outer_edges = list_to_vec(&items[7]).expect("outer window edges");
        let inner_edges = list_to_vec(&items[8]).expect("inner window edges");
        let left_fringe = fringes[0].as_int().expect("left fringe");
        let right_fringe = fringes[1].as_int().expect("right fringe");

        assert_eq!(pixel_width, 960);
        assert!(pixel_height > 0);
        assert_eq!(body_width, pixel_width - left_fringe - right_fringe);
        assert_eq!(text_width, body_width);
        assert_eq!(body_height, text_height);
        assert!(pixel_height >= body_height);
        assert_eq!(
            outer_edges,
            vec![
                Value::Int(0),
                Value::Int(0),
                Value::Int(pixel_width),
                Value::Int(pixel_height)
            ]
        );
        assert_eq!(
            inner_edges,
            vec![
                Value::Int(left_fringe),
                Value::Int(0),
                Value::Int(pixel_width - right_fringe),
                Value::Int(body_height)
            ]
        );
    }

    #[test]
    fn gnu_startup_processes_load_option_from_forwarded_args() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate lives in workspace root");
        let face_test = repo_root.join("test/neomacs/neomacs-face-test.el");
        let startup = gui_startup_with_args(&[
            "-Q",
            "-l",
            face_test
                .to_str()
                .expect("face test path must be valid utf-8"),
        ]);
        configure_gnu_startup_state(&mut eval, frame_id, &startup);

        run_gnu_startup(&mut eval);

        let forms = parse_forms(
            r#"(list
                 (fboundp 'neomacs-face-test-write-matrix-report)
                 (buffer-live-p (get-buffer "*Neomacs Face Test*"))
                 (buffer-name (window-buffer (selected-window))))"#,
        )
        .expect("parse startup load-option probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup load-option probe should evaluate");
        let items = list_to_vec(&result).expect("load-option result list");
        assert_eq!(items[0], Value::True);
        assert_eq!(items[1], Value::True);
        assert_eq!(
            print_value_with_eval(&mut eval, &items[2]),
            "\"*Neomacs Face Test*\""
        );
    }

    #[test]
    fn recursive_edit_processes_load_option_from_forwarded_args_before_first_input() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate lives in workspace root");
        let face_test = repo_root.join("test/neomacs/neomacs-face-test.el");
        let startup = gui_startup_with_args(&[
            "-Q",
            "-l",
            face_test
                .to_str()
                .expect("face test path must be valid utf-8"),
        ]);
        configure_gnu_startup_state(&mut eval, frame_id, &startup);

        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(neovm_core::keyboard::InputEvent::CloseRequested)
            .expect("queue close request");
        drop(tx);
        let mut wake_pipe = [0; 2];
        let pipe_result = unsafe { libc::pipe(wake_pipe.as_mut_ptr()) };
        assert_eq!(pipe_result, 0, "pipe should initialize");
        eval.init_input_system(rx, wake_pipe[0]);

        let result = eval.recursive_edit();
        unsafe {
            libc::close(wake_pipe[0]);
            libc::close(wake_pipe[1]);
        }
        let err = result.expect_err("close request should unwind recursive edit");
        assert!(
            err.contains("quit"),
            "close request should surface quit, got: {err}"
        );

        let forms = parse_forms(
            r#"(list
                 (fboundp 'neomacs-face-test-write-matrix-report)
                 (buffer-live-p (get-buffer "*Neomacs Face Test*"))
                 (buffer-name (window-buffer (selected-window))))"#,
        )
        .expect("parse recursive-edit load-option probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("recursive-edit load-option probe should evaluate");
        let items = list_to_vec(&result).expect("recursive-edit result list");
        assert_eq!(items[0], Value::True);
        assert_eq!(items[1], Value::True);
        assert_eq!(
            print_value_with_eval(&mut eval, &items[2]),
            "\"*Neomacs Face Test*\""
        );
    }

    #[test]
    fn gnu_startup_next_line_moves_point_on_live_gui_frame() {
        let mut eval = create_bootstrap_evaluator_with_features(BOOTSTRAP_CORE_FEATURES)
            .expect("bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());

        run_gnu_startup(&mut eval);

        let forms = parse_forms(
            r#"(progn
                 (erase-buffer)
                 (insert "abc\ndef\nghi")
                 (goto-char 1)
                 (command-execute 'next-line)
                 (point))"#,
        )
        .expect("parse startup next-line probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup next-line probe should evaluate");
        assert_eq!(result, Value::Int(5));
    }

    #[test]
    fn frame_set_background_mode_uses_live_gui_window_system_after_startup_clears_initial_flag() {
        let mut eval = create_bootstrap_evaluator_with_features(BOOTSTRAP_CORE_FEATURES)
            .expect("bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640, gui_display());
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id, &gui_startup());
        eval.set_variable("initial-window-system", Value::Nil);

        let forms = parse_forms(
            r#"(condition-case err
                  (progn
                    (frame-set-background-mode (selected-frame))
                    'ok)
                (error (list 'error (error-message-string err))))"#,
        )
        .expect("parse frame-set-background-mode probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("frame-set-background-mode probe should evaluate");
        assert_eq!(result, Value::symbol("ok"));
    }
}
