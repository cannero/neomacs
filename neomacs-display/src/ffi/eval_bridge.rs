//! FFI bridge to the neovm-core Rust Evaluator.
//!
//! Provides C-callable functions to initialize, query, and evaluate Elisp
//! via the Rust Evaluator singleton.

use std::ffi::{c_char, c_int, CStr, CString};

/// Global Evaluator instance (lazily initialized via `neomacs_rust_eval_init`).
static mut RUST_EVALUATOR: Option<neovm_core::emacs_core::Evaluator> = None;

/// Initialize the Rust Evaluator singleton.
///
/// Returns 0 on success, -1 on failure.  Safe to call multiple times —
/// subsequent calls are no-ops that return 0.
///
/// # Safety
/// Must be called from the Emacs main thread before any other eval_bridge
/// functions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_eval_init() -> c_int {
    if (*std::ptr::addr_of!(RUST_EVALUATOR)).is_some() {
        return 0; // already initialized
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut evaluator =
            neovm_core::emacs_core::load::create_bootstrap_evaluator_cached_with_features(&[
                "neomacs",
            ])
            .expect("GNU-compatible bootstrap should succeed");
        evaluator.set_variable("dump-mode", neovm_core::emacs_core::Value::Nil);
        *std::ptr::addr_of_mut!(RUST_EVALUATOR) = Some(evaluator);
        tracing::info!("Rust Evaluator initialized from GNU-compatible bootstrap");
    }));

    match result {
        Ok(()) => 0,
        Err(e) => {
            let msg = if let Some(s) = e.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            tracing::error!(
                "neomacs_rust_eval_init: panic during initialization: {}",
                msg
            );
            -1
        }
    }
}

/// Parse and evaluate an Elisp string, returning the printed result as a
/// newly-allocated C string.
///
/// The caller **must** free the returned pointer with `neomacs_rust_free_string`.
/// Returns `NULL` on error (parse failure, eval error, or uninitialized
/// evaluator).  Error details are logged.
///
/// # Safety
/// `input` must be a valid, NUL-terminated UTF-8 C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_eval_string(input: *const c_char) -> *mut c_char {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if input.is_null() {
            tracing::error!("neomacs_rust_eval_string: null input");
            return std::ptr::null_mut();
        }

        let eval = match (*std::ptr::addr_of_mut!(RUST_EVALUATOR)).as_mut() {
            Some(e) => e,
            None => {
                tracing::error!("neomacs_rust_eval_string: evaluator not initialized");
                return std::ptr::null_mut();
            }
        };

        eval.setup_thread_locals();

        let c_str = match CStr::from_ptr(input).to_str() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("neomacs_rust_eval_string: invalid UTF-8: {}", e);
                return std::ptr::null_mut();
            }
        };

        let forms = match neovm_core::emacs_core::parse_forms(c_str) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("neomacs_rust_eval_string: parse error: {}", e);
                return std::ptr::null_mut();
            }
        };

        let mut last_value = None;
        for form in &forms {
            match eval.eval_expr(form) {
                Ok(value) => {
                    last_value = Some(value);
                }
                Err(e) => {
                    tracing::error!("neomacs_rust_eval_string: eval error: {:?}", e);
                    return std::ptr::null_mut();
                }
            }
        }

        let printed = match last_value {
            Some(ref v) => neovm_core::emacs_core::print_value_with_eval(eval, v),
            None => "nil".to_string(),
        };

        match CString::new(printed) {
            Ok(cs) => cs.into_raw(),
            Err(e) => {
                tracing::error!("neomacs_rust_eval_string: result contains NUL byte: {}", e);
                std::ptr::null_mut()
            }
        }
    }));

    match result {
        Ok(ptr) => ptr,
        Err(_) => {
            tracing::error!("neomacs_rust_eval_string: panic during evaluation");
            std::ptr::null_mut()
        }
    }
}

/// Free a C string previously returned by `neomacs_rust_eval_string`.
///
/// # Safety
/// `s` must be a pointer returned by `neomacs_rust_eval_string`, or NULL.
/// Each pointer must be freed exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_free_string(s: *mut c_char) {
    if !s.is_null() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(CString::from_raw(s));
        }));
        if let Err(_) = result {
            tracing::error!("neomacs_rust_free_string: panic during drop");
        }
    }
}

/// Check whether the Rust Evaluator has been initialized.
///
/// Returns 1 if initialized, 0 if not.
///
/// # Safety
/// May be called from any thread, but the result is only meaningful on the
/// Emacs main thread (where initialization happens).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_eval_ready() -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if (*std::ptr::addr_of!(RUST_EVALUATOR)).is_some() {
            1
        } else {
            0
        }
    }));
    match result {
        Ok(v) => v,
        Err(_) => {
            tracing::error!("neomacs_rust_eval_ready: panic during static read");
            0
        }
    }
}

/// Load an Elisp file through the Rust Evaluator.
///
/// `path` is a NUL-terminated file path string.  The file is loaded via
/// `(load "path")`, which searches `load-path` and handles `.el` suffix
/// resolution and `.neoc` parse caching.
///
/// Returns 0 on success, -1 on error.
///
/// # Safety
/// `path` must be a valid, NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_load_file(path: *const c_char) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if path.is_null() {
            tracing::error!("neomacs_rust_load_file: null path");
            return -1;
        }

        let eval = match (*std::ptr::addr_of_mut!(RUST_EVALUATOR)).as_mut() {
            Some(e) => e,
            None => {
                tracing::error!("neomacs_rust_load_file: evaluator not initialized");
                return -1;
            }
        };

        eval.setup_thread_locals();

        let path_str = match CStr::from_ptr(path).to_str() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("neomacs_rust_load_file: invalid UTF-8: {}", e);
                return -1;
            }
        };

        // Build the Lisp expression (load "path") and evaluate it.
        let load_expr = format!(
            "(load \"{}\")",
            path_str.replace('\\', "\\\\").replace('"', "\\\"")
        );
        let forms = match neovm_core::emacs_core::parse_forms(&load_expr) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(
                    "neomacs_rust_load_file: parse error for '{}': {}",
                    path_str,
                    e
                );
                return -1;
            }
        };

        for form in &forms {
            match eval.eval_expr(form) {
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(
                        "neomacs_rust_load_file: eval error loading '{}': {:?}",
                        path_str,
                        e
                    );
                    return -1;
                }
            }
        }

        tracing::info!("neomacs_rust_load_file: loaded '{}'", path_str);
        0
    }));

    match result {
        Ok(code) => code,
        Err(_) => {
            tracing::error!("neomacs_rust_load_file: panic during load");
            -1
        }
    }
}

// Modifier bitmask constants (must match neomacs_display.h)
const NEOMACS_SHIFT_MASK: c_int = 1 << 0;
const NEOMACS_CTRL_MASK: c_int = 1 << 1;
const NEOMACS_META_MASK: c_int = 1 << 2;
#[allow(dead_code)]
const NEOMACS_SUPER_MASK: c_int = 1 << 3;

// X11 keysym constants for special keys
const XK_RETURN: c_int = 0xFF0D;
const XK_TAB: c_int = 0xFF09;
const XK_BACKSPACE: c_int = 0xFF08;
const XK_DELETE: c_int = 0xFFFF;
const XK_ESCAPE: c_int = 0xFF1B;
const XK_LEFT: c_int = 0xFF51;
const XK_UP: c_int = 0xFF52;
const XK_RIGHT: c_int = 0xFF53;
const XK_DOWN: c_int = 0xFF54;
const XK_HOME: c_int = 0xFF50;
const XK_END: c_int = 0xFF57;

/// Handle a key event from C's wakeup handler.
///
/// Routes the key through the neovm-core Rust evaluator:
/// - Printable ASCII without modifiers: `(self-insert-command 1)`
/// - Return/Tab/Backspace/Delete: direct command calls
/// - Arrow keys: `forward-char`/`backward-char`/`next-line`/`previous-line`
/// - C-a..C-z: mapped to common Emacs commands
/// - Other keys: logged and silently ignored for now
///
/// `keysym` is the X11 keysym (e.g., 97 for 'a', 0xFF0D for Return).
/// `modifiers` is the modifier bitmask (SHIFT=1, CTRL=2, META=4, SUPER=8).
///
/// Returns 0 on success, -1 on error, 1 if the key was not handled
/// (caller should fall back to C event queue).
///
/// # Safety
/// Must be called from the Emacs main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_handle_key(keysym: c_int, modifiers: c_int) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let eval = match (*std::ptr::addr_of_mut!(RUST_EVALUATOR)).as_mut() {
            Some(e) => e,
            None => {
                tracing::error!("neomacs_rust_handle_key: evaluator not initialized");
                return -1;
            }
        };

        eval.setup_thread_locals();

        // Determine the command to execute based on keysym + modifiers
        let command = match (keysym, modifiers) {
            // Printable ASCII without modifiers (or shift-only for uppercase)
            (32..=126, mods) if (mods & !NEOMACS_SHIFT_MASK) == 0 => {
                eval.set_variable(
                    "last-command-event",
                    neovm_core::emacs_core::Value::Int(keysym as i64),
                );
                "(self-insert-command 1)"
            }

            // Return → newline
            (XK_RETURN, 0) => "(newline)",

            // Tab → insert tab (or indent)
            (XK_TAB, 0) => {
                eval.set_variable(
                    "last-command-event",
                    neovm_core::emacs_core::Value::Int(9), // TAB char
                );
                "(self-insert-command 1)"
            }

            // Backspace → delete-backward-char
            (XK_BACKSPACE, 0) => "(delete-backward-char 1)",

            // Delete → delete-char
            (XK_DELETE, 0) => "(delete-char 1)",

            // Arrow keys → navigation
            (XK_LEFT, 0) => "(backward-char 1)",
            (XK_RIGHT, 0) => "(forward-char 1)",
            (XK_UP, 0) => "(previous-line 1)",
            (XK_DOWN, 0) => "(next-line 1)",

            // Home/End
            (XK_HOME, 0) => "(beginning-of-line)",
            (XK_END, 0) => "(end-of-line)",

            // C-a through C-z (ctrl + letter)
            (key, mods)
                if (mods & NEOMACS_CTRL_MASK) != 0
                    && (mods & !NEOMACS_CTRL_MASK & !NEOMACS_SHIFT_MASK) == 0
                    && (0x61..=0x7A).contains(&key) =>
            // 'a'..'z'
            {
                match (key as u8) as char {
                    'a' => "(beginning-of-line)",
                    'b' => "(backward-char 1)",
                    'd' => "(delete-char 1)",
                    'e' => "(end-of-line)",
                    'f' => "(forward-char 1)",
                    'k' => "(kill-line)",
                    'n' => "(next-line 1)",
                    'p' => "(previous-line 1)",
                    'y' => "(yank)",
                    'w' => "(kill-region (mark) (point))",
                    '/' => "(undo)",
                    _ => {
                        tracing::debug!(
                            "neomacs_rust_handle_key: unhandled C-{}",
                            (key as u8) as char
                        );
                        return 1; // not handled
                    }
                }
            }

            // M-key (meta + letter) — common commands
            (key, mods)
                if (mods & NEOMACS_META_MASK) != 0
                    && (mods & !NEOMACS_META_MASK) == 0
                    && (0x61..=0x7A).contains(&key) =>
            {
                match (key as u8) as char {
                    'f' => "(forward-word 1)",
                    'b' => "(backward-word 1)",
                    'd' => "(kill-word 1)",
                    'w' => "(kill-ring-save (mark) (point))",
                    '<' => "(beginning-of-buffer)",
                    '>' => "(end-of-buffer)",
                    _ => {
                        tracing::debug!(
                            "neomacs_rust_handle_key: unhandled M-{}",
                            (key as u8) as char
                        );
                        return 1; // not handled
                    }
                }
            }

            // M-< and M-> (with shift for >)
            (0x3C, mods) if (mods & NEOMACS_META_MASK) != 0 => "(beginning-of-buffer)", // M-<
            (0x3E, mods) if (mods & NEOMACS_META_MASK) != 0 => "(end-of-buffer)",       // M->

            // Unicode characters (non-ASCII, no modifiers) → self-insert
            (key, 0) if key >= 0x100 && key < 0xFF00 => {
                eval.set_variable(
                    "last-command-event",
                    neovm_core::emacs_core::Value::Int(keysym as i64),
                );
                "(self-insert-command 1)"
            }

            // Escape alone — ignore for now
            (XK_ESCAPE, 0) => {
                tracing::debug!("neomacs_rust_handle_key: ESC pressed");
                return 0;
            }

            _ => {
                tracing::debug!(
                    "neomacs_rust_handle_key: unhandled keysym=0x{:X} mods=0x{:X}",
                    keysym,
                    modifiers
                );
                return 1; // not handled
            }
        };

        // Execute the command
        match neovm_core::emacs_core::parse_forms(command) {
            Ok(forms) => {
                for form in &forms {
                    if let Err(e) = eval.eval_expr(form) {
                        tracing::debug!(
                            "neomacs_rust_handle_key: command '{}' error: {:?}",
                            command,
                            e
                        );
                        // Non-fatal — some commands may fail (e.g., kill-region with no mark)
                        return 0;
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    "neomacs_rust_handle_key: parse error for '{}': {}",
                    command,
                    e
                );
                return -1;
            }
        }

        0
    }));

    match result {
        Ok(code) => code,
        Err(e) => {
            let msg = if let Some(s) = e.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            tracing::error!("neomacs_rust_handle_key: panic: {}", msg);
            -1
        }
    }
}

/// Get a shared reference to the Rust Evaluator.
///
/// # Safety
/// Only call from the main thread. The returned reference is valid until
/// the next mutable access (eval call).
pub(crate) unsafe fn get_evaluator() -> Option<&'static neovm_core::emacs_core::Evaluator> {
    (*std::ptr::addr_of!(RUST_EVALUATOR)).as_ref()
}

/// Get a mutable reference to the Rust Evaluator.
///
/// # Safety
/// Only call from the main thread.
pub(crate) unsafe fn get_evaluator_mut() -> Option<&'static mut neovm_core::emacs_core::Evaluator> {
    (*std::ptr::addr_of_mut!(RUST_EVALUATOR)).as_mut()
}

/// Bootstrap the Rust Evaluator with an initial frame and *scratch* buffer.
///
/// Creates a frame matching the given pixel dimensions and a *scratch*
/// buffer with initial content.  This must be called after
/// `neomacs_rust_eval_init()` and before the first
/// `neomacs_rust_layout_frame_neovm()` call so that the layout engine has
/// a frame/window/buffer to render.
///
/// Returns 0 on success, -1 on error.
///
/// # Safety
/// Must be called from the Emacs main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_bootstrap_frame(
    width: c_int,
    height: c_int,
    char_width: f32,
    char_height: f32,
    font_pixel_size: f32,
) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let eval = match (*std::ptr::addr_of_mut!(RUST_EVALUATOR)).as_mut() {
            Some(e) => e,
            None => {
                tracing::error!("neomacs_rust_bootstrap_frame: evaluator not initialized");
                return -1;
            }
        };

        eval.setup_thread_locals();

        // Set 0-based buffer positions (neovm-core convention).
        // begv=0 (start), zv=char_count (end), pt=char_count (end).
        fn set_buffer_positions(buf: &mut neovm_core::buffer::Buffer) {
            let cc = buf.text.char_count();
            buf.begv = 0;
            buf.zv = cc;
            buf.pt = cc;
        }

        // Create the *scratch* buffer
        let buf_id = eval.buffer_manager_mut().create_buffer("*scratch*");
        if let Some(buf) = eval.buffer_manager_mut().get_mut(buf_id) {
            let content = ";; This buffer is for text that is not saved, and for Lisp evaluation.\n;; To create a file, visit it with \\[find-file] and enter text in its buffer.\n\n";
            buf.text.insert_str(0, content);
            set_buffer_positions(buf);
        }
        eval.buffer_manager_mut().set_current(buf_id);

        // Create a *Messages* buffer
        let msg_buf_id = eval.buffer_manager_mut().create_buffer("*Messages*");
        if let Some(buf) = eval.buffer_manager_mut().get_mut(msg_buf_id) {
            set_buffer_positions(buf);
        }

        // Create the initial frame with a minibuffer buffer
        let mini_buf_id = eval.buffer_manager_mut().create_buffer(" *Minibuf-0*");
        if let Some(buf) = eval.buffer_manager_mut().get_mut(mini_buf_id) {
            set_buffer_positions(buf);
        }

        let frame_id =
            eval.frame_manager_mut()
                .create_frame("F1", width as u32, height as u32, buf_id);

        // Set frame font metrics
        if let Some(frame) = eval.frame_manager_mut().get_mut(frame_id) {
            frame.char_width = if char_width > 0.0 { char_width } else { 8.0 };
            frame.char_height = if char_height > 0.0 { char_height } else { 16.0 };
            frame.font_pixel_size = if font_pixel_size > 0.0 {
                font_pixel_size
            } else {
                14.0
            };

            // Set 0-based window_start and point on root window
            if let neovm_core::window::Window::Leaf {
                window_start,
                point,
                ..
            } = &mut frame.root_window
            {
                *window_start = 0;
                *point = 0;
            }

            // Set up the minibuffer leaf with the minibuffer buffer
            if let Some(ref mut mini) = frame.minibuffer_leaf {
                if let neovm_core::window::Window::Leaf {
                    buffer_id,
                    window_start,
                    point,
                    ..
                } = mini
                {
                    *buffer_id = mini_buf_id;
                    *window_start = 0;
                    *point = 0;
                }
            }

            // Adjust root window bounds to leave room for the minibuffer
            let mini_height = frame.char_height;
            let root_height = (height as f32 - mini_height).max(0.0);
            frame.root_window.set_bounds(neovm_core::window::Rect::new(
                0.0,
                0.0,
                width as f32,
                root_height,
            ));

            // Set minibuffer bounds at the bottom
            if let Some(ref mut mini) = frame.minibuffer_leaf {
                mini.set_bounds(neovm_core::window::Rect::new(
                    0.0,
                    root_height,
                    width as f32,
                    mini_height,
                ));
            }
        }

        tracing::info!(
            "neomacs_rust_bootstrap_frame: created frame {:?} ({}x{}) with *scratch* buffer {:?}",
            frame_id,
            width,
            height,
            buf_id
        );
        0
    }));

    match result {
        Ok(code) => code,
        Err(e) => {
            let msg = if let Some(s) = e.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            tracing::error!("neomacs_rust_bootstrap_frame: panic: {}", msg);
            -1
        }
    }
}

/// Sync frame dimensions from C to the Rust Evaluator.
///
/// Called when the C frame is resized so the neovm-core frame stays in sync.
///
/// Returns 0 on success, -1 on error.
///
/// # Safety
/// Must be called from the Emacs main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_sync_frame_size(
    width: c_int,
    height: c_int,
    char_width: f32,
    char_height: f32,
    font_pixel_size: f32,
) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let eval = match (*std::ptr::addr_of_mut!(RUST_EVALUATOR)).as_mut() {
            Some(e) => e,
            None => return -1,
        };

        let frame_id = match eval.frame_manager().selected_frame() {
            Some(f) => f.id,
            None => return -1,
        };

        if let Some(frame) = eval.frame_manager_mut().get_mut(frame_id) {
            frame.width = width as u32;
            frame.height = height as u32;
            frame.char_width = if char_width > 0.0 {
                char_width
            } else {
                frame.char_width
            };
            frame.char_height = if char_height > 0.0 {
                char_height
            } else {
                frame.char_height
            };
            frame.font_pixel_size = if font_pixel_size > 0.0 {
                font_pixel_size
            } else {
                frame.font_pixel_size
            };

            // Update window bounds
            let mini_height = frame.char_height;
            let root_height = (height as f32 - mini_height).max(0.0);
            frame.root_window.set_bounds(neovm_core::window::Rect::new(
                0.0,
                0.0,
                width as f32,
                root_height,
            ));
            if let Some(ref mut mini) = frame.minibuffer_leaf {
                mini.set_bounds(neovm_core::window::Rect::new(
                    0.0,
                    root_height,
                    width as f32,
                    mini_height,
                ));
            }
        }

        0
    }));

    match result {
        Ok(code) => code,
        Err(_) => -1,
    }
}

/// Set the Evaluator's `load-path` from a colon-separated string of directories.
///
/// Returns 0 on success, -1 on error.
///
/// # Safety
/// `paths` must be a valid, NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_set_load_path(paths: *const c_char) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if paths.is_null() {
            tracing::error!("neomacs_rust_set_load_path: null paths");
            return -1;
        }

        let eval = match (*std::ptr::addr_of_mut!(RUST_EVALUATOR)).as_mut() {
            Some(e) => e,
            None => {
                tracing::error!("neomacs_rust_set_load_path: evaluator not initialized");
                return -1;
            }
        };

        eval.setup_thread_locals();

        let paths_str = match CStr::from_ptr(paths).to_str() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("neomacs_rust_set_load_path: invalid UTF-8: {}", e);
                return -1;
            }
        };

        // Build a Lisp list of directory strings from the colon-separated input.
        let dirs: Vec<neovm_core::emacs_core::Value> = paths_str
            .split(':')
            .filter(|s| !s.is_empty())
            .map(|s| neovm_core::emacs_core::Value::string(s))
            .collect();

        let list = neovm_core::emacs_core::Value::list(dirs);
        eval.set_variable("load-path", list);

        tracing::info!(
            "neomacs_rust_set_load_path: set load-path from '{}'",
            paths_str
        );
        0
    }));

    match result {
        Ok(code) => code,
        Err(_) => {
            tracing::error!("neomacs_rust_set_load_path: panic during set");
            -1
        }
    }
}
