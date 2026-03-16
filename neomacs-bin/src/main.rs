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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use neomacs_display_runtime::FrameGlyphBuffer;
use neomacs_display_runtime::render_thread::{
    RenderThread, SharedImageDimensions, SharedMonitorInfo,
};
use neomacs_display_runtime::thread_comm::{RenderCommand, ThreadComms};

use neovm_core::buffer::BufferId;
use neovm_core::emacs_core::Value;
use neovm_core::emacs_core::error::EvalError;
use neovm_core::emacs_core::intern::resolve_sym;
use neovm_core::emacs_core::print_value_with_eval;
use neovm_core::emacs_core::{DisplayHost, Evaluator, GuiFrameHostRequest};
use neovm_core::window::{FrameId, Window};

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

    // If top-level returns (shouldn't normally happen), fall back to recursive-edit
    tracing::info!("Entering command loop (recursive-edit)");
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
    render_thread.join();
    tracing::info!("Neomacs exited cleanly");
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

fn bootstrap_buffers(eval: &mut Evaluator, width: u32, height: u32) -> BootstrapResult {
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
    let mini_h = 32.0_f32;
    let mini_y = height as f32 - mini_h;
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
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
        bootstrap_buffers, configure_gnu_startup_state, current_layout_frame_id, run_gnu_startup,
    };
    use neovm_core::emacs_core::Evaluator;
    use neovm_core::emacs_core::Value;
    use neovm_core::emacs_core::load::create_bootstrap_evaluator_cached_with_features;
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
}
