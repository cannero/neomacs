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

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use neomacs_display_runtime::FrameGlyphBuffer;
use neomacs_display_runtime::render_thread::{
    RenderThread, SharedImageDimensions, SharedMonitorInfo,
};
use neomacs_display_runtime::thread_comm::{RenderCommand, ThreadComms};
use neomacs_layout_engine::font_metrics::FontMetricsService;

use neovm_core::buffer::BufferId;
use neovm_core::emacs_core::Value;
use neovm_core::emacs_core::error::EvalError;
use neovm_core::emacs_core::intern::resolve_sym;
use neovm_core::emacs_core::print_value_with_eval;
use neovm_core::emacs_core::{DisplayHost, Evaluator, GuiFrameHostRequest};
use neovm_core::window::{FrameId, Window};

#[derive(Debug, Clone, PartialEq, Eq)]
enum EarlyCliAction {
    PrintHelp { program: String },
    PrintVersion,
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

struct PrimaryWindowDisplayHost {
    cmd_tx: crossbeam_channel::Sender<RenderCommand>,
    primary_window_adopted: bool,
}

impl DisplayHost for PrimaryWindowDisplayHost {
    fn realize_gui_frame(&mut self, request: GuiFrameHostRequest) -> Result<(), String> {
        if !self.primary_window_adopted {
            self.cmd_tx
                .send(RenderCommand::SetWindowTitle {
                    title: request.title,
                })
                .map_err(|err| format!("failed to update primary window title: {err}"))?;
            self.cmd_tx
                .send(RenderCommand::SetWindowSize {
                    width: request.width,
                    height: request.height,
                })
                .map_err(|err| format!("failed to update primary window size: {err}"))?;
            self.primary_window_adopted = true;
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

    // 1. Initialize logging
    neomacs_display_runtime::init_logging();

    tracing::info!(
        "Neomacs {} starting (pure Rust, backend={}, pid={})",
        neomacs_display_runtime::VERSION,
        neomacs_display_runtime::CORE_BACKEND,
        std::process::id()
    );

    // 2. Initialize the evaluator from the canonical core bootstrap.
    let mut evaluator =
        neovm_core::emacs_core::load::create_bootstrap_evaluator_cached_with_features(&["neomacs"])
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
    let width: u32 = 960;
    let height: u32 = 640;
    let _bootstrap = bootstrap_buffers(&mut evaluator, width, height);
    let frame_id = evaluator
        .frame_manager()
        .selected_frame()
        .expect("No selected frame after bootstrap")
        .id;
    configure_gnu_startup_state(&mut evaluator, frame_id);
    maybe_install_startup_phase_trace(&mut evaluator);

    // 4. Create communication channels — must happen BEFORE gnu startup,
    //    because `(eval top-level)` enters the command loop (infinite loop)
    //    which needs the display system to be running for input and redisplay.
    let comms = ThreadComms::new().expect("Failed to create thread comms");
    let (emacs_comms, render_comms) = comms.split();
    evaluator.set_display_host(Box::new(PrimaryWindowDisplayHost {
        cmd_tx: emacs_comms.cmd_tx.clone(),
        primary_window_adopted: false,
    }));

    // 5. Create shared state + spawn render thread
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
    );
    tracing::info!("Render thread spawned ({}x{})", width, height);

    // 6. Run initial layout and send first frame
    let mut frame_glyphs = FrameGlyphBuffer::with_size(width as f32, height as f32);
    run_layout(&mut evaluator, &mut frame_glyphs);
    let _ = emacs_comms.frame_tx.try_send(frame_glyphs.clone());
    tracing::info!("Initial frame sent ({} glyphs)", frame_glyphs.glyphs.len());

    // 7. Create input bridge: convert display runtime events → keyboard events
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    let display_input_rx = emacs_comms.input_rx;
    std::thread::Builder::new()
        .name("input-bridge".to_string())
        .spawn(move || {
            while let Ok(event) = display_input_rx.recv() {
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
        buf.undo_list.boundary();
    }

    // 10. Run GNU startup — this evaluates `(eval top-level)` which enters
    //     the command loop and blocks forever.  The render thread and input
    //     bridge must already be running before this point.
    tracing::info!("Running GNU startup (eval top-level)...");
    run_gnu_startup(&mut evaluator);
    tracing::info!("GNU startup returned (unexpected)");

    if evaluator.shutdown_request().is_none() {
        // If top-level returns without an explicit shutdown request
        // (shouldn't normally happen), fall back to recursive-edit.
        tracing::info!("Entering command loop (recursive-edit)");
        let exit_status = evaluator.recursive_edit();
        if exit_status.is_ok() {
            tracing::info!("Command loop exited normally");
        } else {
            tracing::warn!("Command loop exited with error");
        }
    } else {
        tracing::info!(
            "Skipping recursive-edit fallback because shutdown was requested: {:?}",
            evaluator.shutdown_request()
        );
    }

    // 11. Shutdown
    tracing::info!("Shutting down...");
    let _ = emacs_comms
        .cmd_tx
        .try_send(neomacs_display_runtime::thread_comm::RenderCommand::Shutdown);
    render_thread.join();
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

fn bootstrap_buffers(eval: &mut Evaluator, width: u32, height: u32) -> BootstrapResult {
    let frame_metrics = bootstrap_frame_metrics();

    // Create *scratch* buffer with initial content
    let scratch_id = eval.buffer_manager_mut().create_buffer("*scratch*");
    if let Some(buf) = eval.buffer_manager_mut().get_mut(scratch_id) {
        let content = ";; This buffer is for text that is not saved, and for Lisp evaluation.\n\
                       ;; To create a file, visit it with C-x C-f and enter text in its buffer.\n\n";
        buf.text.insert_str(0, content);
        let cc = buf.text.char_count();
        buf.begv = 0;
        buf.zv = cc;
        buf.pt = cc;
    }

    // Set *scratch* as the current buffer
    eval.buffer_manager_mut().set_current(scratch_id);

    // Create *Messages* buffer
    let msg_id = eval.buffer_manager_mut().create_buffer("*Messages*");
    if let Some(buf) = eval.buffer_manager_mut().get_mut(msg_id) {
        buf.begv = 0;
        buf.zv = 0;
        buf.pt = 0;
    }

    // Create *Minibuf-0*
    let mini_id = eval.buffer_manager_mut().create_buffer(" *Minibuf-0*");
    if let Some(buf) = eval.buffer_manager_mut().get_mut(mini_id) {
        buf.begv = 0;
        buf.zv = 0;
        buf.pt = 0;
    }

    // Create frame with *scratch* as the displayed buffer
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

    // Set window-system parameter
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        frame
            .parameters
            .insert("window-system".to_string(), Value::symbol("neomacs"));
        frame.title = "Neomacs".to_string();
        frame.font_pixel_size = frame_metrics.font_pixel_size;
        frame.char_width = frame_metrics.char_width;
        frame.char_height = frame_metrics.char_height;
        if let Window::Leaf {
            window_start,
            point,
            ..
        } = &mut frame.root_window
        {
            *window_start = 0;
            *point = 0;
        }
    }

    // Fix window geometry: root window takes frame height minus minibuffer.
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
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

fn configure_gnu_startup_state(eval: &mut Evaluator, frame_id: FrameId) {
    let argv = std::env::args().map(Value::string).collect::<Vec<_>>();
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
    eval.set_variable("command-line-args-left", Value::Nil);
    eval.set_variable("command-line-processed", Value::Nil);
    eval.set_variable("noninteractive", Value::Nil);
    eval.set_variable("window-system", Value::symbol("neomacs"));
    eval.set_variable("initial-window-system", Value::symbol("neomacs"));
    eval.set_variable("invocation-name", Value::string(invocation_name));
    eval.set_variable("invocation-directory", Value::string(invocation_directory));
    eval.set_variable("terminal-frame", Value::Frame(frame_id.0));
    eval.set_variable("frame-initial-frame", Value::Nil);
    eval.set_variable("default-minibuffer-frame", Value::Nil);
    // Skip the splash screen — its fill-region is extremely slow through
    // with_mirrored_evaluator.  Users who want it can set this to nil in
    // their init file.
    eval.set_variable("inhibit-startup-screen", Value::True);
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
            tracing::warn!(
                "GNU top-level startup signaled: {} {:?} (continuing anyway)",
                resolve_sym(symbol),
                decoded
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
        EarlyCliAction, bootstrap_buffers, bootstrap_frame_metrics, classify_early_cli_action,
        configure_gnu_startup_state, current_layout_frame_id, render_help_text,
        render_version_text, run_gnu_startup, run_layout,
    };
    use neomacs_display_runtime::FrameGlyphBuffer;
    use neomacs_display_runtime::core::frame_glyphs::{FrameGlyph, GlyphRowRole};
    use neovm_core::emacs_core::Evaluator;
    use neovm_core::emacs_core::Value;
    use neovm_core::emacs_core::load::{
        create_bootstrap_evaluator_cached_with_features, create_bootstrap_evaluator_with_features,
    };
    use neovm_core::emacs_core::parse_forms;
    use neovm_core::emacs_core::print_value_with_eval;
    use neovm_core::window::FrameId;

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
    fn configure_gnu_startup_state_marks_bootstrap_frame_as_terminal_frame() {
        let mut eval = Evaluator::new();
        configure_gnu_startup_state(&mut eval, FrameId(42));

        assert_eq!(
            eval.obarray().symbol_value("terminal-frame"),
            Some(&Value::Frame(42))
        );
        assert_eq!(
            eval.obarray().symbol_value("frame-initial-frame"),
            Some(&Value::Nil)
        );
        assert_eq!(
            eval.obarray().symbol_value("default-minibuffer-frame"),
            Some(&Value::Nil)
        );
    }

    #[test]
    fn bootstrap_buffers_seed_frame_with_renderer_metrics() {
        let metrics = bootstrap_frame_metrics();
        let mut eval = Evaluator::new();
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap");
        assert_eq!(frame.char_width, metrics.char_width);
        assert_eq!(frame.char_height, metrics.char_height);
        assert_eq!(frame.font_pixel_size, metrics.font_pixel_size);
        let minibuffer_height = frame
            .minibuffer_leaf
            .as_ref()
            .expect("minibuffer leaf")
            .bounds()
            .height;
        assert_eq!(minibuffer_height, metrics.char_height);
    }

    #[test]
    fn gnu_startup_keeps_scratch_selected_under_q_startup() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

        run_gnu_startup(&mut eval);

        let current = eval
            .buffer_manager()
            .current_buffer()
            .expect("current buffer after startup");
        assert_eq!(current.name, "*scratch*");
    }

    #[test]
    fn gnu_startup_preserves_default_fontset_alias() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

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
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

        run_gnu_startup(&mut eval);

        let forms = parse_forms("(list (current-message) (startup-echo-area-message))")
            .expect("parse startup echo probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup echo probe should evaluate");
        assert_eq!(
            print_value_with_eval(&mut eval, &result),
            "(\"For information about GNU Emacs and the GNU system, type C-h C-a.\" \"For information about GNU Emacs and the GNU system, type C-h C-a.\")"
        );
    }

    #[test]
    fn gnu_startup_keeps_single_row_minibuffer() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

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
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

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
    fn gnu_startup_renders_echo_message_into_minibuffer_row() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

        run_gnu_startup(&mut eval);

        let mut frame_glyphs = FrameGlyphBuffer::with_size(960.0, 640.0);
        run_layout(&mut eval, &mut frame_glyphs);

        let rendered: String = frame_glyphs
            .glyphs
            .iter()
            .filter_map(|glyph| match glyph {
                FrameGlyph::Char { row_role, char, .. }
                    if *row_role == GlyphRowRole::Minibuffer =>
                {
                    Some(*char)
                }
                _ => None,
            })
            .collect();

        assert!(
            rendered.contains("For information about GNU Emacs and the GNU system"),
            "expected startup echo message in minibuffer row, got: {rendered:?}"
        );
    }

    #[test]
    fn gnu_startup_restores_meta_and_ctl_x_bindings() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

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
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

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
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

        run_gnu_startup(&mut eval);

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
        assert_eq!(
            print_value_with_eval(&mut eval, &result),
            "(120 38 10 4 nil nil ok)"
        );
    }

    #[test]
    fn gnu_startup_split_window_below_succeeds_on_opening_frame() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

        run_gnu_startup(&mut eval);

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
        assert_eq!(
            print_value_with_eval(&mut eval, &result),
            "(120 38 10 4 nil nil ok)"
        );
    }

    #[test]
    fn gnu_startup_window_pixel_queries_use_live_frame_pixels() {
        let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
            .expect("cached bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

        run_gnu_startup(&mut eval);

        let forms = parse_forms(
            r#"(list
                 (window-pixel-width)
                 (window-pixel-height)
                 (window-body-width nil t)
                 (window-body-height nil t)
                 (window-text-width nil t)
                 (window-text-height nil t)
                 (window-edges nil nil nil t)
                 (window-edges nil t nil t))"#,
        )
        .expect("parse startup pixel probe");
        let result = eval
            .eval_expr(&forms[0])
            .expect("startup pixel probe should evaluate");
        assert_eq!(
            print_value_with_eval(&mut eval, &result),
            "(960 608 960 592 960 592 (0 0 960 608) (0 0 960 592))"
        );
    }

    #[test]
    fn gnu_startup_next_line_moves_point_on_live_gui_frame() {
        let mut eval =
            create_bootstrap_evaluator_with_features(&["neomacs"]).expect("bootstrap evaluator");
        let _bootstrap = bootstrap_buffers(&mut eval, 960, 640);
        let frame_id = eval
            .frame_manager()
            .selected_frame()
            .expect("selected frame after bootstrap")
            .id;
        configure_gnu_startup_state(&mut eval, frame_id);

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
}
