//! Neomacs — standalone Rust binary
//!
//! This is the pure-Rust entry point for Neomacs, bypassing the C Emacs
//! binary entirely.  It initializes the neovm-core Elisp evaluator,
//! creates the display/render thread, and runs a simple command loop:
//!
//!   key press → evaluator → buffer change → layout → render
//!
//! No C code is involved.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use crossbeam_channel::TryRecvError;

use neomacs_display::FrameGlyphBuffer;
use neomacs_display::render_thread::{RenderThread, SharedImageDimensions, SharedMonitorInfo};
use neomacs_display::thread_comm::InputEvent;
use neomacs_display::thread_comm::ThreadComms;

use neovm_core::buffer::BufferId;
use neovm_core::emacs_core::Evaluator;
use neovm_core::emacs_core::Value;
use neovm_core::window::{SplitDirection, Window, WindowId};

// Modifier bitmask constants (must match neomacs_display.h / thread_comm.rs)
const SHIFT_MASK: u32 = 1 << 0;
const CTRL_MASK: u32 = 1 << 1;
const META_MASK: u32 = 1 << 2;
#[allow(dead_code)]
const SUPER_MASK: u32 = 1 << 3;

// X11 keysym constants for special keys
const XK_RETURN: u32 = 0xFF0D;
const XK_TAB: u32 = 0xFF09;
const XK_BACKSPACE: u32 = 0xFF08;
const XK_DELETE: u32 = 0xFFFF;
const XK_ESCAPE: u32 = 0xFF1B;
const XK_LEFT: u32 = 0xFF51;
const XK_UP: u32 = 0xFF52;
const XK_RIGHT: u32 = 0xFF53;
const XK_DOWN: u32 = 0xFF54;
const XK_HOME: u32 = 0xFF50;
const XK_END: u32 = 0xFF57;
const XK_PAGE_UP: u32 = 0xFF55;
const XK_PAGE_DOWN: u32 = 0xFF56;

use std::cell::RefCell;

thread_local! {
    /// Bookmarks: name -> (buffer_name, position)
    static BOOKMARKS: RefCell<HashMap<String, (String, usize)>> =
        RefCell::new(HashMap::new());
    /// Registers: char -> (buffer_name, position)  or text content
    static REGISTERS: RefCell<HashMap<char, RegisterEntry>> =
        RefCell::new(HashMap::new());
}

#[derive(Clone)]
enum RegisterEntry {
    Position { buffer_name: String, pos: usize },
    Text(String),
}

fn main() {
    // Initialize logging
    neomacs_display::init_logging();

    tracing::info!(
        "Neomacs {} starting (pure Rust, backend={})",
        neomacs_display::VERSION,
        neomacs_display::CORE_BACKEND
    );

    // 1. Initialize the evaluator
    let mut evaluator = Evaluator::new();
    evaluator.setup_thread_locals();
    // Release-mode stack frames are smaller than debug-mode, so we can
    // safely raise the eval recursion limit above the test-safe default.
    evaluator.set_max_depth(1600);
    tracing::info!("Evaluator initialized");

    // 2. Parse command-line arguments
    let args = parse_args();

    // 3. Bootstrap: create *scratch*, *Messages*, *Minibuf-0* buffers
    let width: u32 = 960;
    let height: u32 = 640;
    let bootstrap = bootstrap_buffers(&mut evaluator, width, height);
    let scratch_id = bootstrap.scratch_id;

    // Set a useful mode-line-format with %-constructs
    // %* = modified indicator, %b = buffer name, %l = line, %c = column
    evaluator.set_variable(
        "mode-line-format",
        Value::string(" %*%+ %b   L%l C%c   %f "),
    );

    tracing::info!("Bootstrap complete: *scratch* buffer={:?}", scratch_id);

    // 3.5. Set up load-path and load core Elisp files
    setup_load_path(&mut evaluator);
    load_core_elisp(&mut evaluator);

    // 4. Load Elisp files specified on the command line
    for load_item in &args.load {
        match load_item {
            LoadItem::File(path) => {
                tracing::info!("Loading Elisp file: {}", path.display());
                evaluator.setup_thread_locals();
                match neovm_core::emacs_core::load::load_file(&mut evaluator, path) {
                    Ok(_) => tracing::info!("  Loaded: {}", path.display()),
                    Err(e) => tracing::error!("  Error loading {}: {:?}", path.display(), e),
                }
            }
            LoadItem::Eval(expr) => {
                tracing::info!("Evaluating: {}", expr);
                evaluator.setup_thread_locals();
                match neovm_core::emacs_core::parse_forms(expr) {
                    Ok(forms) => {
                        for form in &forms {
                            match evaluator.eval_expr(form) {
                                Ok(val) => tracing::info!("  => {:?}", val),
                                Err(e) => tracing::error!("  Error: {:?}", e),
                            }
                        }
                    }
                    Err(e) => tracing::error!("  Parse error: {}", e),
                }
            }
        }
    }

    // Open files specified on the command line
    for file_path in &args.files {
        open_file(&mut evaluator, file_path, scratch_id);
    }

    // Add undo boundary after bootstrap so initial content isn't undoable
    if let Some(buf) = evaluator.buffer_manager_mut().current_buffer_mut() {
        buf.undo_list.boundary();
    }

    // 5. Create communication channels
    let comms = ThreadComms::new().expect("Failed to create thread comms");
    let (emacs_comms, render_comms) = comms.split();

    // 4. Create shared state
    let image_dimensions: SharedImageDimensions = Arc::new(Mutex::new(HashMap::new()));
    let shared_monitors: SharedMonitorInfo =
        Arc::new((Mutex::new(Vec::new()), std::sync::Condvar::new()));

    // 5. Spawn render thread
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
    let frame_id = evaluator
        .frame_manager()
        .selected_frame()
        .expect("No selected frame after bootstrap")
        .id;

    let mut frame_glyphs = FrameGlyphBuffer::with_size(width as f32, height as f32);
    run_layout(&mut evaluator, frame_id, &mut frame_glyphs);
    let _ = emacs_comms.frame_tx.try_send(frame_glyphs.clone());
    tracing::info!("Initial frame sent ({} glyphs)", frame_glyphs.glyphs.len());

    // 7. Main event loop
    let wakeup_fd = emacs_comms.wakeup_read_fd;
    let mut running = true;
    let mut prefix_state = PrefixState::None;
    let mut kmacro = MacroState {
        recording: false,
        events: Vec::new(),
        last_macro: Vec::new(),
    };
    let mut minibuf = MinibufferState {
        active: false,
        prompt: String::new(),
        input: String::new(),
        action: MinibufferAction::FindFile,
        prev_selected: WindowId(0),
        minibuf_id: bootstrap.minibuf_id,
        search_origin: 0,
        quit_requested: false,
        history: Vec::new(),
        history_pos: None,
        saved_input: String::new(),
    };

    let mut last_auto_save = std::time::Instant::now();
    let auto_save_interval = std::time::Duration::from_secs(30);
    let mut keystroke_count: u32 = 0;

    while running {
        // Auto-save: save modified file buffers every 30s or 300 keystrokes
        if (last_auto_save.elapsed() >= auto_save_interval && keystroke_count > 0)
            || keystroke_count >= 300
        {
            auto_save_buffers(&evaluator);
            last_auto_save = std::time::Instant::now();
            keystroke_count = 0;
        }

        // Wait for events using poll() on the wakeup fd
        wait_for_wakeup(wakeup_fd);

        // Clear wakeup pipe
        emacs_comms.wakeup_clear.clear();

        // Drain all pending input events
        let mut need_redisplay = false;
        loop {
            match emacs_comms.input_rx.try_recv() {
                Ok(event) => {
                    match event {
                        InputEvent::Key {
                            keysym,
                            modifiers,
                            pressed,
                        } => {
                            if pressed {
                                keystroke_count += 1;
                                if minibuf.active {
                                    // Keys go to the minibuffer handler
                                    match handle_minibuffer_key(
                                        &mut evaluator,
                                        keysym,
                                        modifiers,
                                        &mut minibuf,
                                        scratch_id,
                                    ) {
                                        KeyResult::Handled => need_redisplay = true,
                                        KeyResult::Quit => {
                                            // C-g cancels the minibuffer
                                            cancel_minibuffer(&mut evaluator, &mut minibuf);
                                            need_redisplay = true;
                                        }
                                        KeyResult::Ignored => {}
                                        KeyResult::Save => {}
                                    }
                                    // Check if a minibuffer action requested quit
                                    if minibuf.quit_requested {
                                        running = false;
                                    }
                                } else {
                                    // Record key for keyboard macro (except macro control keys)
                                    let is_macro_key =
                                        is_macro_control_key(keysym, modifiers, &prefix_state);
                                    match handle_key(
                                        &mut evaluator,
                                        keysym,
                                        modifiers,
                                        &mut prefix_state,
                                        &mut minibuf,
                                        &mut kmacro,
                                    ) {
                                        KeyResult::Handled => {
                                            if kmacro.recording && !is_macro_key {
                                                kmacro.events.push((keysym, modifiers));
                                            }
                                            need_redisplay = true;
                                        }
                                        KeyResult::Quit => {
                                            tracing::info!("C-x C-c: quit requested");
                                            running = false;
                                        }
                                        KeyResult::Save => {
                                            if kmacro.recording && !is_macro_key {
                                                kmacro.events.push((keysym, modifiers));
                                            }
                                            delete_trailing_whitespace(&mut evaluator);
                                            save_current_buffer(&evaluator);
                                            // Mark buffer as not modified after save
                                            if let Some(buf) =
                                                evaluator.buffer_manager_mut().current_buffer_mut()
                                            {
                                                buf.modified = false;
                                            }
                                            need_redisplay = true;
                                        }
                                        KeyResult::Ignored => {
                                            // Still record prefix keys
                                            if kmacro.recording && !is_macro_key {
                                                kmacro.events.push((keysym, modifiers));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        InputEvent::WindowClose { .. } => {
                            tracing::info!("Window close requested");
                            running = false;
                        }
                        InputEvent::WindowResize {
                            width: w,
                            height: h,
                            ..
                        } => {
                            tracing::info!("Window resized to {}x{}", w, h);
                            let mini_h = 32.0_f32;
                            let mini_y = h as f32 - mini_h;
                            if let Some(frame) = evaluator.frame_manager_mut().selected_frame_mut()
                            {
                                let old_w = frame.width as f32;
                                let old_h = frame.height as f32 - mini_h; // old text area height
                                frame.width = w;
                                frame.height = h;
                                let new_w = w as f32;
                                let new_h = mini_y; // new text area height
                                // Recursively resize the window tree
                                resize_window_tree(
                                    &mut frame.root_window,
                                    0.0,
                                    0.0,
                                    new_w,
                                    new_h,
                                    old_w,
                                    old_h,
                                );
                                // Reposition minibuffer
                                if let Some(mini_leaf) = &mut frame.minibuffer_leaf {
                                    if let Window::Leaf { bounds, .. } = mini_leaf {
                                        bounds.y = mini_y;
                                        bounds.width = w as f32;
                                        bounds.height = mini_h;
                                    }
                                }
                            }
                            frame_glyphs = FrameGlyphBuffer::with_size(w as f32, h as f32);
                            need_redisplay = true;
                        }
                        InputEvent::MouseButton {
                            button,
                            x,
                            y,
                            pressed,
                            ..
                        } => {
                            if pressed && button == 1 {
                                // Left click: set point in the clicked window
                                handle_mouse_click(&mut evaluator, x, y);
                                need_redisplay = true;
                            }
                        }
                        InputEvent::MouseScroll { delta_y, x, y, .. } => {
                            handle_mouse_scroll(&mut evaluator, delta_y, x, y);
                            need_redisplay = true;
                        }
                        _ => {
                            // Ignore other events (focus, mouse move, etc.)
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    tracing::error!("Render thread disconnected");
                    running = false;
                    break;
                }
            }
        }

        // Redisplay if anything changed
        if need_redisplay {
            // Add undo boundary after each command so undo can pop one group at a time
            if let Some(buf) = evaluator.buffer_manager_mut().current_buffer_mut() {
                buf.undo_list.boundary();
            }
            // Re-fontify if buffer was modified (incremental syntax highlighting)
            {
                let needs_fontify = evaluator
                    .buffer_manager()
                    .current_buffer()
                    .map(|b| b.modified && b.file_name.is_some())
                    .unwrap_or(false);
                if needs_fontify {
                    fontify_buffer(&mut evaluator);
                }
            }
            // Update overlays: region highlight + paren matching
            highlight_region(&mut evaluator);
            highlight_matching_parens(&mut evaluator);
            evaluator.setup_thread_locals();
            run_layout(&mut evaluator, frame_id, &mut frame_glyphs);
            let _ = emacs_comms.frame_tx.try_send(frame_glyphs.clone());
        }
    }

    // Shutdown
    tracing::info!("Shutting down...");
    let _ = emacs_comms
        .cmd_tx
        .try_send(neomacs_display::thread_comm::RenderCommand::Shutdown);
    render_thread.join();
    tracing::info!("Neomacs exited cleanly");
}

/// Bootstrap result containing key buffer IDs.
struct BootstrapResult {
    scratch_id: BufferId,
    minibuf_id: BufferId,
}

/// Create initial buffers and frame in the evaluator.
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

    // Set *scratch* as the current buffer so self-insert-command works
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

    // Set window positions (0-based for neovm-core)
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
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
    // Give the minibuffer 2 lines (32px) so text is clearly visible.
    let mini_h = 32.0_f32;
    let mini_y = height as f32 - mini_h;
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        // Shrink root window to leave room for minibuffer
        if let Window::Leaf { bounds, .. } = &mut frame.root_window {
            bounds.height = mini_y;
        }
        // Point the minibuffer leaf to *Minibuf-0* and set correct position
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

/// Run the layout engine on the current frame state.
fn run_layout(
    evaluator: &mut Evaluator,
    frame_id: neovm_core::window::FrameId,
    frame_glyphs: &mut FrameGlyphBuffer,
) {
    use neomacs_display::layout::LayoutEngine;

    // Use a thread-local layout engine
    thread_local! {
        static ENGINE: std::cell::RefCell<LayoutEngine> = std::cell::RefCell::new(LayoutEngine::new());
    }

    ENGINE.with(|engine| {
        engine
            .borrow_mut()
            .layout_frame_rust(evaluator, frame_id, frame_glyphs);
    });
}

/// Result of key handling.
enum KeyResult {
    /// Key was handled, buffer may have changed.
    Handled,
    /// Key was not handled / no change.
    Ignored,
    /// C-x C-c: quit the editor.
    Quit,
    /// C-x C-s: save the current buffer.
    Save,
}

/// Prefix key state machine.
#[derive(Debug)]
enum PrefixState {
    None,
    CtrlX,
    CtrlXR,
    MetaG,
}

/// Keyboard macro state.
struct MacroState {
    /// Whether we are currently recording.
    recording: bool,
    /// Recorded key events (keysym, modifiers).
    events: Vec<(u32, u32)>,
    /// Last completed macro.
    last_macro: Vec<(u32, u32)>,
}

/// What action the minibuffer is being used for.
enum MinibufferAction {
    /// C-x C-f: find-file — open a file
    FindFile,
    /// C-x b: switch-to-buffer
    SwitchBuffer,
    /// C-s: incremental search forward
    SearchForward,
    /// C-r: incremental search backward
    SearchBackward,
    /// M-z: zap-to-char (prompt for character)
    ZapToChar,
    /// M-x: execute command by name
    ExecuteCommand,
    /// M-g g: goto-line
    GotoLine,
    /// M-%: replace string (first prompt — search string)
    ReplaceFrom,
    /// M-%: replace string (second prompt — replacement string)
    ReplaceTo { from: String },
    /// C-x C-c: confirm quit when modified buffers exist
    ConfirmQuit,
    /// C-x C-w: write-file (save-as)
    WriteFile,
    /// M-x occur: show matching lines
    Occur,
    /// Shell command on region
    ShellCommand,
    /// M-x compile: run compilation command
    Compile,
    /// M-x grep: run grep command
    GrepCmd,
    /// C-x r m: set bookmark
    SetBookmark,
    /// C-x r b: jump to bookmark
    JumpBookmark,
    /// C-x r SPC: point to register
    PointToRegister,
    /// C-x r j: jump to register
    JumpToRegister,
}

/// Minibuffer interaction state.
struct MinibufferState {
    /// Whether the minibuffer is active.
    active: bool,
    /// The prompt displayed to the user.
    prompt: String,
    /// The user's input so far.
    input: String,
    /// What action to take when Enter is pressed.
    action: MinibufferAction,
    /// The previously selected window, to restore on cancel.
    prev_selected: WindowId,
    /// The minibuffer buffer id.
    minibuf_id: BufferId,
    /// Saved point for incremental search (to reset on each keystroke).
    search_origin: usize,
    /// Set to true when a minibuffer action (e.g. ConfirmQuit) wants to quit the editor.
    quit_requested: bool,
    /// History of minibuffer inputs (most recent last).
    history: Vec<String>,
    /// Current position in history (None = editing new input).
    history_pos: Option<usize>,
    /// Saved input before navigating history.
    saved_input: String,
}

/// Check if a key is a macro control key (start/stop/play — don't record these).
fn is_macro_control_key(keysym: u32, _modifiers: u32, prefix: &PrefixState) -> bool {
    if let PrefixState::CtrlX = prefix {
        // C-x ( , C-x ) , C-x e are macro controls
        matches!(keysym, 0x28 | 0x29 | 0x65)
    } else {
        false
    }
}

/// Handle a key event, returning a `KeyResult`.
fn handle_key(
    eval: &mut Evaluator,
    keysym: u32,
    modifiers: u32,
    prefix: &mut PrefixState,
    minibuf: &mut MinibufferState,
    kmacro: &mut MacroState,
) -> KeyResult {
    eval.setup_thread_locals();

    // Handle prefix key sequences first
    if let PrefixState::CtrlX = prefix {
        *prefix = PrefixState::None;
        // Handle macro keys before passing to handle_cx_key
        match keysym {
            0x28 => {
                // '(' — start recording
                kmacro.recording = true;
                kmacro.events.clear();
                tracing::info!("Keyboard macro recording started");
                return KeyResult::Handled;
            }
            0x29 => {
                // ')' — stop recording
                if kmacro.recording {
                    kmacro.recording = false;
                    kmacro.last_macro = kmacro.events.clone();
                    kmacro.events.clear();
                    tracing::info!(
                        "Keyboard macro recorded ({} events)",
                        kmacro.last_macro.len()
                    );
                }
                return KeyResult::Handled;
            }
            _ => {}
        }
        // C-x r → enter register/bookmark prefix
        if keysym == 0x72 && (modifiers & CTRL_MASK) == 0 {
            *prefix = PrefixState::CtrlXR;
            return KeyResult::Ignored;
        }
        return handle_cx_key(eval, keysym, modifiers, minibuf, kmacro);
    }
    if let PrefixState::CtrlXR = prefix {
        *prefix = PrefixState::None;
        return handle_cxr_key(eval, keysym, modifiers, minibuf);
    }
    if let PrefixState::MetaG = prefix {
        *prefix = PrefixState::None;
        return handle_mg_key(eval, keysym, modifiers, minibuf);
    }

    // Check for C-x prefix
    if keysym == 0x78 && (modifiers & CTRL_MASK) != 0 && (modifiers & !CTRL_MASK & !SHIFT_MASK) == 0
    {
        *prefix = PrefixState::CtrlX;
        return KeyResult::Ignored;
    }

    // Determine the command to execute
    let command = match (keysym, modifiers) {
        // C-/ (slash = 0x2f): undo
        (0x2f, mods) if (mods & CTRL_MASK) != 0 => {
            undo(eval);
            return KeyResult::Handled;
        }

        // C-_ (underscore = 0x5f): also undo (Emacs convention)
        (0x5f, mods) if (mods & CTRL_MASK) != 0 => {
            undo(eval);
            return KeyResult::Handled;
        }

        // Printable ASCII without modifiers (or shift-only for uppercase)
        (32..=126, mods) if (mods & !SHIFT_MASK) == 0 => {
            let ch = (keysym as u8) as char;
            // Electric pair: auto-close brackets
            let closing = match ch {
                '(' => Some(')'),
                '[' => Some(']'),
                '{' => Some('}'),
                '"' => Some('"'),
                '\'' => Some('\''),
                _ => None,
            };
            // Skip closing if we're typing a closing bracket that already matches
            if matches!(ch, ')' | ']' | '}') {
                if let Some(buf) = eval.buffer_manager().current_buffer() {
                    let text = buf.text.to_string();
                    if buf.pt < text.len() && text.as_bytes()[buf.pt] == ch as u8 {
                        // Just move past the existing closing bracket
                        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
                            buf.pt += 1;
                        }
                        return KeyResult::Handled;
                    }
                }
            }
            eval.set_variable("last-command-event", Value::Int(keysym as i64));
            if let Some(close) = closing {
                // Insert the char + closing, then move cursor back before closing
                if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
                    let pt = buf.pt;
                    let pair = format!("{}{}", ch, close);
                    buf.text.insert_str(pt, &pair);
                    buf.zv = buf.text.char_count();
                    buf.pt = pt + ch.len_utf8(); // cursor between pair
                    buf.modified = true;
                }
                return KeyResult::Handled;
            }
            "(self-insert-command 1)"
        }

        // Return → newline with auto-indent
        (XK_RETURN, 0) => {
            newline_and_indent(eval);
            return KeyResult::Handled;
        }

        // Tab → indent region or insert spaces
        (XK_TAB, 0) => {
            let has_mark = eval
                .buffer_manager()
                .current_buffer()
                .and_then(|b| b.mark)
                .is_some();
            if has_mark {
                indent_region(eval, true);
            } else {
                indent_for_tab(eval);
            }
            return KeyResult::Handled;
        }
        // Shift-Tab (backtab) → dedent region or line
        (0xFE20, _) => {
            indent_region(eval, false);
            return KeyResult::Handled;
        }

        // Backspace → delete-backward-char
        (XK_BACKSPACE, 0) => "(delete-backward-char 1)",

        // M-Backspace → backward-kill-word
        (XK_BACKSPACE, mods) if (mods & META_MASK) != 0 => {
            backward_kill_word(eval);
            return KeyResult::Handled;
        }

        // Delete → delete-char
        (XK_DELETE, 0) => "(delete-char 1)",

        // Arrow keys
        (XK_LEFT, 0) => "(backward-char 1)",
        (XK_RIGHT, 0) => "(forward-char 1)",
        (XK_UP, 0) => "(previous-line 1)",
        (XK_DOWN, 0) => "(next-line 1)",

        // Shift+arrows: extend selection
        (XK_LEFT, mods) if mods == SHIFT_MASK => {
            shift_select_start(eval);
            exec_command(eval, "(backward-char 1)");
            return KeyResult::Handled;
        }
        (XK_RIGHT, mods) if mods == SHIFT_MASK => {
            shift_select_start(eval);
            exec_command(eval, "(forward-char 1)");
            return KeyResult::Handled;
        }
        (XK_UP, mods) if mods == SHIFT_MASK => {
            shift_select_start(eval);
            exec_command(eval, "(previous-line 1)");
            return KeyResult::Handled;
        }
        (XK_DOWN, mods) if mods == SHIFT_MASK => {
            shift_select_start(eval);
            exec_command(eval, "(next-line 1)");
            return KeyResult::Handled;
        }
        // Shift+Home/End
        (XK_HOME, mods) if mods == SHIFT_MASK => {
            shift_select_start(eval);
            exec_command(eval, "(beginning-of-line)");
            return KeyResult::Handled;
        }
        (XK_END, mods) if mods == SHIFT_MASK => {
            shift_select_start(eval);
            exec_command(eval, "(end-of-line)");
            return KeyResult::Handled;
        }

        // C-Left/Right: word movement
        (XK_LEFT, mods) if (mods & CTRL_MASK) != 0 && (mods & SHIFT_MASK) == 0 => {
            return exec_command(eval, "(backward-word 1)");
        }
        (XK_RIGHT, mods) if (mods & CTRL_MASK) != 0 && (mods & SHIFT_MASK) == 0 => {
            return exec_command(eval, "(forward-word 1)");
        }
        // C-Shift-Left/Right: word movement with selection
        (XK_LEFT, mods)
            if (mods & CTRL_MASK) != 0 && (mods & SHIFT_MASK) != 0 && (mods & META_MASK) == 0 =>
        {
            shift_select_start(eval);
            exec_command(eval, "(backward-word 1)");
            return KeyResult::Handled;
        }
        (XK_RIGHT, mods)
            if (mods & CTRL_MASK) != 0 && (mods & SHIFT_MASK) != 0 && (mods & META_MASK) == 0 =>
        {
            shift_select_start(eval);
            exec_command(eval, "(forward-word 1)");
            return KeyResult::Handled;
        }
        // C-Delete: kill word forward
        (XK_DELETE, mods) if (mods & CTRL_MASK) != 0 => {
            return exec_command(eval, "(kill-word 1)");
        }
        // C-Backspace: kill word backward
        (XK_BACKSPACE, mods) if (mods & CTRL_MASK) != 0 => {
            backward_kill_word(eval);
            return KeyResult::Handled;
        }

        // M-Up/M-Down: move line up/down
        (XK_UP, mods) if (mods & META_MASK) != 0 => {
            move_line_up(eval);
            return KeyResult::Handled;
        }
        (XK_DOWN, mods) if (mods & META_MASK) != 0 => {
            move_line_down(eval);
            return KeyResult::Handled;
        }

        // C-Shift-Up/Down: duplicate line up/down
        (XK_UP, mods) if (mods & CTRL_MASK) != 0 && (mods & SHIFT_MASK) != 0 => {
            duplicate_line(eval, false);
            return KeyResult::Handled;
        }
        (XK_DOWN, mods) if (mods & CTRL_MASK) != 0 && (mods & SHIFT_MASK) != 0 => {
            duplicate_line(eval, true);
            return KeyResult::Handled;
        }

        // Home → smart home (first non-whitespace, or column 0)
        (XK_HOME, 0) => {
            smart_home(eval);
            return KeyResult::Handled;
        }
        (XK_END, 0) => "(end-of-line)",

        // C-Home / C-End: beginning/end of buffer
        (XK_HOME, mods) if (mods & CTRL_MASK) != 0 => "(beginning-of-buffer)",
        (XK_END, mods) if (mods & CTRL_MASK) != 0 => "(end-of-buffer)",

        // PageUp/PageDown
        (XK_PAGE_UP, 0) => {
            scroll_down(eval);
            return KeyResult::Handled;
        }
        (XK_PAGE_DOWN, 0) => {
            scroll_up(eval);
            return KeyResult::Handled;
        }

        // C-SPC / C-@: set mark
        (0x20, mods) if (mods & CTRL_MASK) != 0 => {
            set_mark_command(eval);
            return KeyResult::Handled;
        }
        // C-@ (Ctrl+Shift+2): also set mark (equivalent to C-SPC)
        (0x40, mods) if (mods & CTRL_MASK) != 0 => {
            set_mark_command(eval);
            return KeyResult::Handled;
        }

        // C-] → goto matching paren/bracket
        (0x5d, mods) if (mods & CTRL_MASK) != 0 => {
            goto_matching_paren(eval);
            return KeyResult::Handled;
        }

        // C-a through C-z (except C-x which was handled above)
        (key, mods)
            if (mods & CTRL_MASK) != 0
                && (mods & !CTRL_MASK & !SHIFT_MASK) == 0
                && (0x61..=0x7A).contains(&key) =>
        {
            match (key as u8) as char {
                'a' => "(beginning-of-line)",
                'b' => "(backward-char 1)",
                'd' => "(delete-char 1)",
                'e' => "(end-of-line)",
                'f' => "(forward-char 1)",
                'g' => {
                    // C-g: deactivate mark
                    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
                        buf.mark = None;
                    }
                    return KeyResult::Ignored;
                }
                'r' => {
                    activate_minibuffer(
                        eval,
                        minibuf,
                        "I-search backward: ",
                        MinibufferAction::SearchBackward,
                    );
                    // Save origin for backward search
                    if let Some(buf) = eval.buffer_manager().current_buffer() {
                        minibuf.search_origin = buf.pt;
                    }
                    return KeyResult::Handled;
                }
                's' => {
                    activate_minibuffer(
                        eval,
                        minibuf,
                        "I-search: ",
                        MinibufferAction::SearchForward,
                    );
                    return KeyResult::Handled;
                }
                'k' => {
                    if (mods & SHIFT_MASK) != 0 {
                        delete_whole_line(eval);
                    } else {
                        kill_line(eval);
                    }
                    return KeyResult::Handled;
                }
                'h' => {
                    show_help(eval);
                    return KeyResult::Handled;
                }
                'l' => {
                    recenter(eval);
                    return KeyResult::Handled;
                }
                't' => {
                    transpose_chars(eval);
                    return KeyResult::Handled;
                }
                'n' => "(next-line 1)",
                'o' => "(open-line 1)",
                'p' => "(previous-line 1)",
                'v' => {
                    scroll_up(eval);
                    return KeyResult::Handled;
                }
                'w' => {
                    kill_region(eval);
                    return KeyResult::Handled;
                }
                'y' => {
                    yank(eval);
                    return KeyResult::Handled;
                }
                _ => {
                    tracing::debug!("Unhandled C-{}", (key as u8) as char);
                    return KeyResult::Ignored;
                }
            }
        }

        // M-< (keysym 0x3c '<'): beginning-of-buffer
        (0x3c, mods) if (mods & META_MASK) != 0 => {
            return exec_command(eval, "(beginning-of-buffer)");
        }

        // M-> (keysym 0x3e '>'): end-of-buffer
        (0x3e, mods) if (mods & META_MASK) != 0 => {
            return exec_command(eval, "(end-of-buffer)");
        }

        // M-= (keysym 0x3d '='): count-words-region
        (0x3d, mods) if (mods & META_MASK) != 0 => {
            count_words_region(eval);
            return KeyResult::Handled;
        }

        // M-% (keysym 0x25 '%'): replace-string
        (0x25, mods) if (mods & META_MASK) != 0 => {
            activate_minibuffer(eval, minibuf, "Replace: ", MinibufferAction::ReplaceFrom);
            return KeyResult::Handled;
        }

        // M-/ (keysym 0x2f '/'): dabbrev-expand
        (0x2f, mods) if (mods & META_MASK) != 0 => {
            dabbrev_expand(eval);
            return KeyResult::Handled;
        }

        // M-; (keysym 0x3b ';'): comment/uncomment line
        (0x3b, mods) if (mods & META_MASK) != 0 => {
            toggle_comment_line(eval);
            return KeyResult::Handled;
        }

        // M-^ (keysym 0x5e '^'): join-line
        (0x5e, mods) if (mods & META_MASK) != 0 => {
            join_line(eval);
            return KeyResult::Handled;
        }

        // M-key (meta + letter)
        (key, mods)
            if (mods & META_MASK) != 0
                && (mods & !META_MASK & !SHIFT_MASK) == 0
                && (0x61..=0x7A).contains(&key) =>
        {
            match (key as u8) as char {
                'f' => "(forward-word 1)",
                'b' => "(backward-word 1)",
                'd' => "(kill-word 1)",
                'h' => {
                    mark_paragraph(eval);
                    return KeyResult::Handled;
                }
                'c' => {
                    capitalize_word(eval);
                    return KeyResult::Handled;
                }
                'g' => {
                    *prefix = PrefixState::MetaG;
                    return KeyResult::Ignored;
                }
                'l' => {
                    case_word(eval, false);
                    return KeyResult::Handled;
                }
                'q' => {
                    fill_paragraph(eval);
                    return KeyResult::Handled;
                }
                'u' => {
                    case_word(eval, true);
                    return KeyResult::Handled;
                }
                'v' => {
                    scroll_down(eval);
                    return KeyResult::Handled;
                }
                'w' => {
                    copy_region_as_kill(eval);
                    return KeyResult::Handled;
                }
                'x' => {
                    activate_minibuffer(eval, minibuf, "M-x ", MinibufferAction::ExecuteCommand);
                    return KeyResult::Handled;
                }
                'y' => {
                    yank_pop(eval);
                    return KeyResult::Handled;
                }
                'z' => {
                    activate_minibuffer(
                        eval,
                        minibuf,
                        "Zap to char: ",
                        MinibufferAction::ZapToChar,
                    );
                    return KeyResult::Handled;
                }
                _ => {
                    tracing::debug!("Unhandled M-{}", (key as u8) as char);
                    return KeyResult::Ignored;
                }
            }
        }

        // Unicode characters (non-ASCII, no modifiers) → self-insert
        (key, 0) if key >= 0x100 && key < 0xFF00 => {
            eval.set_variable("last-command-event", Value::Int(keysym as i64));
            "(self-insert-command 1)"
        }

        // Escape — ignore
        (XK_ESCAPE, 0) => return KeyResult::Ignored,

        _ => {
            tracing::debug!("Unhandled keysym=0x{:X} mods=0x{:X}", keysym, modifiers);
            return KeyResult::Ignored;
        }
    };

    exec_command(eval, command)
}

/// Handle keys after C-x prefix.
fn handle_cx_key(
    eval: &mut Evaluator,
    keysym: u32,
    modifiers: u32,
    minibuf: &mut MinibufferState,
    kmacro: &mut MacroState,
) -> KeyResult {
    let is_ctrl = (modifiers & CTRL_MASK) != 0;
    let key_char = if (0x61..=0x7A).contains(&keysym) || (0x30..=0x39).contains(&keysym) {
        Some((keysym as u8) as char)
    } else {
        None
    };

    match (key_char, is_ctrl) {
        // C-x C-c → quit (with confirmation if modified buffers exist)
        (Some('c'), true) => {
            // Check for modified buffers
            let has_modified = eval
                .buffer_manager()
                .buffer_list()
                .into_iter()
                .filter_map(|id| eval.buffer_manager().get(id))
                .filter(|b| !b.name.starts_with(' ') && !b.name.starts_with('*'))
                .any(|b| b.modified && b.file_name.is_some());
            if has_modified {
                activate_minibuffer(
                    eval,
                    minibuf,
                    "Modified buffers exist; exit anyway? (yes or no) ",
                    MinibufferAction::ConfirmQuit,
                );
                KeyResult::Handled
            } else {
                KeyResult::Quit
            }
        }
        // C-x C-s → save
        (Some('s'), true) => KeyResult::Save,
        // C-x C-f → find-file (minibuffer prompt)
        (Some('f'), true) => {
            activate_minibuffer(eval, minibuf, "Find file: ", MinibufferAction::FindFile);
            KeyResult::Handled
        }
        // C-x C-w → write-file (save-as with filename prompt)
        (Some('w'), true) => {
            let default = eval
                .buffer_manager()
                .current_buffer()
                .and_then(|b| b.file_name.clone())
                .unwrap_or_default();
            activate_minibuffer(eval, minibuf, "Write file: ", MinibufferAction::WriteFile);
            // Pre-fill with current file name
            if !default.is_empty() {
                minibuf.input = default;
                update_minibuffer_display(eval, minibuf);
            }
            KeyResult::Handled
        }
        // C-x C-b → list-buffers
        (Some('b'), true) => {
            list_buffers(eval);
            KeyResult::Handled
        }
        // C-x b → switch-to-buffer (minibuffer prompt)
        (Some('b'), false) => {
            activate_minibuffer(
                eval,
                minibuf,
                "Switch to buffer: ",
                MinibufferAction::SwitchBuffer,
            );
            KeyResult::Handled
        }
        // C-x h → mark-whole-buffer
        (Some('h'), false) => {
            mark_whole_buffer(eval);
            KeyResult::Handled
        }
        // C-x C-x → exchange-point-and-mark
        (Some('x'), true) => {
            exchange_point_and_mark(eval);
            KeyResult::Handled
        }
        // C-x u → undo
        (Some('u'), false) => exec_command(eval, "(undo)"),
        // C-x k → kill-buffer (switch to previous buffer or *scratch*)
        (Some('k'), false) => {
            kill_current_buffer(eval);
            KeyResult::Handled
        }
        // C-x C-l → lowercase region
        (Some('l'), true) => {
            transform_region(eval, |s| s.to_lowercase());
            KeyResult::Handled
        }
        // C-x C-u → uppercase region
        (Some('u'), true) => {
            transform_region(eval, |s| s.to_uppercase());
            KeyResult::Handled
        }
        // C-x = → what-cursor-position
        (None, false) if keysym == 0x3d => {
            what_cursor_position(eval);
            KeyResult::Handled
        }
        // C-x C-o → delete blank lines
        (Some('o'), true) => {
            delete_blank_lines(eval);
            KeyResult::Handled
        }
        // C-x C-e → eval-last-sexp
        (Some('e'), true) => {
            eval_last_sexp(eval);
            KeyResult::Handled
        }
        // C-x e → execute last keyboard macro
        (Some('e'), false) => {
            execute_keyboard_macro(eval, minibuf, kmacro);
            KeyResult::Handled
        }
        // C-x o → other-window (cycle between windows)
        (Some('o'), false) => {
            cycle_window(eval);
            KeyResult::Handled
        }
        // C-x 0/1/2/3 → window commands
        (Some(c), false) if c.is_ascii_digit() => match c {
            '0' => {
                delete_window(eval);
                KeyResult::Handled
            }
            '1' => {
                delete_other_windows(eval);
                KeyResult::Handled
            }
            '2' => {
                split_window_below(eval);
                KeyResult::Handled
            }
            '3' => {
                split_window_right(eval);
                KeyResult::Handled
            }
            _ => KeyResult::Ignored,
        },
        _ => {
            tracing::debug!("Unhandled C-x {:?} ctrl={}", key_char, is_ctrl);
            KeyResult::Ignored
        }
    }
}

/// Handle keys after C-x r prefix (registers and bookmarks).
fn handle_cxr_key(
    eval: &mut Evaluator,
    keysym: u32,
    _modifiers: u32,
    minibuf: &mut MinibufferState,
) -> KeyResult {
    let key_char = if (0x20..=0x7E).contains(&keysym) {
        Some((keysym as u8) as char)
    } else {
        None
    };
    match key_char {
        // C-x r m → set bookmark
        Some('m') => {
            activate_minibuffer(
                eval,
                minibuf,
                "Set bookmark: ",
                MinibufferAction::SetBookmark,
            );
            KeyResult::Handled
        }
        // C-x r b → jump to bookmark
        Some('b') => {
            activate_minibuffer(
                eval,
                minibuf,
                "Jump to bookmark: ",
                MinibufferAction::JumpBookmark,
            );
            KeyResult::Handled
        }
        // C-x r SPC → point to register
        Some(' ') => {
            activate_minibuffer(
                eval,
                minibuf,
                "Point to register: ",
                MinibufferAction::PointToRegister,
            );
            KeyResult::Handled
        }
        // C-x r j → jump to register
        Some('j') => {
            activate_minibuffer(
                eval,
                minibuf,
                "Jump to register: ",
                MinibufferAction::JumpToRegister,
            );
            KeyResult::Handled
        }
        // C-x r s → copy region to register
        Some('s') => {
            if let Some(buf) = eval.buffer_manager().current_buffer() {
                if let Some(mark) = buf.mark {
                    let start = mark.min(buf.pt);
                    let end = mark.max(buf.pt);
                    let text = buf.text.to_string();
                    let region = text[start..end.min(text.len())].to_string();
                    activate_minibuffer(
                        eval,
                        minibuf,
                        "Copy to register: ",
                        MinibufferAction::PointToRegister,
                    );
                    // Store the text for later — stash in saved_input
                    minibuf.saved_input = format!("\x01TEXT\x01{}", region);
                    KeyResult::Handled
                } else {
                    tracing::info!("No region active");
                    KeyResult::Handled
                }
            } else {
                KeyResult::Ignored
            }
        }
        // C-x r i → insert register content
        Some('i') => {
            activate_minibuffer(
                eval,
                minibuf,
                "Insert register: ",
                MinibufferAction::JumpToRegister,
            );
            // Mark as "insert text" mode via saved_input
            minibuf.saved_input = "\x01INSERT\x01".to_string();
            KeyResult::Handled
        }
        _ => {
            tracing::debug!("Unhandled C-x r {:?}", key_char);
            KeyResult::Ignored
        }
    }
}

/// Execute an Elisp command string, returning Handled on success.
fn exec_command(eval: &mut Evaluator, command: &str) -> KeyResult {
    match neovm_core::emacs_core::parse_forms(command) {
        Ok(forms) => {
            for form in &forms {
                if let Err(e) = eval.eval_expr(form) {
                    tracing::debug!("Command '{}' error: {:?}", command, e);
                    return KeyResult::Ignored;
                }
            }
            KeyResult::Handled
        }
        Err(e) => {
            tracing::error!("Parse error for '{}': {}", command, e);
            KeyResult::Ignored
        }
    }
}

/// Save the current buffer to its associated file.
fn save_current_buffer(eval: &Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => {
            tracing::warn!("No current buffer to save");
            return;
        }
    };

    let path = match &buf.file_name {
        Some(p) => p.clone(),
        None => {
            tracing::warn!("Buffer '{}' has no file name", buf.name);
            return;
        }
    };

    // Create backup file (file~) on first save
    let backup_path = format!("{}~", path);
    if std::path::Path::new(&path).exists() && !std::path::Path::new(&backup_path).exists() {
        if let Err(e) = std::fs::copy(&path, &backup_path) {
            tracing::warn!("Could not create backup {}: {}", backup_path, e);
        }
    }

    // Extract buffer text
    let text = buf.text.to_string();
    match std::fs::write(&path, &text) {
        Ok(()) => {
            tracing::info!("Saved {} ({} bytes)", path, text.len());
            // Clean up auto-save file after successful save
            let p = std::path::Path::new(&path);
            if let Some(parent) = p.parent() {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let auto_save = parent.join(format!("#{name}#"));
                let _ = std::fs::remove_file(auto_save);
            }
        }
        Err(e) => tracing::error!("Error saving {}: {}", path, e),
    }
}

/// Activate the minibuffer with a prompt.
fn activate_minibuffer(
    eval: &mut Evaluator,
    minibuf: &mut MinibufferState,
    prompt: &str,
    action: MinibufferAction,
) {
    // Save the current selected window
    if let Some(frame) = eval.frame_manager().selected_frame() {
        minibuf.prev_selected = frame.selected_window;
    }

    minibuf.active = true;
    minibuf.prompt = prompt.to_string();
    minibuf.input.clear();
    minibuf.action = action;
    minibuf.history_pos = None;
    minibuf.saved_input.clear();

    // Save point as search origin for incremental search
    minibuf.search_origin = eval
        .buffer_manager()
        .current_buffer()
        .map(|b| b.pt)
        .unwrap_or(0);

    // Update the minibuffer buffer to show the prompt
    update_minibuffer_display(eval, minibuf);

    tracing::info!("Minibuffer activated: {}", prompt);
}

/// Cancel the minibuffer (C-g).
fn cancel_minibuffer(eval: &mut Evaluator, minibuf: &mut MinibufferState) {
    // Clear search highlights if we were searching
    if matches!(
        minibuf.action,
        MinibufferAction::SearchForward | MinibufferAction::SearchBackward
    ) {
        clear_search_highlights(eval);
    }

    minibuf.active = false;
    minibuf.input.clear();
    minibuf.prompt.clear();
    minibuf.history_pos = None;
    minibuf.saved_input.clear();

    // Clear the minibuffer buffer
    if let Some(buf) = eval.buffer_manager_mut().get_mut(minibuf.minibuf_id) {
        let len = buf.text.len();
        if len > 0 {
            buf.text.delete_range(0, len);
        }
        buf.pt = 0;
        buf.begv = 0;
        buf.zv = 0;
    }

    tracing::info!("Minibuffer cancelled");
}

/// Update the minibuffer buffer to show prompt + input.
fn update_minibuffer_display(eval: &mut Evaluator, minibuf: &MinibufferState) {
    if let Some(buf) = eval.buffer_manager_mut().get_mut(minibuf.minibuf_id) {
        let len = buf.text.len();
        if len > 0 {
            buf.text.delete_range(0, len);
        }
        let display = format!("{}{}", minibuf.prompt, minibuf.input);
        buf.text.insert_str(0, &display);
        let cc = buf.text.char_count();
        buf.begv = 0;
        buf.zv = cc;
        buf.pt = cc; // cursor at end
    }
}

/// Handle a key press while the minibuffer is active.
fn handle_minibuffer_key(
    eval: &mut Evaluator,
    keysym: u32,
    modifiers: u32,
    minibuf: &mut MinibufferState,
    scratch_id: BufferId,
) -> KeyResult {
    // C-g: cancel
    if keysym == 0x67 && (modifiers & CTRL_MASK) != 0 {
        return KeyResult::Quit; // will trigger cancel_minibuffer
    }

    // Enter: submit
    if keysym == XK_RETURN && modifiers == 0 {
        let input = minibuf.input.clone();
        // Push non-empty input to history (avoid consecutive duplicates)
        if !input.is_empty() {
            if minibuf.history.last().map_or(true, |last| last != &input) {
                minibuf.history.push(input.clone());
            }
        }
        minibuf.history_pos = None;
        minibuf.saved_input.clear();
        let action = std::mem::replace(&mut minibuf.action, MinibufferAction::FindFile);
        minibuf.active = false;
        minibuf.input.clear();
        minibuf.prompt.clear();

        // Clear the minibuffer display
        if let Some(buf) = eval.buffer_manager_mut().get_mut(minibuf.minibuf_id) {
            let len = buf.text.len();
            if len > 0 {
                buf.text.delete_range(0, len);
            }
            buf.pt = 0;
            buf.begv = 0;
            buf.zv = 0;
        }

        // Execute the action
        match action {
            MinibufferAction::FindFile => {
                let path = PathBuf::from(&input);
                let path = if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir().unwrap_or_default().join(path)
                };
                if path.exists() {
                    open_file(eval, &path, scratch_id);
                    tracing::info!("find-file: opened {}", path.display());
                } else {
                    // Create new buffer for non-existent file
                    open_file_new(eval, &path);
                    tracing::info!("find-file: new file {}", path.display());
                }
            }
            MinibufferAction::SwitchBuffer => {
                // Find buffer by name
                if let Some(buf_id) = eval.buffer_manager().find_buffer_by_name(&input) {
                    eval.buffer_manager_mut().set_current(buf_id);
                    // Update the selected window (not root — handles split windows)
                    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
                        let wid = frame.selected_window;
                        if let Some(w) = frame.find_window_mut(wid) {
                            if let Window::Leaf {
                                buffer_id,
                                window_start,
                                point,
                                ..
                            } = w
                            {
                                *buffer_id = buf_id;
                                *window_start = 0;
                                *point = 0;
                            }
                        }
                    }
                    tracing::info!("switch-to-buffer: {}", input);
                } else {
                    tracing::warn!("No buffer named '{}'", input);
                }
            }
            MinibufferAction::SearchForward => {
                // On Enter, search from current point to find next occurrence
                let pt = eval
                    .buffer_manager()
                    .current_buffer()
                    .map(|b| b.pt)
                    .unwrap_or(0);
                search_forward_from(eval, &input, pt);
                clear_search_highlights(eval);
            }
            MinibufferAction::SearchBackward => {
                // On Enter, keep position where backward search landed
                clear_search_highlights(eval);
            }
            MinibufferAction::ZapToChar => {
                if let Some(ch) = input.chars().next() {
                    zap_to_char(eval, ch);
                }
            }
            MinibufferAction::ExecuteCommand => {
                // Handle native commands first
                match input.as_str() {
                    "sort-lines" => sort_lines(eval),
                    "count-words-region" => count_words_region(eval),
                    "delete-blank-lines" => delete_blank_lines(eval),
                    "goto-matching-paren" => goto_matching_paren(eval),
                    "revert-buffer" => revert_buffer(eval),
                    "toggle-truncate-lines" => toggle_truncate_lines(eval),
                    "toggle-line-numbers" => toggle_line_numbers(eval),
                    "buffer-stats" => display_buffer_stats(eval),
                    "scratch" => open_scratch_buffer(eval, "*scratch*"),
                    "occur" => {
                        // Search for pattern in current buffer, show matches
                        // This is handled by prompting again
                        activate_minibuffer(eval, minibuf, "Occur: ", MinibufferAction::Occur);
                    }
                    "what-line" => {
                        if let Some(buf) = eval.buffer_manager().current_buffer() {
                            let text = buf.text.to_string();
                            let line = text[..buf.pt.min(text.len())].matches('\n').count() + 1;
                            tracing::info!("Line {}", line);
                        }
                    }
                    "shell-command" => {
                        activate_minibuffer(
                            eval,
                            minibuf,
                            "Shell command: ",
                            MinibufferAction::ShellCommand,
                        );
                    }
                    "compile" => {
                        activate_minibuffer(
                            eval,
                            minibuf,
                            "Compile command: ",
                            MinibufferAction::Compile,
                        );
                    }
                    "grep" => {
                        activate_minibuffer(eval, minibuf, "Grep: ", MinibufferAction::GrepCmd);
                    }
                    "whitespace-cleanup" => {
                        whitespace_cleanup(eval);
                    }
                    _ => {
                        // Try to evaluate (command-name) as Elisp
                        let cmd = format!("({})", input);
                        exec_command(eval, &cmd);
                    }
                }
                tracing::info!("M-x {}", input);
            }
            MinibufferAction::GotoLine => {
                if let Ok(line_num) = input.parse::<usize>() {
                    goto_line(eval, line_num);
                    tracing::info!("goto-line: {}", line_num);
                } else {
                    tracing::warn!("goto-line: invalid number '{}'", input);
                }
            }
            MinibufferAction::ReplaceFrom => {
                // First prompt done, now ask for replacement string
                let prompt = format!("Replace {} with: ", input);
                activate_minibuffer(
                    eval,
                    minibuf,
                    &prompt,
                    MinibufferAction::ReplaceTo { from: input },
                );
            }
            MinibufferAction::ReplaceTo { from } => {
                replace_string(eval, &from, &input);
            }
            MinibufferAction::ConfirmQuit => {
                if input.eq_ignore_ascii_case("yes") || input == "y" {
                    minibuf.quit_requested = true;
                } else {
                    tracing::info!("Quit cancelled");
                }
            }
            MinibufferAction::WriteFile => {
                // Save buffer to the specified path
                let path = PathBuf::from(&input);
                let path = if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir().unwrap_or_default().join(path)
                };
                if let Some(buf) = eval.buffer_manager().current_buffer() {
                    let text = buf.text.to_string();
                    match std::fs::write(&path, &text) {
                        Ok(()) => {
                            tracing::info!("Wrote {} ({} bytes)", path.display(), text.len());
                            // Update buffer's file name
                            let path_str = path.to_string_lossy().to_string();
                            let new_name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| path_str.clone());
                            if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
                                buf.file_name = Some(path_str);
                                buf.name = new_name;
                                buf.modified = false;
                            }
                        }
                        Err(e) => tracing::error!("Error writing {}: {}", path.display(), e),
                    }
                }
            }
            MinibufferAction::Occur => {
                // Search current buffer for pattern, create *Occur* buffer
                if !input.is_empty() {
                    occur(eval, &input);
                }
            }
            MinibufferAction::ShellCommand => {
                // Run shell command, show output in echo area or *Shell Output*
                if !input.is_empty() {
                    shell_command(eval, &input);
                }
            }
            MinibufferAction::Compile => {
                if !input.is_empty() {
                    compile_command(eval, &input);
                }
            }
            MinibufferAction::GrepCmd => {
                if !input.is_empty() {
                    grep_command(eval, &input);
                }
            }
            MinibufferAction::SetBookmark => {
                if !input.is_empty() {
                    if let Some(buf) = eval.buffer_manager().current_buffer() {
                        let bname = buf.name.clone();
                        let pt = buf.pt;
                        BOOKMARKS.with(|bm| {
                            bm.borrow_mut().insert(input.clone(), (bname, pt));
                        });
                        tracing::info!("Bookmark '{}' set", input);
                    }
                }
            }
            MinibufferAction::JumpBookmark => {
                BOOKMARKS.with(|bm| {
                    if let Some((buf_name, pos)) = bm.borrow().get(&input).cloned() {
                        if let Some(buf_id) = eval.buffer_manager().find_buffer_by_name(&buf_name) {
                            eval.buffer_manager_mut().set_current(buf_id);
                            if let Some(buf) = eval.buffer_manager_mut().get_mut(buf_id) {
                                buf.pt = pos;
                            }
                            if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
                                let wid = frame.selected_window;
                                if let Some(w) = frame.find_window_mut(wid) {
                                    if let Window::Leaf {
                                        buffer_id, point, ..
                                    } = w
                                    {
                                        *buffer_id = buf_id;
                                        *point = pos;
                                    }
                                }
                            }
                            tracing::info!("Jumped to bookmark '{}'", input);
                        } else {
                            tracing::warn!("Buffer '{}' no longer exists", buf_name);
                        }
                    } else {
                        tracing::warn!("No bookmark named '{}'", input);
                    }
                });
            }
            MinibufferAction::PointToRegister => {
                if let Some(ch) = input.chars().next() {
                    // Check if saved_input has stashed text (C-x r s)
                    let saved = std::mem::take(&mut minibuf.saved_input);
                    if saved.starts_with("\x01TEXT\x01") {
                        let text = saved.trim_start_matches("\x01TEXT\x01");
                        REGISTERS.with(|r| {
                            r.borrow_mut()
                                .insert(ch, RegisterEntry::Text(text.to_string()));
                        });
                        tracing::info!("Register '{}' = text ({} chars)", ch, text.len());
                    } else if let Some(buf) = eval.buffer_manager().current_buffer() {
                        let bname = buf.name.clone();
                        let pt = buf.pt;
                        REGISTERS.with(|r| {
                            r.borrow_mut().insert(
                                ch,
                                RegisterEntry::Position {
                                    buffer_name: bname,
                                    pos: pt,
                                },
                            );
                        });
                        tracing::info!("Register '{}' = point", ch);
                    }
                }
            }
            MinibufferAction::JumpToRegister => {
                if let Some(ch) = input.chars().next() {
                    let saved = std::mem::take(&mut minibuf.saved_input);
                    let insert_mode = saved.starts_with("\x01INSERT\x01");
                    REGISTERS.with(|r| {
                        if let Some(entry) = r.borrow().get(&ch).cloned() {
                            match entry {
                                RegisterEntry::Position { buffer_name, pos } => {
                                    if insert_mode {
                                        tracing::info!("Register '{}' has position, not text", ch);
                                        return;
                                    }
                                    if let Some(buf_id) =
                                        eval.buffer_manager().find_buffer_by_name(&buffer_name)
                                    {
                                        eval.buffer_manager_mut().set_current(buf_id);
                                        if let Some(buf) = eval.buffer_manager_mut().get_mut(buf_id)
                                        {
                                            buf.pt = pos;
                                        }
                                        if let Some(frame) =
                                            eval.frame_manager_mut().selected_frame_mut()
                                        {
                                            let wid = frame.selected_window;
                                            if let Some(w) = frame.find_window_mut(wid) {
                                                if let Window::Leaf {
                                                    buffer_id, point, ..
                                                } = w
                                                {
                                                    *buffer_id = buf_id;
                                                    *point = pos;
                                                }
                                            }
                                        }
                                        tracing::info!("Jumped to register '{}'", ch);
                                    }
                                }
                                RegisterEntry::Text(text) => {
                                    if insert_mode {
                                        // Insert text at point
                                        if let Some(buf) =
                                            eval.buffer_manager_mut().current_buffer_mut()
                                        {
                                            buf.text.insert_str(buf.pt, &text);
                                            buf.pt += text.len();
                                            buf.zv = buf.text.char_count();
                                            buf.modified = true;
                                        }
                                        tracing::info!(
                                            "Inserted register '{}' ({} chars)",
                                            ch,
                                            text.len()
                                        );
                                    } else {
                                        // Jump mode but register has text — just log
                                        tracing::info!("Register '{}' has text: {}", ch, text);
                                    }
                                }
                            }
                        } else {
                            tracing::warn!("Register '{}' is empty", ch);
                        }
                    });
                }
            }
        }
        return KeyResult::Handled;
    }

    // Backspace: delete last char from input
    if keysym == XK_BACKSPACE && modifiers == 0 {
        if !minibuf.input.is_empty() {
            minibuf.input.pop();
            update_minibuffer_display(eval, minibuf);
        }
        return KeyResult::Handled;
    }

    // Printable ASCII: append to input
    if (32..=126).contains(&keysym) && (modifiers & !SHIFT_MASK) == 0 {
        let ch = keysym as u8 as char;
        minibuf.input.push(ch);
        update_minibuffer_display(eval, minibuf);
        // Incremental search: always search from origin point
        if matches!(minibuf.action, MinibufferAction::SearchForward) {
            search_forward_from(eval, &minibuf.input.clone(), minibuf.search_origin);
            highlight_search_matches(eval, &minibuf.input);
        }
        if matches!(minibuf.action, MinibufferAction::SearchBackward) {
            search_backward_from(eval, &minibuf.input.clone(), minibuf.search_origin);
            highlight_search_matches(eval, &minibuf.input);
        }
        // ZapToChar: take the first typed char and execute immediately
        if matches!(minibuf.action, MinibufferAction::ZapToChar) {
            if let Some(ch) = minibuf.input.chars().next() {
                // Immediately execute zap-to-char
                minibuf.active = false;
                minibuf.input.clear();
                minibuf.prompt.clear();
                if let Some(buf) = eval.buffer_manager_mut().get_mut(minibuf.minibuf_id) {
                    let len = buf.text.len();
                    if len > 0 {
                        buf.text.delete_range(0, len);
                    }
                    buf.pt = 0;
                    buf.begv = 0;
                    buf.zv = 0;
                }
                zap_to_char(eval, ch);
            }
        }
        return KeyResult::Handled;
    }

    // Up: navigate history backwards (older)
    if keysym == XK_UP && modifiers == 0 && !minibuf.history.is_empty() {
        match minibuf.history_pos {
            None => {
                // Save current input and go to most recent history entry
                minibuf.saved_input = minibuf.input.clone();
                let idx = minibuf.history.len() - 1;
                minibuf.history_pos = Some(idx);
                minibuf.input = minibuf.history[idx].clone();
            }
            Some(pos) if pos > 0 => {
                // Go to older entry
                let idx = pos - 1;
                minibuf.history_pos = Some(idx);
                minibuf.input = minibuf.history[idx].clone();
            }
            _ => {} // Already at oldest entry
        }
        update_minibuffer_display(eval, minibuf);
        return KeyResult::Handled;
    }

    // Down: navigate history forwards (newer)
    if keysym == XK_DOWN && modifiers == 0 && minibuf.history_pos.is_some() {
        let pos = minibuf.history_pos.unwrap();
        if pos + 1 < minibuf.history.len() {
            // Go to newer entry
            let idx = pos + 1;
            minibuf.history_pos = Some(idx);
            minibuf.input = minibuf.history[idx].clone();
        } else {
            // Past newest entry — restore saved input
            minibuf.history_pos = None;
            minibuf.input = minibuf.saved_input.clone();
        }
        update_minibuffer_display(eval, minibuf);
        return KeyResult::Handled;
    }

    // Tab: completion
    if keysym == XK_TAB && modifiers == 0 {
        if let Some(completed) = try_complete(eval, minibuf) {
            minibuf.input = completed;
            update_minibuffer_display(eval, minibuf);
        }
        return KeyResult::Handled;
    }

    // C-u: clear input
    if keysym == 0x75 && (modifiers & CTRL_MASK) != 0 {
        minibuf.input.clear();
        update_minibuffer_display(eval, minibuf);
        return KeyResult::Handled;
    }

    KeyResult::Ignored
}

/// Kill the current buffer and switch to the next available buffer.
fn kill_current_buffer(eval: &mut Evaluator) {
    let cur_id = match eval.buffer_manager().current_buffer().map(|b| b.id) {
        Some(id) => id,
        None => return,
    };

    let cur_name = eval
        .buffer_manager()
        .get(cur_id)
        .map(|b| b.name.clone())
        .unwrap_or_default();

    // Find another buffer to switch to (skip hidden buffers like *Minibuf-0*)
    let next_id = eval
        .buffer_manager()
        .buffer_list()
        .into_iter()
        .filter(|&id| id != cur_id)
        .filter(|&id| {
            eval.buffer_manager()
                .get(id)
                .map(|b| !b.name.starts_with(' ')) // skip hidden buffers
                .unwrap_or(false)
        })
        .next();

    if let Some(next_id) = next_id {
        eval.buffer_manager_mut().set_current(next_id);
        // Update frame's root window
        if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
            if let Window::Leaf {
                buffer_id,
                window_start,
                point,
                ..
            } = &mut frame.root_window
            {
                *buffer_id = next_id;
                *window_start = 0;
                *point = 0;
            }
        }
        // Kill the old buffer
        eval.buffer_manager_mut().kill_buffer(cur_id);
        let next_name = eval
            .buffer_manager()
            .get(next_id)
            .map(|b| b.name.as_str())
            .unwrap_or("?");
        tracing::info!("Killed buffer '{}', switched to '{}'", cur_name, next_name);
    } else {
        tracing::info!("Cannot kill '{}': no other buffer to switch to", cur_name);
    }
}

/// Search forward in the current buffer from a given starting position.
fn search_forward_from(eval: &mut Evaluator, query: &str, from: usize) {
    if query.is_empty() {
        return;
    }

    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };

    let text = buf.text.to_string();
    let start = from.min(text.len());

    // Search from start position forward
    if let Some(pos) = text[start..].find(query) {
        let new_pt = start + pos + query.len();
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.pt = new_pt;
        }
    } else if let Some(pos) = text[..start].find(query) {
        // Wrap around: search from beginning
        let new_pt = pos + query.len();
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.pt = new_pt;
        }
    }
}

/// Incremental search backward from position.
fn search_backward_from(eval: &mut Evaluator, query: &str, from: usize) {
    if query.is_empty() {
        return;
    }
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let end = from.min(text.len());
    // Search backward from `end`
    if let Some(pos) = text[..end].rfind(query) {
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.pt = pos;
        }
    } else if let Some(pos) = text[end..].rfind(query) {
        // Wrap around: search from end of buffer
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.pt = end + pos;
        }
    }
}

/// Delete from point up to and including the next occurrence of char (M-z).
fn zap_to_char(eval: &mut Evaluator, ch: char) {
    let (pt, zap_end) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let text = buf.text.to_string();
        let pt = buf.pt;
        if pt >= text.len() {
            return;
        }
        // Find next occurrence of ch after point
        match text[pt..].find(ch) {
            Some(offset) => (pt, pt + offset + ch.len_utf8()),
            None => return, // char not found
        }
    };
    // Kill the text from pt to zap_end
    let killed_text = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        buf.text.to_string()[pt..zap_end].to_string()
    };
    eval.kill_ring_mut().push(killed_text);
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.delete_region(pt, zap_end);
        let cc = buf.text.char_count();
        buf.zv = cc;
        buf.pt = pt;
        tracing::info!("zap-to-char '{}': killed {} bytes", ch, zap_end - pt);
    }
}

/// Set mark at point (C-SPC).
fn set_mark_command(eval: &mut Evaluator) {
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let pt = buf.pt;
        buf.mark = Some(pt);
        tracing::info!("Mark set at {}", pt);
    }
}

/// Kill from point to end of line (C-k).
/// Insert newline and copy indentation from previous line.
fn newline_and_indent(eval: &mut Evaluator) {
    let indent = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let text = buf.text.to_string();
        let pt = buf.pt;
        // Find start of current line
        let line_start = text[..pt].rfind('\n').map(|p| p + 1).unwrap_or(0);
        // Extract leading whitespace from current line
        let line = &text[line_start..];
        let indent_end = line.len() - line.trim_start().len();
        text[line_start..line_start + indent_end].to_string()
    };
    // Insert newline + indentation
    let insert_str = format!("\n{}", indent);
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let pt = buf.pt;
        buf.text.insert_str(pt, &insert_str);
        let cc = buf.text.char_count();
        buf.zv = cc;
        buf.pt = pt + insert_str.len();
        buf.modified = true;
    }
}

/// Insert spaces to the next tab stop (4 columns).
fn indent_for_tab(eval: &mut Evaluator) {
    let spaces = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let text = buf.text.to_string();
        let pt = buf.pt;
        // Find column position
        let line_start = text[..pt].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let col = pt - line_start;
        let tab_width = 4;
        let spaces_needed = tab_width - (col % tab_width);
        " ".repeat(spaces_needed)
    };
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let pt = buf.pt;
        buf.text.insert_str(pt, &spaces);
        let cc = buf.text.char_count();
        buf.zv = cc;
        buf.pt = pt + spaces.len();
        buf.modified = true;
    }
}

/// Indent or dedent the region (or current line) by 4 spaces.
fn indent_region(eval: &mut Evaluator, indent: bool) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;
    let mark = buf.mark.unwrap_or(pt);
    let start = pt.min(mark);
    let end = pt.max(mark);

    // Find line boundaries that overlap the region
    let region_start = text[..start].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let region_end = if end < text.len() {
        text[end..]
            .find('\n')
            .map(|p| end + p)
            .unwrap_or(text.len())
    } else {
        text.len()
    };

    let region = &text[region_start..region_end];
    let new_region: String = region
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let modified = if indent {
                format!("    {}", line)
            } else {
                // Remove up to 4 leading spaces
                let stripped = line
                    .strip_prefix("    ")
                    .or_else(|| line.strip_prefix("   "))
                    .or_else(|| line.strip_prefix("  "))
                    .or_else(|| line.strip_prefix(' '))
                    .unwrap_or(line);
                stripped.to_string()
            };
            if i > 0 {
                format!("\n{}", modified)
            } else {
                modified
            }
        })
        .collect();

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let byte_start = buf.text.char_to_byte(region_start);
        let byte_end = buf.text.char_to_byte(region_end);
        buf.text.delete_range(byte_start, byte_end);
        buf.text.insert_str(byte_start, &new_region);
        let cc = buf.text.char_count();
        buf.zv = cc;
        // Adjust mark and point
        buf.mark = Some(region_start);
        buf.pt = region_start + new_region.chars().count();
        buf.modified = true;
    }
}

/// Move the current line up one position.
fn move_line_up(eval: &mut Evaluator) {
    let (line_start, line_end, prev_start, line_text) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let text = buf.text.to_string();
        let pt = buf.pt;
        let line_start = text[..pt].rfind('\n').map(|p| p + 1).unwrap_or(0);
        if line_start == 0 {
            return; // Already at first line
        }
        let line_end = text[pt..]
            .find('\n')
            .map(|p| pt + p + 1)
            .unwrap_or(text.len());
        let prev_start = text[..line_start - 1]
            .rfind('\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        let line_text = text[line_start..line_end].to_string();
        (line_start, line_end, prev_start, line_text)
    };
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let col = buf.pt - line_start;
        buf.text.delete_range(line_start, line_end);
        buf.text.insert_str(prev_start, &line_text);
        buf.zv = buf.text.char_count();
        buf.pt = prev_start + col;
        buf.modified = true;
    }
}

/// Move the current line down one position.
fn move_line_down(eval: &mut Evaluator) {
    let (line_start, line_end, next_end, line_text) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let text = buf.text.to_string();
        let pt = buf.pt;
        let line_start = text[..pt].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let line_end = text[pt..]
            .find('\n')
            .map(|p| pt + p + 1)
            .unwrap_or(text.len());
        if line_end >= text.len() {
            return; // Already at last line
        }
        let next_end = text[line_end..]
            .find('\n')
            .map(|p| line_end + p + 1)
            .unwrap_or(text.len());
        let line_text = text[line_start..line_end].to_string();
        (line_start, line_end, next_end, line_text)
    };
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let col = buf.pt - line_start;
        // Delete current line, then insert after the next line
        buf.text.delete_range(line_start, line_end);
        let new_start = next_end - (line_end - line_start);
        buf.text.insert_str(new_start, &line_text);
        buf.zv = buf.text.char_count();
        buf.pt = new_start + col;
        buf.modified = true;
    }
}

/// Duplicate the current line. If `down` is true, cursor moves to the copy below.
fn duplicate_line(eval: &mut Evaluator, down: bool) {
    let (line_start, line_end, line_text) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let text = buf.text.to_string();
        let pt = buf.pt;
        let line_start = text[..pt].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let mut line_end = text[pt..]
            .find('\n')
            .map(|p| pt + p + 1)
            .unwrap_or(text.len());
        // Include newline if not present at end
        if line_end == text.len() && !text.ends_with('\n') {
            line_end = text.len();
        }
        let line_text = text[line_start..line_end].to_string();
        (line_start, line_end, line_text)
    };
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let col = buf.pt - line_start;
        let needs_newline = !line_text.ends_with('\n');
        let insert_text = if needs_newline {
            format!("\n{}", line_text)
        } else {
            line_text.clone()
        };
        buf.text.insert_str(line_end, &insert_text);
        buf.zv = buf.text.char_count();
        if down {
            let new_line_start = line_end + if needs_newline { 1 } else { 0 };
            buf.pt = new_line_start + col;
        }
        buf.modified = true;
    }
}

/// Delete the entire current line (C-S-k).
fn delete_whole_line(eval: &mut Evaluator) {
    let (line_start, line_end) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let text = buf.text.to_string();
        let pt = buf.pt;
        let line_start = text[..pt].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let line_end = text[pt..]
            .find('\n')
            .map(|p| pt + p + 1)
            .unwrap_or(text.len());
        (line_start, line_end)
    };
    let killed = {
        let buf = match eval.buffer_manager_mut().current_buffer_mut() {
            Some(b) => b,
            None => return,
        };
        let killed = buf.text.to_string()[line_start..line_end].to_string();
        buf.text.delete_range(line_start, line_end);
        buf.zv = buf.text.char_count();
        buf.pt = line_start.min(buf.text.len());
        buf.modified = true;
        killed
    };
    eval.kill_ring_mut().push(killed);
}

fn kill_line(eval: &mut Evaluator) {
    let (pt, line_end, text_at_pt) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let pt = buf.pt;
        let text = buf.text.to_string();
        // Find end of current line
        let remaining = &text[pt..];
        let newline_pos = remaining.find('\n');
        let line_end = match newline_pos {
            Some(0) => pt + 1,  // At newline: kill just the newline
            Some(n) => pt + n,  // Kill to end of line (not including newline)
            None => text.len(), // Last line: kill to end of buffer
        };
        let killed = text[pt..line_end].to_string();
        (pt, line_end, killed)
    };

    if pt < line_end {
        clipboard_set(&text_at_pt);
        eval.kill_ring_mut().push(text_at_pt);
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.delete_region(pt, line_end);
        }
    }
}

/// Kill the region between mark and point (C-w).
/// Copy text to the system clipboard (best-effort, silent on failure).
fn clipboard_set(text: &str) {
    // Try xclip first, then xsel, then wl-copy
    if let Ok(mut child) = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    } else if let Ok(mut child) = Command::new("xsel")
        .args(["--clipboard", "--input"])
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// Get text from the system clipboard (best-effort).
fn clipboard_get() -> Option<String> {
    if let Ok(output) = Command::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output()
    {
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).to_string());
        }
    }
    if let Ok(output) = Command::new("xsel")
        .args(["--clipboard", "--output"])
        .output()
    {
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).to_string());
        }
    }
    None
}

fn kill_region(eval: &mut Evaluator) {
    let (start, end, killed_text) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let pt = buf.pt;
        let mark = match buf.mark {
            Some(m) => m,
            None => {
                tracing::info!("kill-region: no mark set");
                return;
            }
        };
        let start = pt.min(mark);
        let end = pt.max(mark);
        let text = buf.text.to_string();
        let killed = text[start..end].to_string();
        (start, end, killed)
    };

    if start < end {
        clipboard_set(&killed_text);
        eval.kill_ring_mut().push(killed_text);
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.delete_region(start, end);
            buf.mark = None;
        }
    }
}

/// Copy the region to the kill ring without deleting (M-w).
fn copy_region_as_kill(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let pt = buf.pt;
    let mark = match buf.mark {
        Some(m) => m,
        None => {
            tracing::info!("copy-region-as-kill: no mark set");
            return;
        }
    };
    let start = pt.min(mark);
    let end = pt.max(mark);
    let text = buf.text.to_string();
    let copied = text[start..end].to_string();

    if !copied.is_empty() {
        clipboard_set(&copied);
        eval.kill_ring_mut().push(copied);
        tracing::info!("Copied {} chars to kill ring", end - start);
    }
}

/// Yank the most recent kill ring entry at point (C-y).
/// If the system clipboard has different content, use that instead.
fn yank(eval: &mut Evaluator) {
    // Check system clipboard — if it differs from kill ring top, use it
    let text = if let Some(clip) = clipboard_get() {
        let kr_top = eval.kill_ring().current().map(|s| s.to_string());
        if kr_top.as_deref() != Some(&clip) && !clip.is_empty() {
            // System clipboard has new content — add it to kill ring and use it
            eval.kill_ring_mut().push(clip.clone());
            clip
        } else {
            kr_top.unwrap_or_default()
        }
    } else {
        match eval.kill_ring().current() {
            Some(t) => t.to_string(),
            None => {
                tracing::info!("yank: kill ring empty");
                return;
            }
        }
    };

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let pt = buf.pt;
        buf.insert(&text);
        // Set mark at the beginning of yanked text
        buf.mark = Some(pt);
    }
}

/// Cycle through kill ring entries, replacing the last-yanked text (M-y).
fn yank_pop(eval: &mut Evaluator) {
    // Rotate the kill ring to get the next entry
    let (_prev, next) = {
        let kr = eval.kill_ring();
        let prev = match kr.current() {
            Some(t) => t.to_string(),
            None => return,
        };
        // We need to rotate — get next entry
        let _ = kr;
        eval.kill_ring_mut().rotate(1);
        let next = match eval.kill_ring().current() {
            Some(t) => t.to_string(),
            None => return,
        };
        (prev, next)
    };

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        // The previous yank set mark at the start of yanked text
        // Replace from mark to point with the new text
        if let Some(mark) = buf.mark {
            let start = mark.min(buf.pt);
            let end = mark.max(buf.pt);
            // Delete the old yanked text
            let byte_start = buf.text.char_to_byte(start);
            let byte_end = buf.text.char_to_byte(end);
            buf.text.delete_range(byte_start, byte_end);
            buf.zv = buf.text.char_count();
            // Insert the new text at start
            buf.text.insert_str(byte_start, &next);
            buf.zv = buf.text.char_count();
            buf.mark = Some(start);
            buf.pt = start + next.chars().count();
            buf.modified = true;
        }
    }
}

/// Smart home: move to first non-whitespace char, or to column 0 if already there.
fn smart_home(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;

    // Find beginning of line
    let line_start = text[..pt].rfind('\n').map(|p| p + 1).unwrap_or(0);
    // Find first non-whitespace on the line
    let first_nonws = text[line_start..]
        .find(|c: char| !c.is_whitespace() || c == '\n')
        .map(|offset| line_start + offset)
        .unwrap_or(line_start);

    let new_pt = if pt == first_nonws || first_nonws > pt {
        // Already at first non-ws or first non-ws is past us — go to column 0
        line_start
    } else {
        // Go to first non-ws
        first_nonws
    };

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.pt = new_pt;
    }
}

/// Start shift-selection: set mark at current point if no mark is set.
fn shift_select_start(eval: &mut Evaluator) {
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        if buf.mark.is_none() {
            buf.mark = Some(buf.pt);
        }
    }
}

/// Select the entire buffer (C-x h).
fn mark_whole_buffer(eval: &mut Evaluator) {
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.mark = Some(0);
        buf.pt = buf.text.char_count();
    }
}

/// Undo the last change (C-/).
fn undo(eval: &mut Evaluator) {
    let records = {
        let buf = match eval.buffer_manager_mut().current_buffer_mut() {
            Some(b) => b,
            None => return,
        };
        buf.undo_list.pop_undo_group()
    };

    if records.is_empty() {
        tracing::info!("undo: no more undo information");
        return;
    }

    // Apply undo records (they are returned in reverse order — most recent first)
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.undo_list.undoing = true;
        for record in &records {
            use neovm_core::buffer::undo::UndoRecord;
            match record {
                UndoRecord::Insert { pos, len } => {
                    // Undo an insert: delete [pos, pos+len)
                    let end = (*pos + *len).min(buf.text.len());
                    if *pos < end {
                        buf.text.delete_range(*pos, end);
                        let cc = buf.text.char_count();
                        buf.zv = cc;
                        buf.pt = (*pos).min(buf.text.len());
                    }
                }
                UndoRecord::Delete { pos, text } => {
                    // Undo a delete: re-insert text at pos
                    let insert_pos = (*pos).min(buf.text.len());
                    buf.text.insert_str(insert_pos, text);
                    let cc = buf.text.char_count();
                    buf.zv = cc;
                    buf.pt = insert_pos + text.len();
                }
                UndoRecord::CursorMove { pos } => {
                    buf.pt = (*pos).min(buf.text.len());
                }
                _ => {}
            }
        }
        buf.undo_list.undoing = false;
        tracing::info!(
            "Undo: applied {} records, buffer now {} bytes",
            records.len(),
            buf.text.len()
        );
    }
}

/// Scroll up (C-v): move point down by ~half the window height in lines.
fn scroll_up(eval: &mut Evaluator) {
    // Move forward roughly 20 lines (half-window heuristic)
    for _ in 0..20 {
        exec_command(eval, "(next-line 1)");
    }
}

/// Scroll down (M-v): move point up by ~half the window height in lines.
fn scroll_down(eval: &mut Evaluator) {
    for _ in 0..20 {
        exec_command(eval, "(previous-line 1)");
    }
}

/// Recenter the window so the current line is in the middle (C-l).
fn recenter(eval: &mut Evaluator) {
    let (pt, text) = match eval.buffer_manager().current_buffer() {
        Some(buf) => (buf.pt, buf.text.to_string()),
        None => return,
    };

    // Count lines from start to point
    let lines_before_pt = text[..pt.min(text.len())].matches('\n').count();

    // Aim to put point at roughly the middle of the window (~20 lines visible)
    let half_window = 15;
    let target_start_line = if lines_before_pt > half_window {
        lines_before_pt - half_window
    } else {
        0
    };

    // Convert target_start_line to a byte offset
    let mut target_byte = 0;
    let mut line_count = 0;
    for (i, ch) in text.char_indices() {
        if line_count == target_start_line {
            target_byte = i;
            break;
        }
        if ch == '\n' {
            line_count += 1;
        }
    }
    if line_count < target_start_line {
        target_byte = 0; // file too short, just start from top
    }

    // Convert byte offset to char offset for window_start
    let target_char = text[..target_byte].chars().count();

    // Set window_start
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        let wid = frame.selected_window;
        if let Some(w) = frame.find_window_mut(wid) {
            if let Window::Leaf { window_start, .. } = w {
                *window_start = target_char;
            }
        }
    }
}

/// Show cursor position info (C-x =).
fn what_cursor_position(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;
    let total_chars = text.chars().count();

    // Get character at point
    let ch_at_pt = text[pt.min(text.len())..].chars().next();

    // Calculate line and column
    let prefix = &text[..pt.min(text.len())];
    let line = prefix.matches('\n').count() + 1;
    let col = prefix.rfind('\n').map(|i| pt - i - 1).unwrap_or(pt);

    let msg = if let Some(ch) = ch_at_pt {
        format!(
            "Char: {} (0x{:X}, {}), point={} of {}, L{}C{}",
            if ch.is_control() { '?' } else { ch },
            ch as u32,
            ch as u32,
            pt,
            total_chars,
            line,
            col
        )
    } else {
        format!("point={} of {}, L{}C{} (EOB)", pt, total_chars, line, col)
    };
    tracing::info!("{}", msg);
    // Show in echo area / minibuffer
    if let Some(frame) = eval.frame_manager().selected_frame() {
        if let Some(minibuf_wid) = frame.minibuffer_window {
            if let Some(mbuf_id) = frame.find_window(minibuf_wid).and_then(|w| {
                if let Window::Leaf { buffer_id, .. } = w {
                    Some(*buffer_id)
                } else {
                    None
                }
            }) {
                if let Some(mbuf) = eval.buffer_manager_mut().get_mut(mbuf_id) {
                    let len = mbuf.text.len();
                    if len > 0 {
                        mbuf.text.delete_range(0, len);
                    }
                    mbuf.text.insert_str(0, &msg);
                    let cc = mbuf.text.char_count();
                    mbuf.begv = 0;
                    mbuf.zv = cc;
                    mbuf.pt = 0;
                }
            }
        }
    }
}

/// Handle keys after M-g prefix.
fn handle_mg_key(
    eval: &mut Evaluator,
    keysym: u32,
    _modifiers: u32,
    minibuf: &mut MinibufferState,
) -> KeyResult {
    match keysym {
        // g or M-g: goto-line
        0x67 => {
            activate_minibuffer(eval, minibuf, "Goto line: ", MinibufferAction::GotoLine);
            KeyResult::Handled
        }
        _ => {
            tracing::debug!("Unhandled M-g 0x{:X}", keysym);
            KeyResult::Ignored
        }
    }
}

/// Go to a specific line number (1-based).
fn goto_line(eval: &mut Evaluator, line: usize) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let target = if line == 0 { 1 } else { line };
    let mut current_line = 1;
    let mut pos = 0;
    for (i, ch) in text.char_indices() {
        if current_line == target {
            pos = i;
            break;
        }
        if ch == '\n' {
            current_line += 1;
            if current_line == target {
                pos = i + 1;
                break;
            }
        }
    }
    // If target line is beyond end, go to end of buffer
    if current_line < target {
        pos = text.len();
    }
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.pt = pos;
    }
}

/// Replace all occurrences of `from` with `to` in the current buffer.
fn replace_string(eval: &mut Evaluator, from: &str, to: &str) {
    if from.is_empty() {
        return;
    }
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let count = text.matches(from).count();
    if count == 0 {
        tracing::info!("replace-string: no occurrences of '{}'", from);
        return;
    }
    let new_text = text.replace(from, to);
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let len = buf.text.len();
        buf.text.delete_range(0, len);
        buf.text.insert_str(0, &new_text);
        let cc = buf.text.char_count();
        buf.zv = cc;
        buf.pt = buf.pt.min(buf.text.len());
    }
    tracing::info!(
        "replace-string: replaced {} occurrences of '{}' with '{}'",
        count,
        from,
        to
    );
}

/// Show a list of all buffers in a *Buffer List* buffer (C-x C-b).
fn list_buffers(eval: &mut Evaluator) {
    let buf_list = eval.buffer_manager().buffer_list();
    let cur_id = eval.buffer_manager().current_buffer().map(|b| b.id);
    let mut content = String::new();
    content.push_str("  MR Buffer             Size    File\n");
    content.push_str("  -- ------             ----    ----\n");
    for id in &buf_list {
        if let Some(buf) = eval.buffer_manager().get(*id) {
            if buf.name.starts_with(' ') {
                continue; // skip hidden buffers
            }
            let marker = if cur_id == Some(*id) { '.' } else { ' ' };
            let mod_marker = if buf.modified { '*' } else { ' ' };
            let file = buf.file_name.as_deref().unwrap_or("");
            content.push_str(&format!(
                "  {}{} {:<18} {:>6}    {}\n",
                marker,
                mod_marker,
                buf.name,
                buf.text.len(),
                file
            ));
        }
    }

    let list_id = eval
        .buffer_manager()
        .find_buffer_by_name("*Buffer List*")
        .unwrap_or_else(|| eval.buffer_manager_mut().create_buffer("*Buffer List*"));
    if let Some(buf) = eval.buffer_manager_mut().get_mut(list_id) {
        let len = buf.text.len();
        if len > 0 {
            buf.text.delete_range(0, len);
        }
        buf.text.insert_str(0, &content);
        let cc = buf.text.char_count();
        buf.begv = 0;
        buf.zv = cc;
        buf.pt = 0;
    }
    eval.buffer_manager_mut().set_current(list_id);
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        if let Window::Leaf {
            buffer_id,
            window_start,
            point,
            ..
        } = &mut frame.root_window
        {
            *buffer_id = list_id;
            *window_start = 0;
            *point = 0;
        }
    }
}

/// Transpose the two characters before point (C-t).
fn transpose_chars(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;
    // Need at least 2 chars and point > 0
    if text.len() < 2 || pt == 0 {
        return;
    }
    // Get the two characters around point
    let (a_start, a_char, b_start, b_char) = if pt >= text.len() {
        // At end of buffer: swap last two chars
        let mut chars = text.char_indices().rev();
        let (bi, bc) = chars.next().unwrap();
        let (ai, ac) = chars.next().unwrap();
        (ai, ac, bi, bc)
    } else {
        // Swap char before point and char at point
        let before = text[..pt].char_indices().last();
        let at = text[pt..].chars().next();
        match (before, at) {
            (Some((ai, ac)), Some(bc)) => (ai, ac, pt, bc),
            _ => return,
        }
    };
    // Reconstruct with swapped chars
    let mut new_text = String::with_capacity(text.len());
    new_text.push_str(&text[..a_start]);
    new_text.push(b_char);
    let mid_start = a_start + a_char.len_utf8();
    let mid_end = b_start;
    if mid_end > mid_start {
        new_text.push_str(&text[mid_start..mid_end]);
    }
    new_text.push(a_char);
    let after = b_start + b_char.len_utf8();
    new_text.push_str(&text[after..]);

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let len = buf.text.len();
        buf.text.delete_range(0, len);
        buf.text.insert_str(0, &new_text);
        let cc = buf.text.char_count();
        buf.zv = cc;
        // Move point past the transposed pair
        buf.pt = (b_start + b_char.len_utf8()).min(buf.text.len());
    }
}

/// Change the case of the next word (M-u = uppercase, M-l = lowercase).
fn case_word(eval: &mut Evaluator, uppercase: bool) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;
    // Skip non-alphanumeric chars to find word start
    let word_start = text[pt..]
        .find(|c: char| c.is_alphanumeric())
        .map(|i| pt + i)
        .unwrap_or(text.len());
    // Find word end
    let word_end = text[word_start..]
        .find(|c: char| !c.is_alphanumeric())
        .map(|i| word_start + i)
        .unwrap_or(text.len());
    if word_start >= word_end {
        return;
    }
    let word = &text[word_start..word_end];
    let new_word = if uppercase {
        word.to_uppercase()
    } else {
        word.to_lowercase()
    };
    let mut new_text = String::with_capacity(text.len());
    new_text.push_str(&text[..word_start]);
    new_text.push_str(&new_word);
    new_text.push_str(&text[word_end..]);

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let len = buf.text.len();
        buf.text.delete_range(0, len);
        buf.text.insert_str(0, &new_text);
        let cc = buf.text.char_count();
        buf.zv = cc;
        buf.pt = word_end;
    }
}

/// Capitalize the next word (M-c).
fn capitalize_word(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;
    let word_start = text[pt..]
        .find(|c: char| c.is_alphanumeric())
        .map(|i| pt + i)
        .unwrap_or(text.len());
    let word_end = text[word_start..]
        .find(|c: char| !c.is_alphanumeric())
        .map(|i| word_start + i)
        .unwrap_or(text.len());
    if word_start >= word_end {
        return;
    }
    let word = &text[word_start..word_end];
    let mut new_word = String::with_capacity(word.len());
    let mut first = true;
    for c in word.chars() {
        if first {
            for uc in c.to_uppercase() {
                new_word.push(uc);
            }
            first = false;
        } else {
            for lc in c.to_lowercase() {
                new_word.push(lc);
            }
        }
    }
    let mut new_text = String::with_capacity(text.len());
    new_text.push_str(&text[..word_start]);
    new_text.push_str(&new_word);
    new_text.push_str(&text[word_end..]);

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let len = buf.text.len();
        buf.text.delete_range(0, len);
        buf.text.insert_str(0, &new_text);
        let cc = buf.text.char_count();
        buf.zv = cc;
        buf.pt = word_end;
    }
}

/// Kill the word backward from point (M-Backspace).
fn backward_kill_word(eval: &mut Evaluator) {
    let (pt, word_start, killed) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let text = buf.text.to_string();
        let pt = buf.pt;
        if pt == 0 {
            return;
        }
        // Skip non-word chars backward
        let before = &text[..pt];
        let skip_end = before
            .rfind(|c: char| c.is_alphanumeric())
            .map(|i| i + 1)
            .unwrap_or(0);
        // Find word start
        let word_start = text[..skip_end]
            .rfind(|c: char| !c.is_alphanumeric())
            .map(|i| i + 1)
            .unwrap_or(0);
        let killed = text[word_start..pt].to_string();
        (pt, word_start, killed)
    };

    if word_start < pt {
        eval.kill_ring_mut().push(killed);
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.delete_region(word_start, pt);
        }
    }
}

/// Toggle comment on the current line (M-;).
/// Uses ";; " for Lisp-style comment prefix.
fn toggle_comment_line(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;

    // Determine comment prefix based on file extension
    let comment = if let Some(ref fname) = buf.file_name {
        if fname.ends_with(".el") || fname.ends_with(".lisp") || fname.ends_with(".scm") {
            ";; "
        } else if fname.ends_with(".rs")
            || fname.ends_with(".c")
            || fname.ends_with(".cpp")
            || fname.ends_with(".java")
            || fname.ends_with(".js")
            || fname.ends_with(".ts")
            || fname.ends_with(".go")
        {
            "// "
        } else if fname.ends_with(".py") || fname.ends_with(".rb") || fname.ends_with(".sh") {
            "# "
        } else {
            "# "
        }
    } else {
        ";; " // default to Lisp
    };

    // Find current line boundaries
    let line_start = text[..pt].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = text[pt..].find('\n').map(|i| pt + i).unwrap_or(text.len());
    let line = &text[line_start..line_end];

    // Check if line is already commented
    let trimmed = line.trim_start();
    let is_commented = trimmed.starts_with(comment.trim_end());

    if is_commented {
        // Uncomment: remove the comment prefix
        let prefix_start = line_start + line.find(comment.trim_end()).unwrap_or(0);
        let prefix_len = if line[prefix_start - line_start..].starts_with(comment) {
            comment.len()
        } else {
            comment.trim_end().len()
        };
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.delete_region(prefix_start, prefix_start + prefix_len);
        }
    } else {
        // Comment: insert comment prefix at line start (after indentation)
        let indent_end = line_start + line.len() - trimmed.len();
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.text.insert_str(indent_end, comment);
            let cc = buf.text.char_count();
            buf.zv = cc;
            buf.pt = pt + comment.len();
        }
    }
}

/// Fill (wrap) the current paragraph at column 70 (M-q).
fn fill_paragraph(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;
    let fill_column = 70;

    // Find paragraph boundaries (delimited by blank lines)
    let para_start = {
        let before = &text[..pt];
        let mut start = 0;
        if let Some(pos) = before.rfind("\n\n") {
            start = pos + 2;
        }
        start
    };
    let para_end = {
        if let Some(pos) = text[pt..].find("\n\n") {
            pt + pos
        } else {
            text.len()
        }
    };

    if para_start >= para_end {
        return;
    }

    let para = &text[para_start..para_end];
    // Join all words in the paragraph
    let words: Vec<&str> = para.split_whitespace().collect();
    if words.is_empty() {
        return;
    }

    // Re-wrap at fill_column
    let mut filled = String::new();
    let mut col = 0;
    for (i, word) in words.iter().enumerate() {
        if i == 0 {
            filled.push_str(word);
            col = word.len();
        } else if col + 1 + word.len() > fill_column {
            filled.push('\n');
            filled.push_str(word);
            col = word.len();
        } else {
            filled.push(' ');
            filled.push_str(word);
            col += 1 + word.len();
        }
    }

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.delete_region(para_start, para_end);
        buf.text.insert_str(para_start, &filled);
        let cc = buf.text.char_count();
        buf.zv = cc;
        buf.pt = (para_start + filled.len()).min(buf.text.len());
    }
    tracing::info!(
        "fill-paragraph: wrapped {} words at column {}",
        words.len(),
        fill_column
    );
}

/// Join the current line to the previous one (M-^).
/// Deletes the newline and any surrounding whitespace, replacing with a single space.
fn join_line(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;

    // Find the beginning of the current line
    let line_start = text[..pt].rfind('\n').map(|i| i).unwrap_or(0);
    if line_start == 0 && !text.starts_with('\n') {
        return; // First line, nothing to join
    }

    // Delete from end of previous line whitespace through start of current line whitespace
    let prev_end = line_start; // position of the newline
    // Find start of whitespace before the newline on previous line
    let mut ws_start = prev_end;
    while ws_start > 0 && text.as_bytes()[ws_start - 1] == b' '
        || (ws_start > 0 && text.as_bytes()[ws_start - 1] == b'\t')
    {
        ws_start -= 1;
    }
    // Find end of whitespace after the newline on current line
    let mut ws_end = prev_end + 1;
    while ws_end < text.len()
        && (text.as_bytes()[ws_end] == b' ' || text.as_bytes()[ws_end] == b'\t')
    {
        ws_end += 1;
    }

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.delete_region(ws_start, ws_end);
        // Insert a single space (unless at beginning of buffer)
        if ws_start > 0 {
            buf.text.insert_str(ws_start, " ");
            let cc = buf.text.char_count();
            buf.zv = cc;
            buf.pt = ws_start + 1;
        }
    }
}

/// Mark the current paragraph (M-h).
fn mark_paragraph(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;

    // Find paragraph start (previous blank line or start of buffer)
    let para_start = if let Some(pos) = text[..pt].rfind("\n\n") {
        pos + 2
    } else {
        0
    };

    // Find paragraph end (next blank line or end of buffer)
    let para_end = if let Some(pos) = text[pt..].find("\n\n") {
        pt + pos
    } else {
        text.len()
    };

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.mark = Some(para_start);
        buf.pt = para_end;
    }
    tracing::info!("mark-paragraph: {}..{}", para_start, para_end);
}

/// Transform the region between mark and point using a function.
fn transform_region(eval: &mut Evaluator, transform: impl Fn(&str) -> String) {
    let (start, end, transformed) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let pt = buf.pt;
        let mark = match buf.mark {
            Some(m) => m,
            None => {
                tracing::info!("No mark set");
                return;
            }
        };
        let start = pt.min(mark);
        let end = pt.max(mark);
        let text = buf.text.to_string();
        let region = &text[start..end];
        (start, end, transform(region))
    };

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.delete_region(start, end);
        buf.text.insert_str(start, &transformed);
        let cc = buf.text.char_count();
        buf.zv = cc;
        buf.pt = start + transformed.len();
        buf.mark = None;
    }
}

/// Sort lines in the region alphabetically.
fn sort_lines(eval: &mut Evaluator) {
    transform_region(eval, |s| {
        let mut lines: Vec<&str> = s.lines().collect();
        lines.sort();
        lines.join("\n")
    });
    tracing::info!("sort-lines: sorted region");
}

/// Delete blank lines around point (C-x C-o).
fn delete_blank_lines(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;

    // Find the current line
    let line_start = text[..pt].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let line_end = text[pt..].find('\n').map(|p| pt + p).unwrap_or(text.len());
    let current_line = &text[line_start..line_end];

    if current_line.trim().is_empty() {
        // We're on a blank line — delete all consecutive blank lines
        let mut del_start = line_start;
        while del_start > 0 {
            let prev_line_start = text[..del_start.saturating_sub(1)]
                .rfind('\n')
                .map(|p| p + 1)
                .unwrap_or(0);
            let prev_line = &text[prev_line_start..del_start.saturating_sub(1)];
            if prev_line.trim().is_empty() {
                del_start = prev_line_start;
            } else {
                break;
            }
        }
        let mut del_end = line_end;
        while del_end < text.len() {
            if text.as_bytes()[del_end] == b'\n' {
                let next_end = text[del_end + 1..]
                    .find('\n')
                    .map(|p| del_end + 1 + p)
                    .unwrap_or(text.len());
                let next_line = &text[del_end + 1..next_end];
                if next_line.trim().is_empty() {
                    del_end = next_end;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        // Keep one newline
        if del_start > 0 {
            del_start -= 1;
        } // include the newline before
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            let byte_start = buf.text.char_to_byte(del_start);
            let byte_end = buf.text.char_to_byte(del_end);
            buf.text.delete_range(byte_start, byte_end);
            buf.text.insert_str(byte_start, "\n");
            buf.zv = buf.text.char_count();
            buf.pt = del_start + 1;
            buf.modified = true;
        }
    }
}

/// Count words, lines, and characters in region or buffer.
fn count_words_region(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let (start, end, label) = if let Some(mark) = buf.mark {
        let s = mark.min(buf.pt);
        let e = mark.max(buf.pt);
        (s, e, "Region")
    } else {
        (0, text.len(), "Buffer")
    };
    let region = &text[start..end];
    let lines = region.lines().count();
    let words = region.split_whitespace().count();
    let chars = region.chars().count();
    let msg = format!(
        "{} has {} line(s), {} word(s), {} char(s)",
        label, lines, words, chars
    );
    tracing::info!("{}", msg);
}

/// Execute the last recorded keyboard macro (C-x e).
fn execute_keyboard_macro(
    eval: &mut Evaluator,
    minibuf: &mut MinibufferState,
    kmacro: &mut MacroState,
) {
    if kmacro.last_macro.is_empty() {
        tracing::info!("No keyboard macro defined");
        return;
    }
    let events = kmacro.last_macro.clone();
    let mut prefix = PrefixState::None;
    // Temporarily disable recording to prevent recursive recording
    let was_recording = kmacro.recording;
    kmacro.recording = false;
    for (keysym, modifiers) in &events {
        handle_key(eval, *keysym, *modifiers, &mut prefix, minibuf, kmacro);
    }
    kmacro.recording = was_recording;
    tracing::info!("Executed keyboard macro ({} events)", events.len());
}

/// Evaluate the last Lisp expression before point (C-x C-e).
fn eval_last_sexp(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;
    // Find matching opening paren backward from point
    let before = &text[..pt];
    let sexp_start = find_sexp_start(before);
    if sexp_start >= pt {
        tracing::info!("eval-last-sexp: no sexp found before point");
        return;
    }
    let sexp = &text[sexp_start..pt];
    tracing::info!("eval-last-sexp: evaluating '{}'", sexp);
    eval.setup_thread_locals();
    match neovm_core::emacs_core::parse_forms(sexp) {
        Ok(forms) => {
            for form in &forms {
                match eval.eval_expr(form) {
                    Ok(val) => tracing::info!("  => {:?}", val),
                    Err(e) => tracing::error!("  Error: {:?}", e),
                }
            }
        }
        Err(e) => tracing::error!("eval-last-sexp parse error: {}", e),
    }
}

/// Find the start of a sexp ending at `end` by scanning backward for matching parens.
fn find_sexp_start(text: &str) -> usize {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return 0;
    }
    let end = bytes.len();
    // If the text ends with ')', find matching '('
    if bytes[end - 1] == b')' {
        let mut depth = 0i32;
        for i in (0..end).rev() {
            match bytes[i] {
                b')' => depth += 1,
                b'(' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            }
        }
        return 0;
    }
    // Otherwise, find start of current atom (word/number/symbol)
    let mut i = end;
    while i > 0 {
        let c = bytes[i - 1];
        if c == b' ' || c == b'\n' || c == b'\t' || c == b'(' || c == b')' {
            break;
        }
        i -= 1;
    }
    i
}

/// Try tab-completion for the current minibuffer action.
fn try_complete(eval: &Evaluator, minibuf: &MinibufferState) -> Option<String> {
    match &minibuf.action {
        MinibufferAction::SwitchBuffer => {
            // Complete buffer names
            let prefix = &minibuf.input;
            let matches: Vec<String> = eval
                .buffer_manager()
                .buffer_list()
                .into_iter()
                .filter_map(|id| eval.buffer_manager().get(id))
                .filter(|b| !b.name.starts_with(' '))
                .filter(|b| b.name.starts_with(prefix))
                .map(|b| b.name.clone())
                .collect();
            if matches.len() == 1 {
                Some(matches[0].clone())
            } else if matches.len() > 1 {
                // Find common prefix
                let common = common_prefix(&matches);
                if common.len() > prefix.len() {
                    Some(common)
                } else {
                    tracing::info!("Completions: {}", matches.join(", "));
                    None
                }
            } else {
                None
            }
        }
        MinibufferAction::FindFile => try_complete_file(&minibuf.input),
        MinibufferAction::ExecuteCommand => {
            // Complete from known command names
            let commands = vec![
                "beginning-of-buffer",
                "end-of-buffer",
                "beginning-of-line",
                "end-of-line",
                "forward-char",
                "backward-char",
                "forward-word",
                "backward-word",
                "next-line",
                "previous-line",
                "newline",
                "open-line",
                "delete-char",
                "delete-backward-char",
                "kill-word",
                "self-insert-command",
                "recenter",
                "mark-whole-buffer",
                "undo",
                "sort-lines",
                "count-words-region",
                "delete-blank-lines",
                "goto-matching-paren",
                "revert-buffer",
                "toggle-truncate-lines",
                "toggle-line-numbers",
                "buffer-stats",
                "scratch",
                "what-line",
                "occur",
                "shell-command",
                "compile",
                "grep",
                "whitespace-cleanup",
            ];
            let prefix = &minibuf.input;
            let matches: Vec<String> = commands
                .iter()
                .filter(|c| c.starts_with(prefix))
                .map(|c| c.to_string())
                .collect();
            if matches.len() == 1 {
                Some(matches[0].clone())
            } else if matches.len() > 1 {
                let common = common_prefix(&matches);
                if common.len() > prefix.len() {
                    Some(common)
                } else {
                    tracing::info!("Completions: {}", matches.join(", "));
                    None
                }
            } else {
                None
            }
        }
        MinibufferAction::WriteFile => {
            // Reuse file completion from FindFile
            try_complete_file(&minibuf.input)
        }
        _ => None,
    }
}

/// Complete a file path for minibuffer.
fn try_complete_file(input: &str) -> Option<String> {
    let path = if input.is_empty() {
        std::env::current_dir().unwrap_or_default()
    } else {
        let p = PathBuf::from(input);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir().unwrap_or_default().join(p)
        }
    };
    let (dir, prefix) = if input.ends_with('/') || input.is_empty() {
        (path.clone(), String::new())
    } else {
        let parent = path.parent().unwrap_or(&path);
        let stem = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        (parent.to_path_buf(), stem)
    };
    let entries: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() {
                    format!("{}/", name)
                } else {
                    name
                }
            })
            .filter(|n| n.starts_with(&prefix))
            .collect(),
        Err(_) => Vec::new(),
    };
    if entries.len() == 1 {
        let dir_str = dir.to_string_lossy();
        let completed = if dir_str.ends_with('/') {
            format!("{}{}", dir_str, entries[0])
        } else {
            format!("{}/{}", dir_str, entries[0])
        };
        Some(completed)
    } else if entries.len() > 1 {
        let common = common_prefix(&entries);
        if common.len() > prefix.len() {
            let dir_str = dir.to_string_lossy();
            let completed = if dir_str.ends_with('/') {
                format!("{}{}", dir_str, common)
            } else {
                format!("{}/{}", dir_str, common)
            };
            Some(completed)
        } else {
            tracing::info!("Completions: {}", entries.join(", "));
            None
        }
    } else {
        None
    }
}

/// Dynamic abbreviation expansion (M-/).
/// Searches backward in the buffer for words matching the prefix at point.
fn dabbrev_expand(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let pt = buf.pt;

    // Find the partial word before point
    let word_start = text[..pt]
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| i + 1)
        .unwrap_or(0);
    if word_start >= pt {
        return;
    }
    let prefix = &text[word_start..pt];
    if prefix.is_empty() {
        return;
    }

    // Search backward for a matching word
    let search_area = &text[..word_start];
    let mut best_match: Option<&str> = None;

    // Scan backward for words starting with prefix
    let mut i = search_area.len();
    while i > 0 {
        // Find end of a word
        while i > 0
            && !search_area.as_bytes()[i - 1].is_ascii_alphanumeric()
            && search_area.as_bytes()[i - 1] != b'_'
        {
            i -= 1;
        }
        let word_end = i;
        // Find start of this word
        while i > 0
            && (search_area.as_bytes()[i - 1].is_ascii_alphanumeric()
                || search_area.as_bytes()[i - 1] == b'_')
        {
            i -= 1;
        }
        let word_start_pos = i;
        if word_end > word_start_pos {
            let word = &search_area[word_start_pos..word_end];
            if word.starts_with(prefix) && word.len() > prefix.len() {
                best_match = Some(word);
                break;
            }
        }
    }

    // Also search forward
    if best_match.is_none() {
        let forward_area = &text[pt..];
        let mut j = 0;
        while j < forward_area.len() {
            // Skip non-word chars
            while j < forward_area.len()
                && !forward_area.as_bytes()[j].is_ascii_alphanumeric()
                && forward_area.as_bytes()[j] != b'_'
            {
                j += 1;
            }
            let w_start = j;
            // Find word end
            while j < forward_area.len()
                && (forward_area.as_bytes()[j].is_ascii_alphanumeric()
                    || forward_area.as_bytes()[j] == b'_')
            {
                j += 1;
            }
            if j > w_start {
                let word = &forward_area[w_start..j];
                if word.starts_with(prefix) && word.len() > prefix.len() {
                    best_match = Some(word);
                    break;
                }
            }
        }
    }

    if let Some(expansion) = best_match {
        let suffix = &expansion[prefix.len()..];
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.insert(suffix);
        }
        tracing::info!("dabbrev-expand: {} → {}", prefix, expansion);
    } else {
        tracing::info!("dabbrev-expand: no expansion for '{}'", prefix);
    }
}

/// Find the longest common prefix of a list of strings.
fn common_prefix(strings: &[String]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    let first = &strings[0];
    let mut len = first.len();
    for s in &strings[1..] {
        len = len.min(s.len());
        for (i, (a, b)) in first.chars().zip(s.chars()).enumerate() {
            if a != b {
                len = len.min(i);
                break;
            }
        }
    }
    first[..len].to_string()
}

/// Split the selected window vertically (C-x 2).
fn split_window_below(eval: &mut Evaluator) {
    let frame_id = match eval.frame_manager().selected_frame() {
        Some(f) => f.id,
        None => return,
    };
    let selected = eval
        .frame_manager()
        .selected_frame()
        .map(|f| f.selected_window)
        .unwrap_or(WindowId(0));
    let buf_id = eval
        .frame_manager()
        .selected_frame()
        .and_then(|f| f.find_window(selected))
        .and_then(|w| w.buffer_id())
        .unwrap_or(neovm_core::buffer::BufferId(0));

    match eval.frame_manager_mut().split_window(
        frame_id,
        selected,
        SplitDirection::Vertical,
        buf_id,
    ) {
        Some(new_wid) => {
            tracing::info!("split-window-below: new window {:?}", new_wid);
        }
        None => {
            tracing::warn!("split-window-below: failed");
        }
    }
}

/// Split the selected window horizontally (C-x 3).
fn split_window_right(eval: &mut Evaluator) {
    let frame_id = match eval.frame_manager().selected_frame() {
        Some(f) => f.id,
        None => return,
    };
    let selected = eval
        .frame_manager()
        .selected_frame()
        .map(|f| f.selected_window)
        .unwrap_or(WindowId(0));
    let buf_id = eval
        .frame_manager()
        .selected_frame()
        .and_then(|f| f.find_window(selected))
        .and_then(|w| w.buffer_id())
        .unwrap_or(neovm_core::buffer::BufferId(0));

    match eval.frame_manager_mut().split_window(
        frame_id,
        selected,
        SplitDirection::Horizontal,
        buf_id,
    ) {
        Some(new_wid) => {
            tracing::info!("split-window-right: new window {:?}", new_wid);
        }
        None => {
            tracing::warn!("split-window-right: failed");
        }
    }
}

/// Delete the selected window (C-x 0).
fn delete_window(eval: &mut Evaluator) {
    let frame_id = match eval.frame_manager().selected_frame() {
        Some(f) => f.id,
        None => return,
    };
    let selected = eval
        .frame_manager()
        .selected_frame()
        .map(|f| f.selected_window)
        .unwrap_or(WindowId(0));
    if eval.frame_manager_mut().delete_window(frame_id, selected) {
        // Update current buffer to match newly selected window
        if let Some(frame) = eval.frame_manager().selected_frame() {
            if let Some(w) = frame.find_window(frame.selected_window) {
                if let Some(bid) = w.buffer_id() {
                    eval.buffer_manager_mut().set_current(bid);
                }
            }
        }
        tracing::info!("delete-window: deleted {:?}", selected);
    } else {
        tracing::info!("delete-window: cannot delete sole window");
    }
}

/// Delete all other windows (C-x 1).
fn delete_other_windows(eval: &mut Evaluator) {
    let frame_id = match eval.frame_manager().selected_frame() {
        Some(f) => f.id,
        None => return,
    };
    let selected = eval
        .frame_manager()
        .selected_frame()
        .map(|f| f.selected_window)
        .unwrap_or(WindowId(0));
    // Delete all windows except the selected one
    loop {
        let leaves = eval
            .frame_manager()
            .selected_frame()
            .map(|f| f.root_window.leaf_ids())
            .unwrap_or_default();
        let to_delete: Vec<_> = leaves.into_iter().filter(|&id| id != selected).collect();
        if to_delete.is_empty() {
            break;
        }
        for wid in to_delete {
            eval.frame_manager_mut().delete_window(frame_id, wid);
        }
    }
    tracing::info!("delete-other-windows: keeping {:?}", selected);
}

/// Cycle to the next window (C-x o).
fn cycle_window(eval: &mut Evaluator) {
    let frame = match eval.frame_manager().selected_frame() {
        Some(f) => f,
        None => return,
    };
    let leaves = frame.root_window.leaf_ids();
    if leaves.len() <= 1 {
        return;
    }
    let current = frame.selected_window;
    let idx = leaves.iter().position(|&id| id == current).unwrap_or(0);
    let next_idx = (idx + 1) % leaves.len();
    let next_wid = leaves[next_idx];

    let frame_id = frame.id;
    // Update selected window
    if let Some(frame) = eval.frame_manager_mut().get_mut(frame_id) {
        frame.selected_window = next_wid;
    }
    // Update current buffer to match
    if let Some(frame) = eval.frame_manager().selected_frame() {
        if let Some(w) = frame.find_window(next_wid) {
            if let Some(bid) = w.buffer_id() {
                eval.buffer_manager_mut().set_current(bid);
            }
        }
    }
    tracing::info!("other-window: switched to {:?}", next_wid);
}

/// Recursively resize window tree to fit new bounds.
fn resize_window_tree(
    window: &mut Window,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    _old_w: f32,
    _old_h: f32,
) {
    match window {
        Window::Leaf { bounds, .. } => {
            bounds.x = x;
            bounds.y = y;
            bounds.width = width;
            bounds.height = height;
        }
        Window::Internal {
            children,
            direction,
            bounds,
            ..
        } => {
            bounds.x = x;
            bounds.y = y;
            bounds.width = width;
            bounds.height = height;

            let n = children.len() as f32;
            if n == 0.0 {
                return;
            }
            match direction {
                SplitDirection::Vertical => {
                    // Stack top-to-bottom, each gets equal height
                    let child_h = height / n;
                    for (i, child) in children.iter_mut().enumerate() {
                        let child_y = y + child_h * (i as f32);
                        resize_window_tree(child, x, child_y, width, child_h, _old_w, _old_h);
                    }
                }
                SplitDirection::Horizontal => {
                    // Side by side, each gets equal width
                    let child_w = width / n;
                    for (i, child) in children.iter_mut().enumerate() {
                        let child_x = x + child_w * (i as f32);
                        resize_window_tree(child, child_x, y, child_w, height, _old_w, _old_h);
                    }
                }
            }
        }
    }
}

/// Show a help buffer with keybinding summary (C-h).
fn show_help(eval: &mut Evaluator) {
    let content = "\
Neomacs Keybindings
===================

Movement
--------
  C-f / Right      Forward char         C-b / Left       Backward char
  C-n / Down       Next line            C-p / Up         Previous line
  C-a / Home       Beginning of line    C-e / End        End of line
  M-f              Forward word         M-b              Backward word
  C-v / PgDn       Scroll down          M-v / PgUp       Scroll up
  M-<  / C-Home    Beginning of buffer  M->  / C-End     End of buffer
  M-g g            Goto line            C-l              Recenter

Editing
-------
  C-d / Delete     Delete char          Backspace        Delete backward
  C-k              Kill line            C-S-k            Kill whole line
  M-d              Kill word            M-Backspace      Kill word backward
  C-w              Kill region          M-w              Copy region
  C-y              Yank (paste)         M-y              Yank pop (cycle)
  C-/              Undo                 C-t              Transpose chars
  Return           Newline + indent
  Tab              Indent to tab stop   M-^              Join line
  M-u              Uppercase word       M-l              Lowercase word
  M-c              Capitalize word      M-q              Fill paragraph
  M-;              Toggle line comment   C-]              Goto matching paren
  M-Up             Move line up         M-Down           Move line down
  C-S-Up           Duplicate line up    C-S-Down         Duplicate line down
  Tab (region)     Indent region        Shift-Tab        Dedent region/line
  C-x C-o          Delete blank lines

Search & Replace
----------------
  C-s              Search forward       C-r              Search backward
  M-%              Replace string       M-z              Zap to char

Mark & Region
-------------
  C-SPC / C-@      Set mark             C-x h            Mark whole buffer
  M-h              Mark paragraph       Shift+Arrows     Shift-select
  C-x C-u          Uppercase region     C-x C-l          Lowercase region

Files & Buffers
---------------
  C-x C-f          Find file            C-x C-s          Save buffer
  C-x C-w          Write file (save-as) C-x b            Switch buffer
  C-x C-b          List buffers         C-x k            Kill buffer

Windows
-------
  C-x 2            Split below          C-x 3            Split right
  C-x 0            Delete window        C-x 1            Delete others
  C-x o            Cycle window
  Mouse click      Select window & position

Keyboard Macros
--------------
  C-x (            Start recording      C-x )            Stop recording
  C-x e            Execute last macro

Other
-----
  C-x C-e          Eval last sexp       M-x              Execute command
  C-x =            Cursor position      M-/              Dabbrev expand
  C-g              Cancel               C-h              This help
  C-x C-c          Quit
";

    let help_id = eval
        .buffer_manager()
        .find_buffer_by_name("*Help*")
        .unwrap_or_else(|| eval.buffer_manager_mut().create_buffer("*Help*"));
    if let Some(buf) = eval.buffer_manager_mut().get_mut(help_id) {
        let len = buf.text.len();
        if len > 0 {
            buf.text.delete_range(0, len);
        }
        buf.text.insert_str(0, content);
        let cc = buf.text.char_count();
        buf.begv = 0;
        buf.zv = cc;
        buf.pt = 0;
    }
    eval.buffer_manager_mut().set_current(help_id);

    // Show in selected window
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        let wid = frame.selected_window;
        if let Some(w) = frame.find_window_mut(wid) {
            if let Window::Leaf {
                buffer_id,
                window_start,
                point,
                ..
            } = w
            {
                *buffer_id = help_id;
                *window_start = 0;
                *point = 0;
            }
        }
    }
    tracing::info!("Showing help buffer");
}

/// Handle a left mouse click at pixel coordinates (x, y).
/// Finds the clicked window, selects it, and sets point to the approximate position.
fn handle_mouse_click(eval: &mut Evaluator, x: f32, y: f32) {
    let frame = match eval.frame_manager().selected_frame() {
        Some(f) => f,
        None => return,
    };
    let char_w = frame.char_width;
    let char_h = frame.char_height;
    let frame_id = frame.id;

    // Check if click is in the minibuffer
    if let Some(mini_leaf) = &frame.minibuffer_leaf {
        if let Window::Leaf { bounds, .. } = mini_leaf {
            if x >= bounds.x
                && x < bounds.x + bounds.width
                && y >= bounds.y
                && y < bounds.y + bounds.height
            {
                // Click in minibuffer — ignore (minibuffer is keyboard-driven)
                return;
            }
        }
    }

    // Find which leaf window was clicked
    let leaves = frame.root_window.leaf_ids();
    let mut clicked_window = None;
    for wid in &leaves {
        if let Some(w) = frame.find_window(*wid) {
            if let Window::Leaf { bounds, .. } = w {
                if x >= bounds.x
                    && x < bounds.x + bounds.width
                    && y >= bounds.y
                    && y < bounds.y + bounds.height
                {
                    clicked_window = Some(*wid);
                    break;
                }
            }
        }
    }

    let clicked_wid = match clicked_window {
        Some(wid) => wid,
        None => return,
    };

    // Select the clicked window
    if let Some(frame) = eval.frame_manager_mut().get_mut(frame_id) {
        frame.selected_window = clicked_wid;
    }

    // Get window bounds and buffer info
    let (bounds, buf_id, window_start) = {
        let frame = match eval.frame_manager().selected_frame() {
            Some(f) => f,
            None => return,
        };
        match frame.find_window(clicked_wid) {
            Some(Window::Leaf {
                bounds,
                buffer_id,
                window_start,
                ..
            }) => (*bounds, *buffer_id, *window_start),
            _ => return,
        }
    };

    // Update current buffer to match selected window
    eval.buffer_manager_mut().set_current(buf_id);

    // Compute row/col from click position
    // Account for mode-line at the bottom (1 line of char_h)
    let text_y = bounds.y;
    let row = ((y - text_y) / char_h).floor() as usize;
    let col = ((x - bounds.x) / char_w).floor() as usize;

    // Walk through buffer text from window_start to find the target position
    let buf = match eval.buffer_manager().get(buf_id) {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let start = window_start.min(text.len());
    let mut current_row = 0;
    let mut current_col = 0;
    let mut target_pos = start;

    for (i, ch) in text[start..].char_indices() {
        if current_row == row && current_col == col {
            target_pos = start + i;
            break;
        }
        if current_row > row {
            break;
        }
        if ch == '\n' {
            if current_row == row {
                // Click past end of line — set point at end of this line
                target_pos = start + i;
                break;
            }
            current_row += 1;
            current_col = 0;
        } else {
            if current_row == row {
                current_col += 1;
            } else {
                current_col += 1;
            }
        }
        // If we reach end of text
        if start + i + ch.len_utf8() >= text.len() {
            target_pos = text.len();
        }
    }

    // Clamp to row — if target row not found, point stays at start
    if current_row < row {
        target_pos = text.len();
    }

    if let Some(buf) = eval.buffer_manager_mut().get_mut(buf_id) {
        buf.pt = target_pos.min(buf.text.len());
    }
    tracing::info!(
        "Mouse click at ({:.0},{:.0}) → row={}, col={}, pos={}",
        x,
        y,
        row,
        col,
        target_pos
    );
}

/// Handle mouse scroll wheel event.
fn handle_mouse_scroll(eval: &mut Evaluator, delta_y: f32, _x: f32, _y: f32) {
    let lines = if delta_y.abs() > 1.0 {
        // Discrete scroll (standard wheel): delta_y is typically 3.0 or -3.0
        delta_y.abs() as usize
    } else {
        // Fine/pixel-precise scroll
        3
    };

    if delta_y < 0.0 {
        // Scroll up (show earlier content)
        for _ in 0..lines {
            exec_command(eval, "(previous-line 1)");
        }
    } else {
        // Scroll down (show later content)
        for _ in 0..lines {
            exec_command(eval, "(next-line 1)");
        }
    }
}

/// Open a new (non-existent) file: create buffer with the name and file association.
fn open_file_new(eval: &mut Evaluator, path: &PathBuf) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    let buf_id = eval.buffer_manager_mut().create_buffer(&name);
    if let Some(buf) = eval.buffer_manager_mut().get_mut(buf_id) {
        buf.begv = 0;
        buf.zv = 0;
        buf.pt = 0;
        buf.file_name = Some(path.to_string_lossy().to_string());
        // Enable line numbers for file buffers
        buf.properties
            .insert("display-line-numbers".to_string(), Value::True);
    }

    eval.buffer_manager_mut().set_current(buf_id);

    // Update the selected frame's root window to show this buffer
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        if let Window::Leaf {
            buffer_id,
            window_start,
            point,
            ..
        } = &mut frame.root_window
        {
            *buffer_id = buf_id;
            *window_start = 0;
            *point = 0;
        }
    }
}

/// Wait for the wakeup fd to become readable (blocking poll).
fn wait_for_wakeup(fd: std::os::unix::io::RawFd) {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };

    // Poll with 16ms timeout (60fps) to allow periodic work
    unsafe {
        libc::poll(&mut pollfd as *mut _, 1, 16);
    }
}

// ===== Visual Overlays =====

/// Highlight the active region (mark to point) with the "region" face.
fn highlight_region(eval: &mut Evaluator) {
    // Remove old region overlays
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.overlays.remove_overlays_by_property("region-highlight");
    }

    let (mark, pt) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        match buf.mark {
            Some(m) => {
                let start = m.min(buf.pt);
                let end = m.max(buf.pt);
                if start == end {
                    return; // No region to highlight
                }
                (start, end)
            }
            None => return,
        }
    };

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let oid = buf.overlays.make_overlay(mark, pt);
        buf.overlays
            .overlay_put(oid, "face", Value::symbol("region"));
        buf.overlays
            .overlay_put(oid, "region-highlight", Value::True);
    }
}

/// Delete trailing whitespace from all lines in the current buffer.
fn delete_trailing_whitespace(eval: &mut Evaluator) {
    let text = match eval.buffer_manager().current_buffer() {
        Some(b) => b.text.to_string(),
        None => return,
    };
    let cleaned: String = text
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    // Preserve trailing newline if original had one
    let cleaned = if text.ends_with('\n') && !cleaned.ends_with('\n') {
        format!("{}\n", cleaned)
    } else {
        cleaned
    };
    if cleaned != text {
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            let pt = buf.pt;
            let len = buf.text.len();
            buf.text.delete_range(0, len);
            buf.text.insert_str(0, &cleaned);
            buf.zv = buf.text.char_count();
            buf.pt = pt.min(cleaned.len());
        }
    }
}

/// Highlight all matches of `query` in the current buffer with isearch/lazy-highlight faces.
fn highlight_search_matches(eval: &mut Evaluator, query: &str) {
    // Remove old search overlays
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.overlays.remove_overlays_by_property("isearch-overlay");
    }
    if query.is_empty() {
        return;
    }
    let (text, pt) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        (buf.text.to_string(), buf.pt)
    };
    // Find all occurrences
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();
    let mut start = 0;
    while let Some(pos) = text_lower[start..].find(&query_lower) {
        let abs_pos = start + pos;
        let end_pos = abs_pos + query.len();
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            let oid = buf.overlays.make_overlay(abs_pos, end_pos);
            // Current match gets "isearch" face, others get "lazy-highlight"
            let face = if abs_pos == pt || (pt >= abs_pos && pt < end_pos) {
                "isearch"
            } else {
                "lazy-highlight"
            };
            buf.overlays.overlay_put(oid, "face", Value::symbol(face));
            buf.overlays
                .overlay_put(oid, "isearch-overlay", Value::True);
        }
        start = abs_pos + 1;
    }
}

/// Clear search highlight overlays.
fn clear_search_highlights(eval: &mut Evaluator) {
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.overlays.remove_overlays_by_property("isearch-overlay");
    }
}

// ===== Syntax Highlighting =====

/// Simple syntax highlighting: apply font-lock face text properties.
///
/// Called after opening a file or after buffer modification.
/// Highlight matching parentheses/brackets around point.
///
/// Uses overlays with the "show-paren-match" face.
/// Jump to the matching paren/bracket/brace (C-M-f forward, C-M-b backward).
fn goto_matching_paren(eval: &mut Evaluator) {
    let (text, pt) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        (buf.text.to_string(), buf.pt)
    };
    if text.is_empty() || pt >= text.len() {
        return;
    }
    let bytes = text.as_bytes();

    // Check char at point or before point
    let (check_pos, ch) = if pt < bytes.len() && matches!(bytes[pt] as char, '(' | '[' | '{') {
        (pt, bytes[pt] as char)
    } else if pt > 0 && matches!(bytes[pt - 1] as char, ')' | ']' | '}') {
        (pt - 1, bytes[pt - 1] as char)
    } else if pt < bytes.len() && matches!(bytes[pt] as char, ')' | ']' | '}') {
        (pt, bytes[pt] as char)
    } else if pt > 0 && matches!(bytes[pt - 1] as char, '(' | '[' | '{') {
        (pt - 1, bytes[pt - 1] as char)
    } else {
        return;
    };

    let (is_open, match_ch) = match ch {
        '(' => (true, ')'),
        '[' => (true, ']'),
        '{' => (true, '}'),
        ')' => (false, '('),
        ']' => (false, '['),
        '}' => (false, '{'),
        _ => return,
    };

    let match_pos = if is_open {
        let mut depth = 1i32;
        let mut i = check_pos + 1;
        while i < bytes.len() && depth > 0 {
            let c = bytes[i] as char;
            if c == ch {
                depth += 1;
            }
            if c == match_ch {
                depth -= 1;
            }
            if depth == 0 {
                break;
            }
            i += 1;
        }
        if depth == 0 { Some(i + 1) } else { None } // +1 to go past the match
    } else {
        let mut depth = 1i32;
        let mut i = check_pos as isize - 1;
        while i >= 0 && depth > 0 {
            let c = bytes[i as usize] as char;
            if c == ch {
                depth += 1;
            }
            if c == match_ch {
                depth -= 1;
            }
            if depth == 0 {
                break;
            }
            i -= 1;
        }
        if depth == 0 { Some(i as usize) } else { None }
    };

    if let Some(pos) = match_pos {
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            buf.pt = pos;
        }
    }
}

fn highlight_matching_parens(eval: &mut Evaluator) {
    // Remove old paren match overlays
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.overlays.remove_overlays_by_property("show-paren");
    }

    let (text, pt) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        (buf.text.to_string(), buf.pt)
    };

    if text.is_empty() {
        return;
    }

    let bytes = text.as_bytes();

    // Check char before point (like Emacs show-paren-mode)
    let check_pos = if pt > 0 { pt - 1 } else { return };
    if check_pos >= bytes.len() {
        return;
    }

    let ch = bytes[check_pos] as char;
    let (is_open, match_ch) = match ch {
        '(' => (true, ')'),
        '[' => (true, ']'),
        '{' => (true, '}'),
        ')' => (false, '('),
        ']' => (false, '['),
        '}' => (false, '{'),
        _ => return,
    };

    // Find matching paren
    let match_pos = if is_open {
        // Search forward
        let mut depth = 1i32;
        let mut i = check_pos + 1;
        while i < bytes.len() && depth > 0 {
            let c = bytes[i] as char;
            if c == ch {
                depth += 1;
            }
            if c == match_ch {
                depth -= 1;
            }
            if depth == 0 {
                break;
            }
            i += 1;
        }
        if depth == 0 { Some(i) } else { None }
    } else {
        // Search backward
        let mut depth = 1i32;
        let mut i = check_pos as isize - 1;
        while i >= 0 && depth > 0 {
            let c = bytes[i as usize] as char;
            if c == ch {
                depth += 1;
            }
            if c == match_ch {
                depth -= 1;
            }
            if depth == 0 {
                break;
            }
            i -= 1;
        }
        if depth == 0 && i >= 0 {
            Some(i as usize)
        } else {
            None
        }
    };

    if let Some(mp) = match_pos {
        if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
            // Highlight the paren at point
            let oid1 = buf.overlays.make_overlay(check_pos, check_pos + 1);
            buf.overlays
                .overlay_put(oid1, "face", Value::symbol("show-paren-match"));
            buf.overlays.overlay_put(oid1, "show-paren", Value::True);
            // Highlight the matching paren
            let oid2 = buf.overlays.make_overlay(mp, mp + 1);
            buf.overlays
                .overlay_put(oid2, "face", Value::symbol("show-paren-match"));
            buf.overlays.overlay_put(oid2, "show-paren", Value::True);
        }
    }
}

fn fontify_buffer(eval: &mut Evaluator) {
    let (text, ext) = {
        let buf = match eval.buffer_manager().current_buffer() {
            Some(b) => b,
            None => return,
        };
        let ext = buf
            .file_name
            .as_ref()
            .and_then(|f| std::path::Path::new(f).extension())
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        (buf.text.to_string(), ext)
    };

    if text.is_empty() {
        return;
    }

    let keywords: &[&str];
    let line_comment: &str;
    let string_chars: &[char];

    match ext.as_str() {
        "rs" => {
            keywords = &[
                "fn",
                "let",
                "mut",
                "const",
                "static",
                "struct",
                "enum",
                "impl",
                "trait",
                "type",
                "pub",
                "use",
                "mod",
                "crate",
                "self",
                "super",
                "if",
                "else",
                "match",
                "for",
                "while",
                "loop",
                "return",
                "break",
                "continue",
                "where",
                "as",
                "in",
                "ref",
                "move",
                "async",
                "await",
                "unsafe",
                "extern",
                "dyn",
                "macro_rules",
            ];
            line_comment = "//";
            string_chars = &['"'];
        }
        "py" => {
            keywords = &[
                "def", "class", "import", "from", "return", "if", "elif", "else", "for", "while",
                "break", "continue", "pass", "try", "except", "finally", "raise", "with", "as",
                "yield", "lambda", "and", "or", "not", "in", "is", "None", "True", "False", "self",
                "global", "nonlocal", "assert", "del",
            ];
            line_comment = "#";
            string_chars = &['"', '\''];
        }
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" => {
            keywords = &[
                "int", "char", "float", "double", "void", "long", "short", "unsigned", "signed",
                "const", "static", "extern", "struct", "union", "enum", "typedef", "if", "else",
                "for", "while", "do", "switch", "case", "break", "continue", "return", "sizeof",
                "goto", "default", "volatile", "register", "inline", "#include", "#define",
                "#ifdef", "#ifndef", "#endif", "#if",
            ];
            line_comment = "//";
            string_chars = &['"', '\''];
        }
        "el" | "lisp" | "scm" | "clj" => {
            keywords = &[
                "defun",
                "defvar",
                "defconst",
                "defcustom",
                "defmacro",
                "defsubst",
                "let",
                "let*",
                "lambda",
                "if",
                "when",
                "unless",
                "cond",
                "progn",
                "setq",
                "setf",
                "require",
                "provide",
                "interactive",
                "save-excursion",
                "with-current-buffer",
                "dolist",
                "dotimes",
                "while",
                "catch",
                "throw",
            ];
            line_comment = ";";
            string_chars = &['"'];
        }
        "js" | "ts" | "jsx" | "tsx" => {
            keywords = &[
                "function",
                "const",
                "let",
                "var",
                "return",
                "if",
                "else",
                "for",
                "while",
                "do",
                "switch",
                "case",
                "break",
                "continue",
                "class",
                "extends",
                "new",
                "this",
                "super",
                "import",
                "export",
                "from",
                "default",
                "try",
                "catch",
                "finally",
                "throw",
                "async",
                "await",
                "yield",
                "typeof",
                "instanceof",
                "in",
                "of",
                "delete",
                "void",
            ];
            line_comment = "//";
            string_chars = &['"', '\'', '`'];
        }
        "sh" | "bash" | "zsh" => {
            keywords = &[
                "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac",
                "function", "return", "local", "export", "source", "echo", "read", "set", "unset",
                "shift", "exit", "break", "continue",
            ];
            line_comment = "#";
            string_chars = &['"', '\''];
        }
        "go" => {
            keywords = &[
                "func",
                "var",
                "const",
                "type",
                "struct",
                "interface",
                "map",
                "chan",
                "go",
                "select",
                "switch",
                "case",
                "default",
                "if",
                "else",
                "for",
                "range",
                "return",
                "break",
                "continue",
                "defer",
                "package",
                "import",
                "fallthrough",
                "goto",
            ];
            line_comment = "//";
            string_chars = &['"', '`'];
        }
        "java" => {
            keywords = &[
                "class",
                "interface",
                "extends",
                "implements",
                "import",
                "package",
                "public",
                "private",
                "protected",
                "static",
                "final",
                "abstract",
                "void",
                "int",
                "long",
                "double",
                "float",
                "boolean",
                "char",
                "byte",
                "short",
                "new",
                "return",
                "if",
                "else",
                "for",
                "while",
                "do",
                "switch",
                "case",
                "break",
                "continue",
                "try",
                "catch",
                "finally",
                "throw",
                "throws",
                "this",
                "super",
                "null",
                "true",
                "false",
                "synchronized",
                "volatile",
                "transient",
                "instanceof",
            ];
            line_comment = "//";
            string_chars = &['"', '\''];
        }
        "toml" => {
            keywords = &[];
            line_comment = "#";
            string_chars = &['"', '\''];
        }
        "yaml" | "yml" => {
            keywords = &["true", "false", "null", "yes", "no"];
            line_comment = "#";
            string_chars = &['"', '\''];
        }
        "md" | "markdown" => {
            keywords = &[];
            line_comment = "";
            string_chars = &[];
        }
        "rb" => {
            keywords = &[
                "def",
                "end",
                "class",
                "module",
                "if",
                "elsif",
                "else",
                "unless",
                "while",
                "until",
                "for",
                "do",
                "begin",
                "rescue",
                "ensure",
                "raise",
                "return",
                "yield",
                "block_given?",
                "require",
                "include",
                "extend",
                "attr_reader",
                "attr_writer",
                "attr_accessor",
                "nil",
                "true",
                "false",
                "self",
                "super",
                "puts",
                "print",
            ];
            line_comment = "#";
            string_chars = &['"', '\''];
        }
        _ => return, // No highlighting for unknown file types
    }

    // Clear existing text properties by replacing with a new empty table
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        buf.text_props = neovm_core::buffer::text_props::TextPropertyTable::new();
    }

    // Apply highlighting
    let bytes = text.as_bytes();
    let len = text.len();
    let mut i = 0;

    while i < len {
        // Line comments
        if !line_comment.is_empty() && text[i..].starts_with(line_comment) {
            let start = i;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
                buf.text_props.put_property(
                    start,
                    i,
                    "face",
                    Value::symbol("font-lock-comment-face"),
                );
            }
            continue;
        }

        // Strings
        if string_chars.contains(&(bytes[i] as char)) {
            let quote = bytes[i];
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2; // skip escaped char
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else if bytes[i] == b'\n' && quote != b'`' {
                    break; // unterminated string
                } else {
                    i += 1;
                }
            }
            if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
                buf.text_props.put_property(
                    start,
                    i,
                    "face",
                    Value::symbol("font-lock-string-face"),
                );
            }
            continue;
        }

        // Keywords (word boundary check)
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' || bytes[i] == b'#' {
            let start = i;
            while i < len
                && (bytes[i].is_ascii_alphanumeric()
                    || bytes[i] == b'_'
                    || bytes[i] == b'!'
                    || bytes[i] == b'#')
            {
                i += 1;
            }
            let word = &text[start..i];
            // Check if it's preceded by a non-word char (word boundary)
            let at_boundary =
                start == 0 || !bytes[start - 1].is_ascii_alphanumeric() && bytes[start - 1] != b'_';
            // Check if followed by non-word char
            let at_end_boundary = i >= len || !bytes[i].is_ascii_alphanumeric() && bytes[i] != b'_';
            if at_boundary && at_end_boundary && keywords.contains(&word) {
                if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
                    buf.text_props.put_property(
                        start,
                        i,
                        "face",
                        Value::symbol("font-lock-keyword-face"),
                    );
                }
            }
            continue;
        }

        i += 1;
    }
}

// ===== CLI argument parsing =====

/// Items to load at startup.
enum LoadItem {
    /// Load an Elisp file.
    File(PathBuf),
    /// Evaluate an Elisp expression.
    Eval(String),
}

/// Parsed command-line arguments.
struct Args {
    /// Elisp files/expressions to load (in order).
    load: Vec<LoadItem>,
    /// Files to open in buffers.
    files: Vec<PathBuf>,
}

/// Parse command-line arguments.
///
/// Supported flags:
///   --load FILE / -l FILE   Load an Elisp file
///   --eval EXPR / -e EXPR   Evaluate an Elisp expression
///   FILE                    Open a file in a buffer
fn parse_args() -> Args {
    let mut args = Args {
        load: Vec::new(),
        files: Vec::new(),
    };

    let mut iter = std::env::args().skip(1); // skip program name
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--load" | "-l" => {
                if let Some(path) = iter.next() {
                    args.load.push(LoadItem::File(PathBuf::from(path)));
                } else {
                    tracing::error!("{} requires a file path argument", arg);
                }
            }
            "--eval" | "-e" => {
                if let Some(expr) = iter.next() {
                    args.load.push(LoadItem::Eval(expr));
                } else {
                    tracing::error!("{} requires an expression argument", arg);
                }
            }
            "--" => {
                // Everything after -- is a file to open
                for remaining in iter.by_ref() {
                    args.files.push(PathBuf::from(remaining));
                }
                break;
            }
            _ if arg.starts_with('-') => {
                tracing::warn!("Unknown option: {}", arg);
            }
            _ => {
                // Positional argument: file to open
                args.files.push(PathBuf::from(arg));
            }
        }
    }

    args
}

/// Open a file into a new buffer and switch to it.
fn open_file(eval: &mut Evaluator, path: &PathBuf, _scratch_id: BufferId) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Cannot open {}: {}", path.display(), e);
            return;
        }
    };

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    let buf_id = eval.buffer_manager_mut().create_buffer(&name);
    if let Some(buf) = eval.buffer_manager_mut().get_mut(buf_id) {
        buf.text.insert_str(0, &content);
        let cc = buf.text.char_count();
        buf.begv = 0;
        buf.zv = cc;
        buf.pt = 0; // start at beginning
        buf.file_name = Some(path.to_string_lossy().to_string());
        // Enable line numbers for file buffers
        buf.properties
            .insert("display-line-numbers".to_string(), Value::True);
    }

    eval.buffer_manager_mut().set_current(buf_id);

    // Update the selected frame's root window to show this buffer
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        if let Window::Leaf {
            buffer_id,
            window_start,
            point,
            ..
        } = &mut frame.root_window
        {
            *buffer_id = buf_id;
            *window_start = 0;
            *point = 0;
        }
    }

    tracing::info!("Opened file: {} ({} chars)", path.display(), content.len());
    fontify_buffer(eval);
}

/// Auto-save modified file buffers to #file# auto-save files.
fn auto_save_buffers(eval: &Evaluator) {
    let buf_list = eval.buffer_manager().buffer_list();
    for id in &buf_list {
        if let Some(buf) = eval.buffer_manager().get(*id) {
            if buf.modified {
                if let Some(ref file_name) = buf.file_name {
                    let path = std::path::Path::new(file_name);
                    let auto_save_name = if let Some(parent) = path.parent() {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        parent
                            .join(format!("#{name}#"))
                            .to_string_lossy()
                            .to_string()
                    } else {
                        format!("#{file_name}#")
                    };
                    let text = buf.text.to_string();
                    match std::fs::write(&auto_save_name, &text) {
                        Ok(()) => tracing::info!("Auto-saved {}", auto_save_name),
                        Err(e) => tracing::warn!("Auto-save failed {}: {}", auto_save_name, e),
                    }
                }
            }
        }
    }
}

/// Revert the current buffer from its file on disk.
fn revert_buffer(eval: &mut Evaluator) {
    let file_name = match eval.buffer_manager().current_buffer() {
        Some(buf) => match &buf.file_name {
            Some(f) => f.clone(),
            None => {
                tracing::warn!("revert-buffer: no file associated");
                return;
            }
        },
        None => return,
    };

    let content = match std::fs::read_to_string(&file_name) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("revert-buffer: cannot read {}: {}", file_name, e);
            return;
        }
    };

    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let len = buf.text.len();
        if len > 0 {
            buf.text.delete_range(0, len);
        }
        buf.text.insert_str(0, &content);
        let cc = buf.text.char_count();
        buf.begv = 0;
        buf.zv = cc;
        buf.pt = buf.pt.min(cc);
        buf.modified = false;
        buf.mark = None;
    }
    fontify_buffer(eval);
    tracing::info!("Reverted buffer from {}", file_name);
}

/// Display the line count and word count of the buffer in the echo area.
fn display_buffer_stats(eval: &mut Evaluator) {
    let buf = match eval.buffer_manager().current_buffer() {
        Some(b) => b,
        None => return,
    };
    let text = buf.text.to_string();
    let lines = text.lines().count();
    let words = text.split_whitespace().count();
    let chars = text.chars().count();
    let bytes = text.len();
    let name = buf.name.clone();
    let modified = if buf.modified { " (modified)" } else { "" };
    let msg = format!(
        "{}{}: {} lines, {} words, {} chars, {} bytes",
        name, modified, lines, words, chars, bytes
    );
    tracing::info!("{}", msg);
}

/// Toggle display of line numbers in the current buffer.
fn toggle_line_numbers(eval: &mut Evaluator) {
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let has_lnums = buf
            .properties
            .get("display-line-numbers")
            .map(|v| matches!(v, Value::True))
            .unwrap_or(false);
        if has_lnums {
            buf.properties.remove("display-line-numbers");
            tracing::info!("Line numbers disabled");
        } else {
            buf.properties
                .insert("display-line-numbers".to_string(), Value::True);
            tracing::info!("Line numbers enabled");
        }
    }
}

/// Toggle line wrapping mode for the current buffer.
fn toggle_truncate_lines(eval: &mut Evaluator) {
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let current = buf
            .properties
            .get("truncate-lines")
            .map(|v| matches!(v, Value::True))
            .unwrap_or(false);
        if current {
            buf.properties.remove("truncate-lines");
            tracing::info!("Word wrap enabled");
        } else {
            buf.properties
                .insert("truncate-lines".to_string(), Value::True);
            tracing::info!("Line truncation enabled");
        }
    }
}

/// Open a scratch buffer with the given name.
fn open_scratch_buffer(eval: &mut Evaluator, name: &str) {
    let buf_id = eval
        .buffer_manager()
        .find_buffer_by_name(name)
        .unwrap_or_else(|| eval.buffer_manager_mut().create_buffer(name));
    let buf_pt = eval.buffer_manager().get(buf_id).map(|b| b.pt).unwrap_or(0);
    eval.buffer_manager_mut().set_current(buf_id);
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        let wid = frame.selected_window;
        if let Some(w) = frame.find_window_mut(wid) {
            if let Window::Leaf {
                buffer_id,
                window_start,
                point,
                ..
            } = w
            {
                *buffer_id = buf_id;
                *window_start = 0;
                *point = buf_pt;
            }
        }
    }
}

/// M-x occur: find all lines matching pattern, show in *Occur* buffer.
fn occur(eval: &mut Evaluator, pattern: &str) {
    let (buf_name, text) = match eval.buffer_manager().current_buffer() {
        Some(b) => (b.name.clone(), b.text.to_string()),
        None => return,
    };
    let mut result = format!("Lines matching \"{}\" in {}:\n\n", pattern, buf_name);
    let mut match_count = 0;
    let pattern_lower = pattern.to_lowercase();
    for (i, line) in text.lines().enumerate() {
        if line.to_lowercase().contains(&pattern_lower) {
            result.push_str(&format!("{:>6}: {}\n", i + 1, line));
            match_count += 1;
        }
    }
    if match_count == 0 {
        tracing::info!("No matches for \"{}\"", pattern);
        return;
    }
    result.push_str(&format!("\n{} match(es) found.\n", match_count));
    // Create or reuse *Occur* buffer
    let occur_id = eval
        .buffer_manager()
        .find_buffer_by_name("*Occur*")
        .unwrap_or_else(|| eval.buffer_manager_mut().create_buffer("*Occur*"));
    if let Some(buf) = eval.buffer_manager_mut().get_mut(occur_id) {
        let len = buf.text.len();
        if len > 0 {
            buf.text.delete_range(0, len);
        }
        buf.text.insert_str(0, &result);
        buf.pt = 0;
        buf.begv = 0;
        buf.zv = buf.text.char_count();
        buf.modified = false;
    }
    // Switch to *Occur* buffer
    eval.buffer_manager_mut().set_current(occur_id);
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        let wid = frame.selected_window;
        if let Some(w) = frame.find_window_mut(wid) {
            if let Window::Leaf {
                buffer_id,
                window_start,
                point,
                ..
            } = w
            {
                *buffer_id = occur_id;
                *window_start = 0;
                *point = 0;
            }
        }
    }
    tracing::info!("Occur: {} match(es) for \"{}\"", match_count, pattern);
}

/// M-x shell-command: run shell command, show output in *Shell Output*.
fn shell_command(eval: &mut Evaluator, cmd: &str) {
    use std::process::Command;
    let output = match Command::new("sh").args(["-c", cmd]).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("Shell command failed: {}", e);
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&stderr);
    }
    if result.is_empty() {
        tracing::info!("(Shell command completed with no output)");
        return;
    }
    // Short output goes to echo area; long output to *Shell Output* buffer
    if result.lines().count() <= 1 && result.len() < 200 {
        tracing::info!("{}", result.trim());
    } else {
        let shell_id = eval
            .buffer_manager()
            .find_buffer_by_name("*Shell Output*")
            .unwrap_or_else(|| eval.buffer_manager_mut().create_buffer("*Shell Output*"));
        if let Some(buf) = eval.buffer_manager_mut().get_mut(shell_id) {
            let len = buf.text.len();
            if len > 0 {
                buf.text.delete_range(0, len);
            }
            buf.text.insert_str(0, &result);
            buf.pt = 0;
            buf.begv = 0;
            buf.zv = buf.text.char_count();
            buf.modified = false;
        }
        eval.buffer_manager_mut().set_current(shell_id);
        if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
            let wid = frame.selected_window;
            if let Some(w) = frame.find_window_mut(wid) {
                if let Window::Leaf {
                    buffer_id,
                    window_start,
                    point,
                    ..
                } = w
                {
                    *buffer_id = shell_id;
                    *window_start = 0;
                    *point = 0;
                }
            }
        }
        tracing::info!("Shell command output in *Shell Output*");
    }
}

/// M-x compile: run compilation command, show output in *compilation* buffer.
fn compile_command(eval: &mut Evaluator, cmd: &str) {
    use std::process::Command;
    let output = match Command::new("sh").args(["-c", cmd]).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("Compile failed: {}", e);
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut result = format!("-*- mode: compilation -*-\n\n$ {}\n\n", cmd);
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        result.push_str(&stderr);
    }
    let status = if output.status.success() {
        "\nCompilation finished.\n"
    } else {
        "\nCompilation exited abnormally.\n"
    };
    result.push_str(status);

    let comp_id = eval
        .buffer_manager()
        .find_buffer_by_name("*compilation*")
        .unwrap_or_else(|| eval.buffer_manager_mut().create_buffer("*compilation*"));
    if let Some(buf) = eval.buffer_manager_mut().get_mut(comp_id) {
        let len = buf.text.len();
        if len > 0 {
            buf.text.delete_range(0, len);
        }
        buf.text.insert_str(0, &result);
        buf.pt = 0;
        buf.begv = 0;
        buf.zv = buf.text.char_count();
        buf.modified = false;
    }
    eval.buffer_manager_mut().set_current(comp_id);
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        let wid = frame.selected_window;
        if let Some(w) = frame.find_window_mut(wid) {
            if let Window::Leaf {
                buffer_id,
                window_start,
                point,
                ..
            } = w
            {
                *buffer_id = comp_id;
                *window_start = 0;
                *point = 0;
            }
        }
    }
    tracing::info!(
        "Compilation: {}",
        if output.status.success() {
            "finished"
        } else {
            "failed"
        }
    );
}

/// M-x grep: run grep command, show results in *grep* buffer.
fn grep_command(eval: &mut Evaluator, cmd: &str) {
    use std::process::Command;
    // If the input doesn't start with "grep", prepend it
    let full_cmd = if cmd.starts_with("grep") || cmd.starts_with("rg") {
        cmd.to_string()
    } else {
        format!("grep -rn '{}' .", cmd)
    };
    let output = match Command::new("sh").args(["-c", &full_cmd]).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("Grep failed: {}", e);
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut result = format!("-*- mode: grep -*-\n\n$ {}\n\n", full_cmd);
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        result.push_str(&stderr);
    }
    let match_count = stdout.lines().count();
    result.push_str(&format!("\n{} match(es).\n", match_count));

    let grep_id = eval
        .buffer_manager()
        .find_buffer_by_name("*grep*")
        .unwrap_or_else(|| eval.buffer_manager_mut().create_buffer("*grep*"));
    if let Some(buf) = eval.buffer_manager_mut().get_mut(grep_id) {
        let len = buf.text.len();
        if len > 0 {
            buf.text.delete_range(0, len);
        }
        buf.text.insert_str(0, &result);
        buf.pt = 0;
        buf.begv = 0;
        buf.zv = buf.text.char_count();
        buf.modified = false;
    }
    eval.buffer_manager_mut().set_current(grep_id);
    if let Some(frame) = eval.frame_manager_mut().selected_frame_mut() {
        let wid = frame.selected_window;
        if let Some(w) = frame.find_window_mut(wid) {
            if let Window::Leaf {
                buffer_id,
                window_start,
                point,
                ..
            } = w
            {
                *buffer_id = grep_id;
                *window_start = 0;
                *point = 0;
            }
        }
    }
    tracing::info!("Grep: {} match(es)", match_count);
}

/// M-x whitespace-cleanup: remove trailing whitespace and ensure final newline.
fn whitespace_cleanup(eval: &mut Evaluator) {
    delete_trailing_whitespace(eval);
    // Ensure final newline
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        let text = buf.text.to_string();
        if !text.is_empty() && !text.ends_with('\n') {
            let len = text.len();
            buf.text.insert_str(len, "\n");
            buf.zv = buf.text.char_count();
            buf.modified = true;
        }
    }
    tracing::info!("whitespace-cleanup done");
}

/// C-x C-x: exchange point and mark.
fn exchange_point_and_mark(eval: &mut Evaluator) {
    if let Some(buf) = eval.buffer_manager_mut().current_buffer_mut() {
        if let Some(mark) = buf.mark {
            let old_pt = buf.pt;
            buf.pt = mark;
            buf.mark = Some(old_pt);
        }
    }
}

/// Set up Emacs `load-path` to include the project's lisp/ directory.
fn setup_load_path(eval: &mut Evaluator) {
    // Try to find the lisp/ directory relative to the binary
    let exe_path = std::env::current_exe().ok();
    let lisp_dirs: Vec<PathBuf> = [
        // Relative to binary: ../../../lisp (from neomacs-bin/target/release/)
        exe_path.as_ref().and_then(|p| {
            p.parent()?
                .parent()?
                .parent()?
                .parent()
                .map(|root| root.join("lisp"))
        }),
        // Also check project root via NEOMACS_LISP_DIR env var
        std::env::var("NEOMACS_LISP_DIR").ok().map(PathBuf::from),
        // Try CWD-relative
        Some(PathBuf::from("lisp")),
        // Absolute fallback for dev
        Some(PathBuf::from(
            "/home/exec/Projects/github.com/eval-exec/neomacs/lisp",
        )),
    ]
    .into_iter()
    .flatten()
    .filter(|p| p.is_dir())
    .collect();

    if lisp_dirs.is_empty() {
        tracing::warn!("No lisp/ directory found — Elisp features unavailable");
        return;
    }

    // Build load-path as a Lisp list of directory strings
    let mut load_path = Value::Nil;
    // Add subdirectories too (like emacs does)
    for dir in lisp_dirs.iter().rev() {
        // Add subdirectories first
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut subdirs: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .collect();
            subdirs.sort();
            for subdir in subdirs.iter().rev() {
                // Skip hidden dirs and version-control dirs
                let name = subdir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name.starts_with('.') || name == "obsolete" || name == "term" {
                    continue;
                }
                load_path =
                    Value::cons(Value::string(subdir.to_string_lossy().as_ref()), load_path);
            }
        }
        // Add the main directory
        load_path = Value::cons(Value::string(dir.to_string_lossy().as_ref()), load_path);
    }
    eval.set_variable("load-path", load_path);
    tracing::info!("load-path set to: {:?}", lisp_dirs);
}

/// Load core Elisp files that provide fundamental Emacs functionality.
/// Follows the Emacs loadup.el bootstrap sequence.
fn load_core_elisp(eval: &mut Evaluator) {
    eval.setup_thread_locals();

    // Pre-set variables that correspond to C-level globals in real Emacs.
    // These are always defined (never void) in real Emacs because they're
    // initialized in C source, but neovm-core doesn't have those C globals.
    let bootstrap_vars: Vec<(&str, Value)> = vec![
        ("dump-mode", Value::Nil),
        ("purify-flag", Value::Nil),
        ("max-lisp-eval-depth", Value::Int(4200)),
        ("inhibit-load-charset-map", Value::Nil),
        // C global in real Emacs (macroexp.c) — used by eval-and-compile in byte-run.el
        ("macroexp--dynvars", Value::Nil),
        // C globals referenced early in bootstrap
        ("lexical-binding", Value::Nil),
        ("load-file-name", Value::Nil),
        (
            "load-suffixes",
            Value::list(vec![Value::string(".elc"), Value::string(".el")]),
        ),
        (
            "load-file-rep-suffixes",
            Value::list(vec![Value::string("")]),
        ),
        // Version info (used by version.el, custom.el, etc.)
        ("emacs-version", Value::string("30.1")),
        ("emacs-major-version", Value::Int(30)),
        ("emacs-minor-version", Value::Int(1)),
        ("emacs-build-number", Value::Int(1)),
        ("system-type", Value::symbol("gnu/linux")),
        ("system-configuration", Value::string("x86_64-pc-linux-gnu")),
        // Required by keymap.el, bindings.el, simple.el
        ("most-positive-fixnum", Value::Int(i64::MAX >> 2)), // Emacs fixnum range
        ("most-negative-fixnum", Value::Int(-(i64::MAX >> 2) - 1)),
        // Required by international/mule.el
        ("enable-multibyte-characters", Value::True),
        // Required by env.el
        ("initial-environment", Value::Nil),
        ("process-environment", Value::Nil),
        // Required by faces.el, custom.el
        ("noninteractive", Value::True), // batch mode
        ("inhibit-quit", Value::Nil),
        // Required by window.el
        ("window-system", Value::Nil),
        ("frame-initial-frame", Value::Nil),
        // Required by simple.el, files.el
        ("kill-ring", Value::Nil),
        ("kill-ring-max", Value::Int(60)),
        ("default-directory", Value::string("/")),
        ("buffer-file-name", Value::Nil),
        ("auto-save-default", Value::True),
        // Required by custom.el
        ("custom-current-group-alist", Value::Nil),
        // Required by minibuffer.el
        ("minibuffer-history", Value::Nil),
        // Required by bindings.el, simple.el
        ("help-char", Value::Char('?')),
        ("inhibit-x-resources", Value::Nil),
        // Required by faces.el, frame.el
        ("face-new-frame-defaults", Value::Nil),
        ("initial-frame-alist", Value::Nil),
        ("default-frame-alist", Value::Nil),
        // Required by simple.el
        ("command-error-function", Value::Nil),
        ("executing-kbd-macro", Value::Nil),
        ("last-command", Value::Nil),
        ("this-command", Value::Nil),
        ("real-last-command", Value::Nil),
        ("last-repeatable-command", Value::Nil),
        ("last-command-event", Value::Nil),
        ("last-input-event", Value::Nil),
        ("deactivate-mark", Value::Nil),
        ("transient-mark-mode", Value::Nil),
        ("mark-active", Value::Nil),
        ("inhibit-read-only", Value::Nil),
        ("standard-output", Value::symbol("t")),
        ("standard-input", Value::symbol("t")),
        ("print-length", Value::Nil),
        ("print-level", Value::Nil),
        // Required by files.el
        ("after-init-time", Value::Nil),
        ("user-init-file", Value::Nil),
        ("command-line-args", Value::Nil),
        ("auto-save-list-file-prefix", Value::Nil),
        ("coding-system-for-read", Value::Nil),
        ("coding-system-for-write", Value::Nil),
        ("buffer-file-coding-system", Value::Nil),
        ("file-name-coding-system", Value::Nil),
        ("locale-coding-system", Value::Nil),
        // Required by window.el
        ("window-size-fixed", Value::Nil),
        ("window-combination-limit", Value::Nil),
        ("window-combination-resize", Value::Nil),
        ("fit-window-to-buffer-horizontally", Value::Nil),
        ("temp-buffer-max-height", Value::Nil),
        ("temp-buffer-max-width", Value::Nil),
    ];
    for (name, val) in &bootstrap_vars {
        eval.set_variable(name, val.clone());
    }

    // Create standard keymaps (normally created in C code)
    let keymap_setup = r#"
        (setq global-map (make-sparse-keymap))
        (setq esc-map (make-sparse-keymap))
        (setq ctl-x-map (make-sparse-keymap))
        (setq ctl-x-4-map (make-sparse-keymap))
        (setq ctl-x-5-map (make-sparse-keymap))
        (setq help-map (make-sparse-keymap))
        (setq mode-specific-map (make-sparse-keymap))
        (setq minibuffer-local-map (make-sparse-keymap))
    "#;
    for form in neovm_core::emacs_core::parse_forms(keymap_setup).unwrap_or_default() {
        let _ = eval.eval_expr(&form);
    }

    // Core files to load in order — matching Emacs loadup.el bootstrap sequence.
    // Each file depends on the ones loaded before it.
    let core_files = [
        // Phase 1: Minimum bootstrap (defines defsubst, defmacro enhancements, backquote)
        "emacs-lisp/debug-early", // backtrace for early errors
        "emacs-lisp/byte-run",    // defines defsubst, function-put, declare
        "emacs-lisp/backquote",   // backquote (`) macro
        "subr",                   // fundamental subroutines (when, unless, dolist, etc.)
        // Phase 2: Core infrastructure
        "keymap",                   // keymap functions
        "version",                  // emacs-version, etc.
        "widget",                   // widget library
        "custom",                   // defcustom, defgroup, customize
        "emacs-lisp/map-ynp",       // y-or-n-p with map
        "international/mule",       // MULE (multi-lingual)
        "international/mule-conf",  // MULE configuration
        "env",                      // environment variable functions
        "format",                   // format-spec
        "bindings",                 // key bindings setup
        "window",                   // window management (save-selected-window, etc.)
        "files",                    // file operations
        "emacs-lisp/macroexp",      // macroexpand-all
        "cus-face",                 // defface support
        "faces",                    // face definitions
        "button",                   // button/link support
        "emacs-lisp/cl-preloaded",  // cl-lib basics
        "emacs-lisp/oclosure",      // open closures (used by cl-generic)
        "obarray",                  // obarray functions
        "abbrev",                   // abbreviations
        "help",                     // help system
        "jka-cmpr-hook",            // compressed file hooks
        "epa-hook",                 // encryption hooks
        "international/mule-cmds",  // MULE commands
        "case-table",               // case conversion tables
        "international/characters", // character properties
        "composite",                // character composition
        // Language support
        "language/chinese",
        "language/cyrillic",
        "language/indian",
        "language/sinhala",
        "language/english",
        "language/ethiopic",
        "language/european",
        "language/czech",
        "language/slovak",
        "language/romanian",
        "language/greek",
        "language/hebrew",
        "international/cp51932",
        "international/eucjp-ms",
        "language/japanese",
        "language/korean",
        "language/lao",
        "language/tai-viet",
        "language/thai",
        "language/tibetan",
        "language/vietnamese",
        "language/misc-lang",
        "language/utf-8-lang",
        "language/georgian",
        "language/khmer",
        "language/burmese",
        "language/cham",
        "language/philippine",
        // Core editing features
        "indent",
        "emacs-lisp/cl-generic", // CLOS-style generic functions
        "simple",                // basic editing commands
        "minibuffer",            // minibuffer
        "startup",               // startup sequence
    ];

    let mut loaded_count = 0;
    let mut failed_count = 0;

    for file in &core_files {
        let loaded = load_elisp_file(eval, file);
        if loaded {
            loaded_count += 1;
        } else {
            failed_count += 1;
        }
    }

    tracing::info!(
        "Elisp bootstrap: {} loaded, {} failed",
        loaded_count,
        failed_count
    );
}

/// Load a single Elisp file by searching the lisp/ directory.
/// Uses form-by-form evaluation so individual errors don't abort the whole file.
/// Returns true if the file was found and loaded (even with some form errors).
fn load_elisp_file(eval: &mut Evaluator, name: &str) -> bool {
    tracing::info!("Loading core Elisp: {}", name);

    // Find the .el file in the lisp/ directory tree
    let lisp_base = find_lisp_dir();
    let Some(lisp_dir) = lisp_base else {
        tracing::warn!("  No lisp/ directory found");
        return false;
    };

    let el_path = lisp_dir.join(format!("{}.el", name));
    if !el_path.exists() {
        tracing::warn!("  Not found: {}", el_path.display());
        return false;
    }

    let content = match std::fs::read_to_string(&el_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("  Read error {}: {}", el_path.display(), e);
            return false;
        }
    };

    // Detect lexical-binding from the file's first line
    let use_lexical = content
        .lines()
        .next()
        .map(|line| line.contains("lexical-binding: t"))
        .unwrap_or(false);
    let old_lexical = eval.lexical_binding();
    eval.set_lexical_binding(use_lexical);

    // Set load-file-name so load-related code works
    let path_str = el_path.to_string_lossy().to_string();
    eval.set_variable("load-file-name", Value::string(&path_str));

    let forms = match neovm_core::emacs_core::parse_forms(&content) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("  Parse error {}: {}", name, e);
            eval.set_lexical_binding(old_lexical);
            eval.set_variable("load-file-name", Value::Nil);
            return false;
        }
    };

    let total = forms.len();
    let mut ok = 0;
    let mut errors = 0;
    for (i, form) in forms.iter().enumerate() {
        match eval.eval_expr(form) {
            Ok(_) => ok += 1,
            Err(e) => {
                errors += 1;
                // Only log at debug level for common/expected errors, warn for rare ones
                if errors <= 5 {
                    tracing::warn!("  {}: form {}/{} failed: {}", name, i, total, e);
                } else if errors == 6 {
                    tracing::warn!("  {}: suppressing further error messages...", name);
                }
            }
        }
    }

    eval.set_lexical_binding(old_lexical);
    eval.set_variable("load-file-name", Value::Nil);

    if errors > 0 {
        tracing::info!(
            "  Loaded: {} ({}/{} forms OK, {} errors)",
            name,
            ok,
            total,
            errors
        );
    } else {
        tracing::info!("  Loaded: {} ({} forms)", name, total);
    }
    true
}

/// Find the project's lisp/ directory.
fn find_lisp_dir() -> Option<PathBuf> {
    // Try relative to binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = exe
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            let candidate = root.join("lisp");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    // Try env var
    if let Ok(dir) = std::env::var("NEOMACS_LISP_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Some(p);
        }
    }
    // Try CWD-relative
    let cwd = PathBuf::from("lisp");
    if cwd.is_dir() {
        return Some(cwd);
    }
    None
}
