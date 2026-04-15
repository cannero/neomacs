//! Reader/printer builtins: read-from-string, read, prin1-to-string (enhanced),
//! format-spec, and various interactive-input stubs.

use super::custom::CustomManager;
use super::error::{EvalResult, Flow, signal};
use super::intern::{SymId, intern, resolve_sym};
// storage imports removed — now using emacs_char directly
use super::symbol::Obarray;
use super::value::*;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(super::builtins::lisp_string_to_runtime_string(*value)),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

fn expect_lisp_string(value: &Value) -> Result<crate::heap_types::LispString, Flow> {
    match value.kind() {
        ValueKind::String => Ok(value
            .as_lisp_string()
            .expect("ValueKind::String must carry LispString payload")
            .clone()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

fn expect_number(value: &Value) -> Result<(), Flow> {
    if value.is_number() {
        return Ok(());
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("numberp"), *value],
    ))
}

pub(crate) fn parse_optional_read_seconds_arg(
    value: Option<&Value>,
) -> Result<Option<Duration>, Flow> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_nil() {
        return Ok(None);
    }

    let seconds = value.as_number_f64().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *value],
        )
    })?;
    if seconds <= 0.0 {
        return Ok(Some(Duration::ZERO));
    }

    Ok(Some(Duration::from_secs_f64(seconds)))
}

fn expect_initial_input_stringish(value: &Value) -> Result<(), Flow> {
    match value.kind() {
        ValueKind::Nil | ValueKind::String => Ok(()),
        ValueKind::Cons => {
            let pair_car = value.cons_car();
            let pair_cdr = value.cons_cdr();
            if !pair_car.is_string() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), pair_car],
                ));
            }
            Ok(())
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

fn expect_completing_read_initial_input(value: &Value) -> Result<(), Flow> {
    match value.kind() {
        ValueKind::Nil | ValueKind::String => Ok(()),
        ValueKind::Cons => {
            let pair_car = value.cons_car();
            let pair_cdr = value.cons_cdr();
            if !pair_car.is_string() {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), pair_car],
                ));
            }
            if !(pair_cdr.is_fixnum() || pair_cdr.as_char().is_some()) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), pair_cdr],
                ));
            }
            Ok(())
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

#[derive(Clone, Copy, Debug)]
struct ActiveMinibufferWindowState {
    frame_id: crate::window::FrameId,
    minibuffer_window_id: crate::window::WindowId,
    previous_selected_window: crate::window::WindowId,
    previous_minibuffer_buffer: Option<crate::buffer::BufferId>,
    previous_minibuffer_window_start: usize,
    previous_minibuffer_point: usize,
    previous_minibuffer_selected_window: Option<crate::window::WindowId>,
    previous_active_minibuffer_window: Option<crate::window::WindowId>,
}

fn activate_minibuffer_window_in_state(
    frames: &mut crate::window::FrameManager,
    buffers: &mut crate::buffer::BufferManager,
    minibuffer_selected_window: &mut Option<crate::window::WindowId>,
    active_minibuffer_window: &mut Option<crate::window::WindowId>,
    minibuf_id: crate::buffer::BufferId,
) -> Option<ActiveMinibufferWindowState> {
    let frame_id = super::window_cmds::ensure_selected_frame_id_in_state(frames, buffers);
    let frame = frames.get(frame_id)?;
    let minibuffer_window_id = frame.minibuffer_window?;
    let previous_selected_window = frame.selected_window;
    let mut previous_minibuffer_buffer = None;
    let mut previous_minibuffer_window_start = 1;
    let mut previous_minibuffer_point = 1;
    if let Some(crate::window::Window::Leaf {
        buffer_id,
        window_start,
        point,
        ..
    }) = frame.find_window(minibuffer_window_id)
    {
        previous_minibuffer_buffer = Some(*buffer_id);
        previous_minibuffer_window_start = *window_start;
        previous_minibuffer_point = *point;
    }

    let saved = ActiveMinibufferWindowState {
        frame_id,
        minibuffer_window_id,
        previous_selected_window,
        previous_minibuffer_buffer,
        previous_minibuffer_window_start,
        previous_minibuffer_point,
        previous_minibuffer_selected_window: *minibuffer_selected_window,
        previous_active_minibuffer_window: *active_minibuffer_window,
    };

    if let Some(frame) = frames.get_mut(frame_id) {
        if let Some(window) = frame.find_window_mut(minibuffer_window_id) {
            window.set_buffer(minibuf_id);
        }
        let _ = frame.select_window(minibuffer_window_id);
    }
    buffers.switch_current(minibuf_id);
    *minibuffer_selected_window = Some(previous_selected_window);
    *active_minibuffer_window = Some(minibuffer_window_id);
    Some(saved)
}

fn activate_minibuffer_window(
    eval: &mut super::eval::Context,
    minibuf_id: crate::buffer::BufferId,
) -> Option<ActiveMinibufferWindowState> {
    activate_minibuffer_window_in_state(
        &mut eval.frames,
        &mut eval.buffers,
        &mut eval.minibuffer_selected_window,
        &mut eval.active_minibuffer_window,
        minibuf_id,
    )
}

fn restore_minibuffer_window_in_state(
    frames: &mut crate::window::FrameManager,
    minibuffer_selected_window: &mut Option<crate::window::WindowId>,
    active_minibuffer_window: &mut Option<crate::window::WindowId>,
    saved: ActiveMinibufferWindowState,
) {
    if let Some(frame) = frames.get_mut(saved.frame_id) {
        if let Some(window) = frame.find_window_mut(saved.minibuffer_window_id) {
            if let Some(prev_buffer_id) = saved.previous_minibuffer_buffer {
                window.set_buffer(prev_buffer_id);
                if let crate::window::Window::Leaf {
                    window_start,
                    point,
                    ..
                } = window
                {
                    *window_start = saved.previous_minibuffer_window_start.max(1);
                    *point = saved.previous_minibuffer_point.max(1);
                }
            }
        }
        let _ = frame.select_window(saved.previous_selected_window);
    }
    *minibuffer_selected_window = saved.previous_minibuffer_selected_window;
    *active_minibuffer_window = saved.previous_active_minibuffer_window;
}

fn restore_minibuffer_window(eval: &mut super::eval::Context, saved: ActiveMinibufferWindowState) {
    restore_minibuffer_window_in_state(
        &mut eval.frames,
        &mut eval.minibuffer_selected_window,
        &mut eval.active_minibuffer_window,
        saved,
    )
}

fn signal_invalid_read_syntax_in_buffer(
    buffer_text: &str,
    absolute_error_pos: usize,
    message: String,
) -> Flow {
    let clamped_pos = absolute_error_pos.min(buffer_text.len());
    let prefix = &buffer_text[..clamped_pos];
    let line = prefix.bytes().filter(|b| *b == b'\n').count() as i64 + 1;
    let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() as i64;
    signal(
        "invalid-read-syntax",
        vec![
            Value::string(message),
            Value::fixnum(line),
            Value::fixnum(column),
        ],
    )
}

fn stdin_end_of_file_error() -> Flow {
    signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    )
}

// ---------------------------------------------------------------------------
// 1. read-from-string
// ---------------------------------------------------------------------------

/// `(read-from-string STRING &optional START END)`
///
/// Parse a single Lisp object from STRING starting at position START (default 0).
/// Returns `(OBJECT . END-POSITION)` where END-POSITION is the character index
/// after the parsed object.
pub(crate) fn builtin_read_from_string(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    read_from_string_impl(&ctx.obarray, args)
}

pub(crate) fn read_from_string_impl(
    obarray: &crate::emacs_core::symbol::Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-from-string", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("read-from-string"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    let full_string = expect_lisp_string(&args[0])?;
    let full_storage = super::builtins::runtime_string_from_lisp_string(&full_string);

    // GNU Emacs `Fread_from_string` (`src/lread.c:2514`) treats START and
    // END as character indices into STRING (validated via
    // `validate_subarray` against `SCHARS (string)`), translates them to
    // byte offsets through `string_char_to_byte`, and reports
    // FINAL-STRING-INDEX as a *character* index too. Indexing by raw
    // UTF-8 byte length here was a long-standing bug (audit §11.6) that
    // would either panic on multibyte input (slicing mid-codepoint) or
    // return a byte offset where elisp expected a character count.
    let full_string_bytes = full_string.as_bytes();
    let char_count = full_string.schars();

    let start_arg = args.get(1).cloned().unwrap_or(Value::NIL);
    let end_arg = args.get(2).cloned().unwrap_or(Value::NIL);
    let to_char_index = |value: &Value| -> Result<usize, Flow> {
        match value.kind() {
            ValueKind::Nil => Ok(0),
            ValueKind::Fixnum(n) => {
                let idx = if n < 0 { (char_count as i64) + n } else { n };
                if idx < 0 || idx > char_count as i64 {
                    return Err(signal(
                        "args-out-of-range",
                        vec![args[0], start_arg, end_arg],
                    ));
                }
                Ok(idx as usize)
            }
            _ => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), *value],
            )),
        }
    };
    let start_char = if args.len() > 1 {
        to_char_index(&start_arg)?
    } else {
        0
    };
    let end_char = if args.len() > 2 {
        to_char_index(&end_arg)?
    } else {
        char_count
    };

    if start_char > end_char {
        return Err(signal(
            "args-out-of-range",
            vec![args[0], start_arg, end_arg],
        ));
    }

    let start_byte = if full_string.is_multibyte() {
        crate::emacs_core::emacs_char::char_to_byte_pos(full_string_bytes, start_char)
    } else {
        start_char
    };
    let end_byte = if full_string.is_multibyte() {
        crate::emacs_core::emacs_char::char_to_byte_pos(full_string_bytes, end_char)
    } else {
        end_char
    };

    let start_storage = crate::emacs_core::string_escape::storage_logical_byte_to_storage_byte(
        &full_storage,
        start_byte,
    );
    let end_storage = crate::emacs_core::string_escape::storage_logical_byte_to_storage_byte(
        &full_storage,
        end_byte,
    );

    let substring = &full_storage[start_storage..end_storage];
    if starts_with_hash_skip_dispatch(substring) {
        return Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        ));
    }

    // Mirror GNU `Fread_from_string` (`src/lread.c`): the `#$` reader
    // shorthand expands to the *current* value of the elisp variable
    // `load-file-name`. NeoVM's reader keeps this in a thread-local
    // (set by `with_load_context` in load.rs); when called from
    // `read-from-string` outside of a load, the elisp obarray binding
    // is the only source of truth, so bridge it across before reading.
    let saved_reader_load_file_name = super::value_reader::get_reader_load_file_name_public();
    let load_file_name_value = obarray.symbol_value("load-file-name").copied();
    let load_file_name_for_reader = match load_file_name_value {
        Some(v) if !v.is_nil() => Some(v),
        _ => None,
    };
    super::value_reader::set_reader_load_file_name(load_file_name_for_reader);

    let read_result = super::value_reader::read_one_with_source_multibyte(
        substring,
        full_string.is_multibyte(),
        0,
    );

    super::value_reader::set_reader_load_file_name(saved_reader_load_file_name);

    let (value, end_pos) = read_result
        .map_err(|e| {
            if e.message.contains("unterminated") || e.message.contains("end of input") {
                signal(
                    "end-of-file",
                    vec![Value::string("End of file during parsing")],
                )
            } else {
                signal(
                    "invalid-read-syntax",
                    vec![Value::string(e.message.clone())],
                )
            }
        })?
        .ok_or_else(|| {
            signal(
                "end-of-file",
                vec![Value::string("End of file during parsing")],
            )
        })?;

    // `read_one` returns a byte offset into `substring`. Convert it back
    // into a character index in the original STRING so the returned
    // FINAL-STRING-INDEX matches GNU's contract.
    let absolute_end_byte = start_byte
        + crate::emacs_core::string_escape::storage_byte_to_logical_byte(substring, end_pos);
    let absolute_end_char = if full_string.is_multibyte() {
        crate::emacs_core::emacs_char::byte_to_char_pos(full_string_bytes, absolute_end_byte)
    } else {
        absolute_end_byte
    };

    Ok(Value::cons(value, Value::fixnum(absolute_end_char as i64)))
}

fn starts_with_hash_skip_dispatch(input: &str) -> bool {
    let bytes = input.as_bytes();
    let pos = skip_ws_comments(input, 0);
    pos + 1 < bytes.len() && bytes[pos] == b'#' && bytes[pos + 1] == b'@'
}

fn skip_ws_comments(input: &str, mut pos: usize) -> usize {
    let bytes = input.as_bytes();
    loop {
        if pos >= bytes.len() {
            return pos;
        }
        let ch = bytes[pos];
        if ch.is_ascii_whitespace() {
            pos += 1;
            continue;
        }
        if ch == b';' {
            // line comment
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            if pos < bytes.len() {
                pos += 1; // skip newline
            }
            continue;
        }
        if ch == b'#' && pos + 1 < bytes.len() && bytes[pos + 1] == b'|' {
            // block comment
            pos += 2;
            let mut depth = 1;
            while depth > 0 && pos < bytes.len() {
                if bytes[pos] == b'#' && pos + 1 < bytes.len() && bytes[pos + 1] == b'|' {
                    depth += 1;
                    pos += 2;
                } else if bytes[pos] == b'|' && pos + 1 < bytes.len() && bytes[pos + 1] == b'#' {
                    depth -= 1;
                    pos += 2;
                } else {
                    pos += 1;
                }
            }
            continue;
        }
        return pos;
    }
}

// ---------------------------------------------------------------------------
// 2. read
// ---------------------------------------------------------------------------

/// `(read &optional STREAM)`
///
/// Read one Lisp expression from STREAM.
/// - If STREAM is a string, read from that string (equivalent to car of read-from-string).
/// - If STREAM is nil, would read from stdin (returns nil in non-interactive mode).
/// - If STREAM is a buffer, read from buffer at point.
pub fn builtin_read(ctx: &mut crate::emacs_core::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_max_args("read", &args, 1)?;

    if args.is_empty() || args[0].is_nil() {
        // In batch/non-interactive runs, stdin-backed read signals EOF.
        return Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        ));
    }

    match args[0].kind() {
        ValueKind::String => {
            // Read from string
            let result = read_from_string_impl(&ctx.obarray, args)?;
            // Return just the car (the parsed object)
            match result.kind() {
                ValueKind::Cons => {
                    let pair_car = result.cons_car();
                    let pair_cdr = result.cons_cdr();
                    Ok(pair_car)
                }
                _ => Ok(result),
            }
        }
        ValueKind::Veclike(VecLikeType::Buffer) => {
            // Read from buffer at point
            let buf_id = args[0].as_buffer_id().unwrap();
            let (text, source_multibyte, pt, begv_byte) = {
                let buf = &mut ctx
                    .buffers
                    .get(buf_id)
                    .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
                let text = buf.buffer_substring_lisp_string(buf.point_min(), buf.point_max());
                (
                    crate::emacs_core::builtins::runtime_string_from_lisp_string(&text),
                    text.is_multibyte(),
                    buf.pt_byte,
                    buf.begv_byte,
                )
            };
            let start = crate::emacs_core::string_escape::storage_logical_byte_to_storage_byte(
                &text,
                pt.saturating_sub(begv_byte),
            );
            if start >= text.len() {
                return Err(signal(
                    "end-of-file",
                    vec![Value::string("End of file during parsing")],
                ));
            }
            let substring = &text[start..];
            let (value, end_offset) =
                super::value_reader::read_one_with_source_multibyte(substring, source_multibyte, 0)
                    .map_err(|e| {
                        if e.message.contains("unterminated") || e.message.contains("end of input")
                        {
                            signal(
                                "end-of-file",
                                vec![Value::string("End of file during parsing")],
                            )
                        } else {
                            signal_invalid_read_syntax_in_buffer(
                                &text,
                                start + e.position,
                                e.message,
                            )
                        }
                    })?
                    .ok_or_else(|| {
                        signal(
                            "end-of-file",
                            vec![Value::string("End of file during parsing")],
                        )
                    })?;
            // Advance point past the read form
            let new_pt = pt
                + crate::emacs_core::string_escape::storage_byte_to_logical_byte(
                    substring, end_offset,
                );
            let _ = &mut ctx.buffers.goto_buffer_byte(buf_id, new_pt);
            Ok(value)
        }
        ValueKind::Symbol(id) => Err(signal(
            "void-function",
            vec![Value::symbol(resolve_sym(id))],
        )),
        ValueKind::T => Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        )),
        _ => {
            // Unsupported stream source type for read-char function protocol.
            Err(signal("invalid-function", vec![args[0]]))
        }
    }
}

// ---------------------------------------------------------------------------
// 5. read-from-minibuffer
// ---------------------------------------------------------------------------

/// `(read-from-minibuffer PROMPT &optional INITIAL KEYMAP READ HIST DEFAULT INHERIT-INPUT-METHOD)`
///
/// Read a string from the minibuffer.
/// In interactive mode, sets up the minibuffer buffer, enters recursive-edit,
/// and returns the user's input when they press RET (exit-minibuffer).
/// In batch mode, signals `end-of-file`.
pub(crate) fn builtin_read_from_minibuffer(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_read_from_minibuffer_in_runtime(eval, &args)?;
    finish_read_from_minibuffer_in_eval(eval, &args)
}

pub(crate) fn finish_read_from_minibuffer_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    let eval_ptr = std::ptr::NonNull::from(&mut *eval);
    let command_loop_depth = eval.recursive_command_loop_depth();
    finish_read_from_minibuffer_in_state_with_recursive_edit(
        &mut eval.obarray,
        &mut eval.buffers,
        &mut eval.frames,
        &mut eval.minibuffers,
        &mut eval.minibuffer_selected_window,
        &mut eval.active_minibuffer_window,
        command_loop_depth,
        args,
        move || unsafe {
            eval_ptr
                .as_ptr()
                .as_mut()
                .unwrap()
                .run_hook_if_bound("minibuffer-setup-hook")
        },
        move || unsafe {
            match eval_ptr
                .as_ptr()
                .as_mut()
                .unwrap()
                .run_hook_if_bound("minibuffer-exit-hook")
            {
                Ok(value) => Ok(value),
                Err(Flow::Signal(_)) => Ok(Value::NIL),
                Err(flow) => Err(flow),
            }
        },
        move || unsafe {
            eval_ptr
                .as_ptr()
                .as_mut()
                .unwrap()
                .minibuffer_command_loop_inner()
        },
    )
}

pub(crate) fn builtin_read_from_minibuffer_in_runtime(
    runtime: &impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<(), Flow> {
    expect_min_args("read-from-minibuffer", args, 1)?;
    expect_max_args("read-from-minibuffer", args, 7)?;
    let _prompt = expect_string(&args[0])?;
    if let Some(initial) = args.get(1) {
        expect_initial_input_stringish(initial)?;
    }

    if runtime.has_input_receiver() {
        Ok(())
    } else {
        Err(stdin_end_of_file_error())
    }
}

/// Shared runtime setup/teardown for `read-from-minibuffer`.
///
/// GNU's `read_minibuf` is a C/runtime path that only enters the command
/// loop for the actual recursive edit. This helper mirrors that shape: it
/// performs buffer/window setup and final result handling in shared runtime
/// state, and delegates only the recursive edit itself to the callback.
pub(crate) fn finish_read_from_minibuffer_in_state_with_recursive_edit(
    obarray: &mut super::symbol::Obarray,
    buffers: &mut crate::buffer::BufferManager,
    frames: &mut crate::window::FrameManager,
    minibuffers: &mut crate::emacs_core::minibuffer::MinibufferManager,
    minibuffer_selected_window: &mut Option<crate::window::WindowId>,
    active_minibuffer_window: &mut Option<crate::window::WindowId>,
    recursive_depth: usize,
    args: &[Value],
    mut run_setup_hook: impl FnMut() -> EvalResult,
    mut run_exit_hook: impl FnMut() -> EvalResult,
    mut run_recursive_edit: impl FnMut() -> EvalResult,
) -> EvalResult {
    // Check inhibit-interaction — GNU Emacs signals an error when any
    // interactive read is attempted while this variable is non-nil.
    if obarray
        .symbol_value("inhibit-interaction")
        .is_some_and(|v| v.is_truthy())
    {
        return Err(signal(
            "inhibited-interaction",
            vec![Value::string(
                "Attempt to interact with user while inhibit-interaction is non-nil",
            )],
        ));
    }

    let prompt = expect_string(&args[0])?;
    // Extract optional arguments
    let initial_input = args.get(1).and_then(|v| match v.kind() {
        ValueKind::String => Some(super::builtins::lisp_string_to_runtime_string(*v)),
        _ => None,
    });
    let keymap_arg = args.get(2).copied().unwrap_or(Value::NIL);
    let read_arg = args.get(3).copied().unwrap_or(Value::NIL);
    let history_name = minibuffer_history_name(args.get(4));
    let default_val = args.get(5).copied().unwrap_or(Value::NIL);

    // Save state
    let saved_buffer_id = buffers.current_buffer().map(|b| b.id);

    // Find or create *Minibuf-N* buffer
    let minibuf_depth = minibuffers.depth() + 1;
    let minibuf_name = format!(" *Minibuf-{}*", minibuf_depth);
    let minibuf_id = buffers
        .find_buffer_by_name(&minibuf_name)
        .unwrap_or_else(|| buffers.create_buffer(&minibuf_name));

    // Clear the minibuffer buffer and insert prompt + initial input
    let prompt_byte_len;
    {
        let buf = buffers.get_mut(minibuf_id).unwrap();
        let text_len = buf.text.len();
        if text_len > 0 {
            buf.text.delete_range(0, text_len);
        }
        buf.text.insert_str(0, &prompt);
        let prompt_storage_len = prompt.len();
        prompt_byte_len = crate::emacs_core::string_escape::storage_byte_len(&prompt);
        if let Some(ref initial) = initial_input {
            buf.text.insert_str(prompt_storage_len, initial);
        }
        let total_len = buf.total_bytes();
        buf.widen();
        buf.goto_byte(total_len); // cursor at end of initial input
    }

    let active_window_state = activate_minibuffer_window_in_state(
        frames,
        buffers,
        minibuffer_selected_window,
        active_minibuffer_window,
        minibuf_id,
    );
    if active_window_state.is_none() {
        // Batch/no-frame fallback: still switch current buffer so tests without
        // a realized GUI frame can exercise the minibuffer logic.
        buffers.switch_current(minibuf_id);
    }
    tracing::debug!(
        "read-from-minibuffer: prompt={:?} minibuf_id={:?} current_buffer={:?} active_window={:?} selected_window={:?}",
        prompt,
        minibuf_id,
        buffers.current_buffer_id(),
        *active_minibuffer_window,
        frames.selected_frame().map(|frame| frame.selected_window)
    );

    let enable_recursive = obarray
        .symbol_value("enable-recursive-minibuffers")
        .copied()
        .unwrap_or(Value::NIL)
        .is_truthy();
    minibuffers.set_enable_recursive(enable_recursive);
    let state = minibuffers.read_from_minibuffer(
        minibuf_id,
        &prompt,
        initial_input.as_deref(),
        history_name.as_deref(),
    )?;
    state.command_loop_depth = recursive_depth;

    // Set local keymap: use KEYMAP arg if provided, otherwise minibuffer-local-map
    let minibuf_keymap = if !keymap_arg.is_nil() {
        keymap_arg
    } else {
        obarray
            .symbol_value("minibuffer-local-map")
            .copied()
            .unwrap_or(Value::NIL)
    };
    let _ = buffers.set_current_local_map(minibuf_keymap);

    // Set minibuffer-related variables
    obarray.set_symbol_value("minibuffer-prompt", Value::string(prompt));
    obarray.set_symbol_value("minibuffer-depth", Value::fixnum(minibuf_depth as i64));

    run_setup_hook()?;

    // Enter recursive edit — the command loop runs until exit-minibuffer throws 'exit.
    let edit_result = run_recursive_edit();

    // Read the minibuffer contents (everything after the prompt)
    let result_string = if let Some(buf) = buffers.get(minibuf_id) {
        let total_len = buf.total_bytes();
        if total_len > prompt_byte_len {
            let text = buf.buffer_substring_lisp_string(prompt_byte_len, total_len);
            crate::emacs_core::builtins::runtime_string_from_lisp_string(&text)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let _ = buffers.switch_current(minibuf_id);
    let exit_hook_result = match run_exit_hook() {
        Err(Flow::Signal(_)) => Ok(Value::NIL),
        other => other,
    };

    match &edit_result {
        Ok(_) => {
            let _ = minibuffers.exit_minibuffer();
        }
        Err(Flow::Throw { tag, value }) if tag.is_symbol_named("exit") => {
            if value.is_truthy() {
                minibuffers.abort_minibuffer();
            } else {
                let _ = minibuffers.exit_minibuffer();
            }
        }
        Err(_) => {
            minibuffers.abort_minibuffer();
        }
    }

    // Restore state
    if let Some(saved) = active_window_state {
        restore_minibuffer_window_in_state(
            frames,
            minibuffer_selected_window,
            active_minibuffer_window,
            saved,
        );
    }
    if let Some(buf_id) = saved_buffer_id {
        buffers.switch_current(buf_id);
    }
    tracing::debug!(
        "read-from-minibuffer: restored current_buffer={:?} active_window={:?} selected_window={:?}",
        buffers.current_buffer_id(),
        *active_minibuffer_window,
        frames.selected_frame().map(|frame| frame.selected_window)
    );
    obarray.set_symbol_value(
        "minibuffer-depth",
        Value::fixnum(minibuffers.depth() as i64),
    );
    exit_hook_result?;

    // Handle the recursive edit result
    match edit_result {
        Ok(_) | Err(Flow::Throw { .. }) => {
            // Normal exit (throw 'exit from exit-minibuffer)
            // If READ arg is non-nil, evaluate the result as a Lisp expression
            if !read_arg.is_nil() && !result_string.is_empty() {
                // READ is non-nil: parse the result string as a Lisp expression
                // (like calling (read STRING)) and return the parsed object.
                let read_result =
                    read_from_string_impl(obarray, vec![Value::string(&result_string)])?;
                // read-from-string returns (OBJECT . END-POS), extract OBJECT
                if read_result.is_cons() {
                    return Ok(read_result.cons_car());
                }
                return Ok(read_result);
            }

            // If result is empty and DEFAULT is provided, use it
            if result_string.is_empty() && !default_val.is_nil() {
                return Ok(default_val);
            }

            Ok(Value::string(result_string))
        }
        Err(flow) => Err(flow),
    }
}

fn minibuffer_history_name(hist_arg: Option<&Value>) -> Option<String> {
    match hist_arg.copied().unwrap_or(Value::NIL).kind() {
        ValueKind::Symbol(id) => Some(resolve_sym(id).to_string()),
        ValueKind::Cons => hist_arg
            .copied()
            .unwrap_or(Value::NIL)
            .cons_car()
            .as_symbol_name()
            .map(str::to_string),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 6. read-string
// ---------------------------------------------------------------------------

/// `(read-string PROMPT &optional INITIAL HISTORY DEFAULT INHERIT-INPUT-METHOD)`
///
/// Read a string from the minibuffer.  Delegates to `read-from-minibuffer`.
pub(crate) fn builtin_read_string(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_read_string_in_runtime(eval, &args)?;
    finish_read_string_in_eval(eval, &args)
}

pub(crate) fn finish_read_string_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_read_string_with_minibuffer(args, |minibuffer_args| {
        finish_read_from_minibuffer_in_eval(eval, minibuffer_args)
    })
}

pub(crate) fn builtin_read_string_in_runtime(
    runtime: &impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<(), Flow> {
    expect_min_args("read-string", args, 1)?;
    expect_max_args("read-string", args, 5)?;
    let prompt = args[0];
    if let Some(initial) = args.get(1) {
        expect_initial_input_stringish(initial)?;
    }

    let initial = args.get(1).copied().unwrap_or(Value::NIL);
    let history = args.get(2).copied().unwrap_or(Value::NIL);
    let default = args.get(3).copied().unwrap_or(Value::NIL);
    let inherit = args.get(4).copied().unwrap_or(Value::NIL);
    let minibuffer_args = [
        prompt,
        initial,
        Value::NIL,
        Value::NIL,
        history,
        default,
        inherit,
    ];
    builtin_read_from_minibuffer_in_runtime(runtime, &minibuffer_args)
}

pub(crate) fn finish_read_string_with_minibuffer(
    args: &[Value],
    mut read_from_minibuffer: impl FnMut(&[Value]) -> EvalResult,
) -> EvalResult {
    let prompt = args[0];

    // (read-from-minibuffer PROMPT INITIAL nil nil HIST DEFAULT INHERIT-INPUT-METHOD)
    let initial = args.get(1).copied().unwrap_or(Value::NIL);
    let history = args.get(2).copied().unwrap_or(Value::NIL);
    let default = args.get(3).copied().unwrap_or(Value::NIL);
    let inherit = args.get(4).copied().unwrap_or(Value::NIL);

    let minibuffer_args = [
        prompt,
        initial,
        Value::NIL,
        Value::NIL,
        history,
        default,
        inherit,
    ];
    read_from_minibuffer(&minibuffer_args)
}

pub(crate) fn finish_read_string_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_read_string_in_runtime(shared, args)?;
    finish_read_string_with_minibuffer(args, |minibuffer_args| {
        finish_read_from_minibuffer_in_vm_runtime(shared, vm_gc_roots, minibuffer_args)
    })
}

// ---------------------------------------------------------------------------
// 7. read-number
// ---------------------------------------------------------------------------

/// `(read-number PROMPT &optional DEFAULT)`
///
/// Read a numeric value from the minibuffer.
/// Delegates to read-from-minibuffer with READ=t, then validates the result
/// is a number.
pub(crate) fn builtin_read_number(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_read_number_in_runtime(eval, &args)?;
    finish_read_number_in_eval(eval, &args)
}

pub(crate) fn builtin_read_number_in_runtime(
    runtime: &impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<(), Flow> {
    expect_min_args("read-number", args, 1)?;
    expect_max_args("read-number", args, 3)?;
    let prompt = args[0];
    expect_string(&prompt)?;
    if let Some(default) = args.get(1)
        && !default.is_nil()
    {
        expect_number(default)?;
    }
    if runtime.has_input_receiver() {
        Ok(())
    } else {
        Err(stdin_end_of_file_error())
    }
}

fn read_number_minibuffer_args(args: &[Value]) -> [Value; 6] {
    let prompt = args[0];
    let default_val = args.get(1).copied().unwrap_or(Value::NIL);
    [
        prompt,
        Value::NIL,
        Value::NIL,
        Value::T,
        Value::NIL,
        default_val,
    ]
}

fn validate_read_number_result(result: Value) -> EvalResult {
    if result.is_number() {
        return Ok(result);
    }
    Err(signal("error", vec![Value::string("Not a number")]))
}

pub(crate) fn finish_read_number_with_minibuffer(
    args: &[Value],
    mut read_from_minibuffer: impl FnMut(&[Value]) -> EvalResult,
) -> EvalResult {
    let minibuffer_args = read_number_minibuffer_args(args);
    validate_read_number_result(read_from_minibuffer(&minibuffer_args)?)
}

pub(crate) fn finish_read_number_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_read_number_with_minibuffer(args, |minibuffer_args| {
        finish_read_from_minibuffer_in_eval(eval, minibuffer_args)
    })
}

pub(crate) fn finish_read_number_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_read_number_in_runtime(shared, args)?;
    finish_read_number_with_minibuffer(args, |minibuffer_args| {
        finish_read_from_minibuffer_in_vm_runtime(shared, vm_gc_roots, minibuffer_args)
    })
}

// ---------------------------------------------------------------------------
// 8. completing-read
// ---------------------------------------------------------------------------

/// `(completing-read PROMPT COLLECTION &optional PREDICATE REQUIRE-MATCH
///                    INITIAL-INPUT HIST DEF INHERIT-INPUT-METHOD)`
///
/// Read a string from the minibuffer with completion.
/// In interactive mode, delegates to read-from-minibuffer with
/// minibuffer-local-completion-map (or minibuffer-local-must-match-map
/// if REQUIRE-MATCH is non-nil).
pub(crate) fn builtin_completing_read(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_completing_read_in_runtime(eval, &args)?;
    finish_completing_read_in_eval(eval, &args)
}

pub(crate) fn finish_completing_read_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    let minibuffer_args = completing_read_minibuffer_args(eval.obarray(), args);
    let collection = args[1];
    eval.assign("minibuffer-completion-table", collection);
    let predicate = args.get(2).copied().unwrap_or(Value::NIL);
    eval.assign("minibuffer-completion-predicate", predicate);
    let require_match = args.get(3).copied().unwrap_or(Value::NIL);
    eval.assign(
        "minibuffer-completion-confirm",
        completion_confirm_from_require_match(require_match),
    );

    let result = finish_read_from_minibuffer_in_eval(eval, &minibuffer_args);

    eval.assign("minibuffer-completion-table", Value::NIL);
    eval.assign("minibuffer-completion-predicate", Value::NIL);
    eval.assign("minibuffer-completion-confirm", Value::NIL);

    result
}

pub(crate) fn builtin_completing_read_in_runtime(
    runtime: &impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<(), Flow> {
    expect_min_args("completing-read", args, 2)?;
    expect_max_args("completing-read", args, 8)?;
    let prompt = args[0];
    expect_string(&prompt)?;
    if let Some(initial) = args.get(4) {
        expect_completing_read_initial_input(initial)?;
    }

    if runtime.has_input_receiver() {
        Ok(())
    } else {
        Err(stdin_end_of_file_error())
    }
}

pub(crate) fn finish_completing_read_in_state_with_minibuffer(
    obarray: &mut Obarray,
    dynamic: &mut [OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    custom: &crate::emacs_core::custom::CustomManager,
    specpdl: &[crate::emacs_core::eval::SpecBinding],
    args: &[Value],
    mut read_from_minibuffer: impl FnMut(&[Value]) -> EvalResult,
) -> EvalResult {
    let minibuffer_args = completing_read_minibuffer_args(obarray, args);
    let _ = crate::emacs_core::eval::set_runtime_binding(
        obarray,
        buffers,
        custom,
        specpdl,
        intern("minibuffer-completion-table"),
        args[1],
    );
    let _ = crate::emacs_core::eval::set_runtime_binding(
        obarray,
        buffers,
        custom,
        specpdl,
        intern("minibuffer-completion-predicate"),
        args.get(2).copied().unwrap_or(Value::NIL),
    );
    let require_match = args.get(3).copied().unwrap_or(Value::NIL);
    let _ = crate::emacs_core::eval::set_runtime_binding(
        obarray,
        buffers,
        custom,
        specpdl,
        intern("minibuffer-completion-confirm"),
        completion_confirm_from_require_match(require_match),
    );

    let result = read_from_minibuffer(&minibuffer_args);

    let _ = crate::emacs_core::eval::set_runtime_binding(
        obarray,
        buffers,
        custom,
        specpdl,
        intern("minibuffer-completion-table"),
        Value::NIL,
    );
    let _ = crate::emacs_core::eval::set_runtime_binding(
        obarray,
        buffers,
        custom,
        specpdl,
        intern("minibuffer-completion-predicate"),
        Value::NIL,
    );
    let _ = crate::emacs_core::eval::set_runtime_binding(
        obarray,
        buffers,
        custom,
        specpdl,
        intern("minibuffer-completion-confirm"),
        Value::NIL,
    );

    result
}

pub(crate) fn finish_read_from_minibuffer_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_read_from_minibuffer_in_runtime(shared, args)?;

    // Check inhibit-interaction — GNU Emacs signals an error when any
    // interactive read is attempted while this variable is non-nil.
    if shared
        .obarray
        .symbol_value("inhibit-interaction")
        .is_some_and(|v| v.is_truthy())
    {
        return Err(signal(
            "inhibited-interaction",
            vec![Value::string(
                "Attempt to interact with user while inhibit-interaction is non-nil",
            )],
        ));
    }

    let prompt = expect_string(&args[0])?;
    let initial_input = args.get(1).and_then(|v| match v.kind() {
        ValueKind::String => Some(super::builtins::lisp_string_to_runtime_string(*v)),
        _ => None,
    });
    let keymap_arg = args.get(2).copied().unwrap_or(Value::NIL);
    let read_arg = args.get(3).copied().unwrap_or(Value::NIL);
    let history_name = minibuffer_history_name(args.get(4));
    let default_val = args.get(5).copied().unwrap_or(Value::NIL);

    let saved_buffer_id = shared.buffers.current_buffer().map(|b| b.id);
    let recursive_depth = shared.recursive_command_loop_depth();

    let minibuf_depth = shared.minibuffers.depth() + 1;
    let minibuf_name = format!(" *Minibuf-{}*", minibuf_depth);
    let minibuf_id = shared
        .buffers
        .find_buffer_by_name(&minibuf_name)
        .unwrap_or_else(|| shared.buffers.create_buffer(&minibuf_name));

    let prompt_byte_len;
    {
        let buf = shared.buffers.get_mut(minibuf_id).unwrap();
        let text_len = buf.text.len();
        if text_len > 0 {
            buf.text.delete_range(0, text_len);
        }
        buf.text.insert_str(0, &prompt);
        let prompt_storage_len = prompt.len();
        prompt_byte_len = crate::emacs_core::string_escape::storage_byte_len(&prompt);
        if let Some(ref initial) = initial_input {
            buf.text.insert_str(prompt_storage_len, initial);
        }
        let total_len = buf.total_bytes();
        buf.widen();
        buf.goto_byte(total_len);
    }

    let active_window_state = activate_minibuffer_window_in_state(
        &mut shared.frames,
        &mut shared.buffers,
        &mut shared.minibuffer_selected_window,
        &mut shared.active_minibuffer_window,
        minibuf_id,
    );
    if active_window_state.is_none() {
        shared.buffers.switch_current(minibuf_id);
    }
    tracing::debug!(
        "read-from-minibuffer: prompt={:?} minibuf_id={:?} current_buffer={:?} active_window={:?} selected_window={:?}",
        prompt,
        minibuf_id,
        shared.buffers.current_buffer_id(),
        shared.active_minibuffer_window,
        shared
            .frames
            .selected_frame()
            .map(|frame| frame.selected_window)
    );

    let enable_recursive = shared
        .obarray
        .symbol_value("enable-recursive-minibuffers")
        .copied()
        .unwrap_or(Value::NIL)
        .is_truthy();
    shared.minibuffers.set_enable_recursive(enable_recursive);
    {
        let state = shared.minibuffers.read_from_minibuffer(
            minibuf_id,
            &prompt,
            initial_input.as_deref(),
            history_name.as_deref(),
        )?;
        state.command_loop_depth = recursive_depth;
    }

    let minibuf_keymap = if !keymap_arg.is_nil() {
        keymap_arg
    } else {
        shared
            .obarray
            .symbol_value("minibuffer-local-map")
            .copied()
            .unwrap_or(Value::NIL)
    };
    let _ = shared.buffers.set_current_local_map(minibuf_keymap);
    shared
        .obarray
        .set_symbol_value("minibuffer-prompt", Value::string(&prompt));
    shared
        .obarray
        .set_symbol_value("minibuffer-depth", Value::fixnum(minibuf_depth as i64));
    shared.run_hook_if_bound("minibuffer-setup-hook")?;

    let extra_roots = args.to_vec();
    let edit_result = shared.with_extra_gc_roots(vm_gc_roots, &extra_roots, |eval| {
        eval.minibuffer_command_loop_inner()
    });

    let result_string = if let Some(buf) = shared.buffers.get(minibuf_id) {
        let total_len = buf.total_bytes();
        if total_len > prompt_byte_len {
            let text = buf.buffer_substring_lisp_string(prompt_byte_len, total_len);
            crate::emacs_core::builtins::runtime_string_from_lisp_string(&text)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let _ = shared.buffers.switch_current(minibuf_id);
    let exit_hook_result = match shared.run_hook_if_bound("minibuffer-exit-hook") {
        Ok(value) => Ok(value),
        Err(Flow::Signal(_)) => Ok(Value::NIL),
        Err(flow) => Err(flow),
    };

    match &edit_result {
        Ok(_) => {
            let _ = shared.minibuffers.exit_minibuffer();
        }
        Err(Flow::Throw { tag, value }) if tag.is_symbol_named("exit") => {
            if value.is_truthy() {
                shared.minibuffers.abort_minibuffer();
            } else {
                let _ = shared.minibuffers.exit_minibuffer();
            }
        }
        Err(_) => {
            shared.minibuffers.abort_minibuffer();
        }
    }

    if let Some(saved) = active_window_state {
        restore_minibuffer_window_in_state(
            &mut shared.frames,
            &mut shared.minibuffer_selected_window,
            &mut shared.active_minibuffer_window,
            saved,
        );
    }
    if let Some(buf_id) = saved_buffer_id {
        shared.buffers.switch_current(buf_id);
    }
    tracing::debug!(
        "read-from-minibuffer: restored current_buffer={:?} active_window={:?} selected_window={:?}",
        shared.buffers.current_buffer_id(),
        shared.active_minibuffer_window,
        shared
            .frames
            .selected_frame()
            .map(|frame| frame.selected_window)
    );
    shared.obarray.set_symbol_value(
        "minibuffer-depth",
        Value::fixnum(shared.minibuffers.depth() as i64),
    );
    exit_hook_result?;

    match edit_result {
        Ok(_) | Err(Flow::Throw { .. }) => {
            if !read_arg.is_nil() && !result_string.is_empty() {
                let read_result =
                    read_from_string_impl(&shared.obarray, vec![Value::string(&result_string)])?;
                if read_result.is_cons() {
                    return Ok(read_result.cons_car());
                }
                return Ok(read_result);
            }

            if result_string.is_empty() && !default_val.is_nil() {
                return Ok(default_val);
            }

            Ok(Value::string(result_string))
        }
        Err(flow) => Err(flow),
    }
}

pub(crate) fn finish_completing_read_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_completing_read_in_runtime(shared, args)?;
    let minibuffer_args = completing_read_minibuffer_args(&shared.obarray, args);
    let _ = crate::emacs_core::eval::set_runtime_binding(
        &mut shared.obarray,
        &mut shared.buffers,
        &shared.custom,
        shared.specpdl.as_slice(),
        intern("minibuffer-completion-table"),
        args[1],
    );
    let _ = crate::emacs_core::eval::set_runtime_binding(
        &mut shared.obarray,
        &mut shared.buffers,
        &shared.custom,
        shared.specpdl.as_slice(),
        intern("minibuffer-completion-predicate"),
        args.get(2).copied().unwrap_or(Value::NIL),
    );
    let require_match = args.get(3).copied().unwrap_or(Value::NIL);
    let _ = crate::emacs_core::eval::set_runtime_binding(
        &mut shared.obarray,
        &mut shared.buffers,
        &shared.custom,
        shared.specpdl.as_slice(),
        intern("minibuffer-completion-confirm"),
        completion_confirm_from_require_match(require_match),
    );
    let result = finish_read_from_minibuffer_in_vm_runtime(shared, vm_gc_roots, &minibuffer_args);
    let _ = crate::emacs_core::eval::set_runtime_binding(
        &mut shared.obarray,
        &mut shared.buffers,
        &shared.custom,
        shared.specpdl.as_slice(),
        intern("minibuffer-completion-table"),
        Value::NIL,
    );
    let _ = crate::emacs_core::eval::set_runtime_binding(
        &mut shared.obarray,
        &mut shared.buffers,
        &shared.custom,
        shared.specpdl.as_slice(),
        intern("minibuffer-completion-predicate"),
        Value::NIL,
    );
    let _ = crate::emacs_core::eval::set_runtime_binding(
        &mut shared.obarray,
        &mut shared.buffers,
        &shared.custom,
        shared.specpdl.as_slice(),
        intern("minibuffer-completion-confirm"),
        Value::NIL,
    );
    result
}

/// Map the `REQUIRE-MATCH` argument of `completing-read` to the value
/// stored in `minibuffer-completion-confirm`.
///
/// GNU semantics:
///   nil        → nil   (any input accepted)
///   confirm    → confirm
///   confirm-after-completion → confirm-after-completion
///   t / other  → nil   (must-match keymap enforces exact match via
///                        `minibuffer-complete-and-exit`)
fn completion_confirm_from_require_match(require_match: Value) -> Value {
    if require_match.is_symbol_named("confirm")
        || require_match.is_symbol_named("confirm-after-completion")
    {
        require_match
    } else {
        Value::NIL
    }
}

pub(crate) fn completing_read_minibuffer_args(obarray: &Obarray, args: &[Value]) -> [Value; 7] {
    let prompt = args[0];
    let require_match = args.get(3).copied().unwrap_or(Value::NIL);
    let initial_input = args.get(4).copied().unwrap_or(Value::NIL);
    let hist = args.get(5).copied().unwrap_or(Value::NIL);
    let default_val = args.get(6).copied().unwrap_or(Value::NIL);
    let inherit = args.get(7).copied().unwrap_or(Value::NIL);

    let keymap = if !require_match.is_nil() {
        obarray
            .symbol_value("minibuffer-local-must-match-map")
            .copied()
            .unwrap_or(Value::NIL)
    } else {
        obarray
            .symbol_value("minibuffer-local-completion-map")
            .copied()
            .unwrap_or(Value::NIL)
    };

    [
        prompt,
        initial_input,
        keymap,
        Value::NIL,
        hist,
        default_val,
        inherit,
    ]
}

fn event_to_int(event: &Value) -> Option<i64> {
    match event.kind() {
        ValueKind::Fixnum(n) => Some(n),
        _ => None,
    }
}

fn event_to_char(event: &Value) -> Option<char> {
    match event.kind() {
        ValueKind::Fixnum(c) => char::from_u32(c as u32),
        _ => None,
    }
}

fn expect_optional_prompt_string(args: &[Value]) -> Result<(), Flow> {
    if args.is_empty() || args[0].is_nil() || args[0].is_string() {
        return Ok(());
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("stringp"), args[0]],
    ))
}

fn non_character_input_event_error() -> Flow {
    signal("error", vec![Value::string("Non-character input-event")])
}

pub(crate) trait KeyboardInputRuntime {
    fn pop_unread_command_event(&mut self) -> Option<Value>;
    fn peek_unread_command_event(&self) -> Option<Value>;
    fn replace_unread_command_event_with_singleton(&mut self, event: Value);
    fn record_input_event(&mut self, event: Value);
    fn record_nonmenu_input_event(&mut self, event: Value);
    fn set_read_command_keys(&mut self, keys: Vec<Value>);
    fn clear_read_command_keys(&mut self);
    fn read_command_keys(&self) -> &[Value];
    fn has_input_receiver(&self) -> bool;
    fn read_char_blocking(&mut self) -> Result<Value, Flow>;
    fn read_char_with_timeout(&mut self, timeout: Option<Duration>) -> Result<Option<Value>, Flow>;
    fn read_key_sequence_blocking(
        &mut self,
        options: crate::keyboard::ReadKeySequenceOptions,
    ) -> Result<(Vec<Value>, Value), Flow>;
}

impl KeyboardInputRuntime for super::eval::Context {
    fn pop_unread_command_event(&mut self) -> Option<Value> {
        super::eval::Context::pop_unread_command_event(self)
    }

    fn peek_unread_command_event(&self) -> Option<Value> {
        super::eval::Context::peek_unread_command_event(self)
    }

    fn replace_unread_command_event_with_singleton(&mut self, event: Value) {
        super::eval::Context::replace_unread_command_event_with_singleton(self, event);
    }

    fn record_input_event(&mut self, event: Value) {
        super::eval::Context::record_input_event(self, event);
    }

    fn record_nonmenu_input_event(&mut self, event: Value) {
        super::eval::Context::record_nonmenu_input_event(self, event);
    }

    fn set_read_command_keys(&mut self, keys: Vec<Value>) {
        super::eval::Context::set_read_command_keys(self, keys);
    }

    fn clear_read_command_keys(&mut self) {
        super::eval::Context::clear_read_command_keys(self);
    }

    fn read_command_keys(&self) -> &[Value] {
        super::eval::Context::read_command_keys(self)
    }

    fn has_input_receiver(&self) -> bool {
        super::eval::Context::has_input_receiver(self)
    }

    fn read_char_blocking(&mut self) -> Result<Value, Flow> {
        super::eval::Context::read_char(self)
    }

    fn read_char_with_timeout(&mut self, timeout: Option<Duration>) -> Result<Option<Value>, Flow> {
        super::eval::Context::read_char_with_timeout(self, timeout)
    }

    fn read_key_sequence_blocking(
        &mut self,
        options: crate::keyboard::ReadKeySequenceOptions,
    ) -> Result<(Vec<Value>, Value), Flow> {
        super::eval::Context::read_key_sequence_with_options(self, options)
    }
}

pub(crate) fn read_key_sequence_options_from_args(
    args: &[Value],
) -> crate::keyboard::ReadKeySequenceOptions {
    crate::keyboard::ReadKeySequenceOptions::new(
        args.first().copied().unwrap_or(Value::NIL),
        args.get(2).is_some_and(|v| v.is_truthy()),
        args.get(3).is_some_and(|v| v.is_truthy()),
    )
}

// ---------------------------------------------------------------------------
// 10. input-pending-p
// ---------------------------------------------------------------------------

/// `(input-pending-p &optional CHECK-TIMERS)`
///
/// Return non-nil when unread input, staged host input, or `quit-flag` is pending.
/// `CHECK-TIMERS` is accepted and fires due timers before checking.
fn input_pending_now(ctx: &crate::emacs_core::eval::Context, filter_events: bool) -> bool {
    if peek_unread_command_event_in_state(&ctx.obarray, &[]).is_some() {
        return true;
    }

    if ctx.command_loop.keyboard.has_pending_kboard_input() {
        return true;
    }

    if !ctx.quit_flag_value().is_nil() {
        return true;
    }

    ctx.command_loop
        .keyboard
        .pending_input_events
        .iter()
        .any(|event| ctx.input_event_counts_as_pending(event, filter_events))
}

pub(crate) fn builtin_input_pending_p(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("input-pending-p", &args, 1)?;
    ctx.sync_keyboard_terminal_owner();
    ctx.sync_pending_resize_events();
    let _ = ctx.stage_next_host_input_event_if_available()?;

    let filter_events = ctx.input_pending_p_filters_events();
    if input_pending_now(ctx, filter_events) {
        return Ok(Value::T);
    }

    if args.first().is_some_and(|v| v.is_truthy()) {
        // GNU `input-pending-p' can run due timers here, but it does not
        // force a redisplay the way `detect_input_pending_run_timers' does.
        let _ = ctx.service_pending_timers_with_wait_policy(false);
        ctx.sync_pending_resize_events();
        let _ = ctx.stage_next_host_input_event_if_available()?;
    }

    Ok(Value::bool_val(input_pending_now(ctx, filter_events)))
}

// ---------------------------------------------------------------------------
// 11. discard-input
// ---------------------------------------------------------------------------

/// `(discard-input)`
///
/// Discard pending unread command events for the current scope.
pub(crate) fn builtin_discard_input(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("discard-input", &args, 0)?;
    super::eval::set_runtime_binding(
        &mut ctx.obarray,
        &mut ctx.buffers,
        &ctx.custom,
        ctx.specpdl.as_slice(),
        intern("unread-command-events"),
        Value::NIL,
    );
    Ok(Value::NIL)
}

// ---------------------------------------------------------------------------
// 11b. insert-special-event
// ---------------------------------------------------------------------------

/// `(insert-special-event EVENT)` -> nil
///
/// Prepend EVENT to the front of `unread-command-events`, so that
/// the next key-reading operation consumes it immediately without
/// waiting for a user keystroke.
///
/// Mirrors GNU `Finsert_special_event` at
/// `src/keyboard.c:12060`:
///
///   DEFUN ("insert-special-event", Finsert_special_event, ...)
///     (Lisp_Object event)
///   {
///     kbd_buffer_store_event (event);
///     return Qnil;
///   }
///
/// GNU pushes into the kernel kbd_buffer (which is a ring of
/// `struct input_event` records) so the event is delivered via the
/// same path as hardware input. neomacs does not keep a separate
/// kbd_buffer ring — every lisp-visible event funnels through
/// `unread-command-events`, so we route insertions there. This is
/// the same choice made elsewhere in the reader (`read-event`,
/// `push_unread_command_event`, etc.), and preserves the observable
/// "run this event next" semantic that callers rely on.
///
/// Keyboard audit Finding 16 in
/// `drafts/keyboard-command-loop-audit.md`.
pub(crate) fn builtin_insert_special_event(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("insert-special-event", &args, 1)?;
    let event = args[0];
    ctx.push_unread_command_event(event);
    Ok(Value::NIL)
}

// ---------------------------------------------------------------------------
// 12. current-input-mode / set-input-mode
// ---------------------------------------------------------------------------

/// `(current-input-mode)` -> `(INTERRUPT FLOW META QUIT)`
pub(crate) fn builtin_current_input_mode(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-input-mode", &args, 0)?;
    let (interrupt, flow, meta, quit) = ctx.current_input_mode_tuple();
    Ok(Value::list(vec![
        Value::bool_val(interrupt),
        Value::bool_val(flow),
        Value::bool_val(meta),
        Value::fixnum(quit),
    ]))
}

/// `(set-input-mode INTERRUPT FLOW META QUIT)`
///
/// Batch-compatible behavior currently tracks INTERRUPT plus Lisp-visible
/// QUIT while leaving FLOW/META fixed.
pub(crate) fn builtin_set_input_mode(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-input-mode", &args, 3)?;
    expect_max_args("set-input-mode", &args, 4)?;
    eval.set_input_mode_interrupt(args[0].is_truthy());
    if let Some(quit) = args.get(3).copied()
        && !quit.is_nil()
    {
        set_quit_char_in_context(eval, quit)?;
    }
    Ok(Value::NIL)
}

// ---------------------------------------------------------------------------
// 13. input mode helper setters
// ---------------------------------------------------------------------------

/// `(set-input-interrupt-mode INTERRUPT)`
pub(crate) fn builtin_set_input_interrupt_mode(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-input-interrupt-mode", &args, 1)?;
    eval.set_input_mode_interrupt(args[0].is_truthy());
    Ok(Value::NIL)
}

fn peek_unread_command_event_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
) -> Option<Value> {
    let name_id = intern("unread-command-events");
    let unread = dynamic
        .iter()
        .rev()
        .find_map(|frame| frame.get(&name_id).copied())
        .or_else(|| obarray.symbol_value("unread-command-events").copied());
    match unread {
        Some(v) if v.is_cons() => Some(v.cons_car()),
        _ => None,
    }
}

pub(crate) fn builtin_read_char_in_runtime(
    runtime: &mut impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<Option<Value>, Flow> {
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("read-char"), Value::fixnum(args.len() as i64)],
        ));
    }
    expect_optional_prompt_string(args)?;
    let seconds_is_nil_or_omitted = args.get(2).is_none_or(|v| v.is_nil());

    if let Some(event) = runtime.peek_unread_command_event() {
        if let Some(n) = event_to_int(&event) {
            let event = runtime
                .pop_unread_command_event()
                .expect("peeked unread event should still be present");
            if runtime.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                runtime.set_read_command_keys(vec![event]);
            }
            return Ok(Some(Value::fixnum(n)));
        }
        runtime.replace_unread_command_event_with_singleton(event);
        runtime.record_input_event(event);
        return Err(non_character_input_event_error());
    }

    if runtime.has_input_receiver() {
        Ok(None)
    } else {
        Ok(Some(Value::NIL))
    }
}

pub(crate) fn builtin_read_key_sequence_in_runtime(
    runtime: &mut impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<Option<Value>, Flow> {
    expect_min_args("read-key-sequence", args, 1)?;
    expect_max_args("read-key-sequence", args, 6)?;
    expect_optional_prompt_string(args)?;

    if let Some(event) = runtime.pop_unread_command_event() {
        runtime.record_nonmenu_input_event(event);
        runtime.set_read_command_keys(vec![event]);
        if let Some(c) = event_to_char(&event) {
            return Ok(Some(Value::string(c.to_string())));
        }
        return Ok(Some(Value::vector(vec![event])));
    }

    if runtime.has_input_receiver() {
        Ok(None)
    } else {
        runtime.clear_read_command_keys();
        Ok(Some(Value::string("")))
    }
}

pub(crate) fn builtin_read_key_sequence_vector_in_runtime(
    runtime: &mut impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<Option<Value>, Flow> {
    expect_min_args("read-key-sequence-vector", args, 1)?;
    expect_max_args("read-key-sequence-vector", args, 6)?;
    expect_optional_prompt_string(args)?;

    if let Some(event) = runtime.pop_unread_command_event() {
        runtime.record_nonmenu_input_event(event);
        runtime.set_read_command_keys(vec![event]);
        if let Some(n) = event_to_int(&event) {
            return Ok(Some(Value::vector(vec![Value::fixnum(n)])));
        }
        return Ok(Some(Value::vector(vec![event])));
    }

    runtime.clear_read_command_keys();
    Ok(Some(Value::vector(vec![])))
}

/// `(set-input-meta-mode META)`
///
/// Batch-compatible behavior: accepts GNU-compatible optional TERMINAL and returns nil.
pub(crate) fn builtin_set_input_meta_mode(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-input-meta-mode", &args, 1)?;
    expect_max_args("set-input-meta-mode", &args, 2)?;
    Ok(Value::NIL)
}

/// `(set-output-flow-control FLOW)`
///
/// Batch-compatible behavior: accepts one argument and returns nil.
pub(crate) fn builtin_set_output_flow_control(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-output-flow-control", &args, 1)?;
    expect_max_args("set-output-flow-control", &args, 2)?;
    Ok(Value::NIL)
}

/// `(set-quit-char CHAR)`
///
fn set_quit_char_in_context(eval: &mut super::eval::Context, quit: Value) -> EvalResult {
    let Some(quit) = quit.as_fixnum() else {
        return Err(signal(
            "error",
            vec![Value::string("QUIT must be an ASCII character")],
        ));
    };
    if !(0..=0o400).contains(&quit) {
        return Err(signal(
            "error",
            vec![Value::string("QUIT must be an ASCII character")],
        ));
    }

    eval.set_quit_char(quit);
    Ok(Value::NIL)
}

/// GNU-compatible quit-char setter for the current evaluator.
pub(crate) fn builtin_set_quit_char(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-quit-char", &args, 1)?;
    set_quit_char_in_context(eval, args[0])
}

// ---------------------------------------------------------------------------
// 14. waiting-for-user-input-p
// ---------------------------------------------------------------------------

/// `(waiting-for-user-input-p)`
///
/// Batch-mode compatibility: always returns nil.
pub(crate) fn builtin_waiting_for_user_input_p(args: Vec<Value>) -> EvalResult {
    expect_args("waiting-for-user-input-p", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_waiting_for_user_input_p_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("waiting-for-user-input-p", &args, 0)?;
    Ok(Value::bool_val(eval.waiting_for_user_input()))
}

// ---------------------------------------------------------------------------
// 15. y-or-n-p
// ---------------------------------------------------------------------------

/// `(y-or-n-p PROMPT)`
///
/// Ask user a yes-or-no question. Returns t for 'y', nil for 'n'.
/// In interactive mode, reads a single character.
/// In batch mode, signals end-of-file.
pub(crate) fn builtin_y_or_n_p(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("y-or-n-p", &args, 1)?;
    match args[0].kind() {
        ValueKind::String | ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Nil => {}
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("sequencep"), args[0]],
            ));
        }
    }

    // Interactive mode: read single character
    if eval.input_rx.is_some() {
        // Display prompt in echo area (message)
        if args[0].is_string() {
            let prompt_str = super::builtins::lisp_string_to_runtime_string(args[0]);
            let msg = format!("{} (y or n) ", prompt_str);
            eval.assign("minibuffer-message", Value::string(&msg));
        }
        loop {
            let event = eval.read_char()?;
            if let Some(n) = event_to_int(&event) {
                match n as u32 {
                    y if y == 'y' as u32 || y == 'Y' as u32 => return Ok(Value::T),
                    n if n == 'n' as u32 || n == 'N' as u32 => return Ok(Value::NIL),
                    _ => continue, // Invalid response, try again
                }
            }
            // Non-character event, ignore
        }
    }

    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

// ---------------------------------------------------------------------------
// 16. yes-or-no-p
// ---------------------------------------------------------------------------

/// `(yes-or-no-p PROMPT)`
///
/// Ask user a yes-or-no question requiring "yes" or "no" typed in full.
/// In interactive mode, uses read-from-minibuffer.
/// In batch mode, signals end-of-file.
pub(crate) fn builtin_yes_or_no_p(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    builtin_yes_or_no_p_in_runtime(eval, &args)?;
    finish_yes_or_no_p_in_eval(eval, &args)
}

pub(crate) fn finish_yes_or_no_p_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_yes_or_no_p_with_minibuffer(args, |minibuffer_args| {
        finish_read_from_minibuffer_in_eval(eval, minibuffer_args)
    })
}

pub(crate) fn finish_yes_or_no_p_with_minibuffer(
    args: &[Value],
    mut read_from_minibuffer: impl FnMut(&[Value]) -> EvalResult,
) -> EvalResult {
    let prompt_str = if args[0].is_string() {
        super::builtins::lisp_string_to_runtime_string(args[0])
    } else {
        String::new()
    };
    loop {
        let full_prompt = format!("{} (yes or no) ", prompt_str);
        let result = read_from_minibuffer(&[Value::string(&full_prompt)])?;
        if result.is_string() {
            let answer = super::builtins::lisp_string_to_runtime_string(result);
            match answer.trim() {
                "yes" => return Ok(Value::T),
                "no" => return Ok(Value::NIL),
                _ => continue,
            }
        }
    }
}

pub(crate) fn finish_yes_or_no_p_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_yes_or_no_p_in_runtime(shared, args)?;
    finish_yes_or_no_p_with_minibuffer(args, |minibuffer_args| {
        finish_read_from_minibuffer_in_vm_runtime(shared, vm_gc_roots, minibuffer_args)
    })
}

pub(crate) fn builtin_yes_or_no_p_in_runtime(
    runtime: &impl KeyboardInputRuntime,
    args: &[Value],
) -> Result<(), Flow> {
    expect_args("yes-or-no-p", args, 1)?;
    if !args[0].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        ));
    }

    if runtime.has_input_receiver() {
        Ok(())
    } else {
        Err(stdin_end_of_file_error())
    }
}

// ---------------------------------------------------------------------------
// 17. read-char
// ---------------------------------------------------------------------------

/// `(read-char &optional PROMPT INHERIT-INPUT-METHOD SECONDS)`
///
/// Read a character from the command input (keyboard or macro).
/// In batch mode, checks `unread-command-events` and returns nil if empty.
/// In interactive mode, blocks on the input channel via `read_char()`.
pub(crate) fn builtin_read_char(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if let Some(value) = builtin_read_char_in_runtime(eval, &args)? {
        return Ok(value);
    }

    finish_read_char_in_eval(eval, &args)
}

pub(crate) fn finish_read_char_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_read_char_interactive_in_runtime(eval, args)
}

pub(crate) fn finish_read_char_interactive_in_runtime(
    runtime: &mut impl KeyboardInputRuntime,
    args: &[Value],
) -> EvalResult {
    if runtime.has_input_receiver() {
        let timeout = parse_optional_read_seconds_arg(args.get(2))?;
        let Some(event) = runtime.read_char_with_timeout(timeout)? else {
            return Ok(Value::NIL);
        };
        let seconds_is_nil_or_omitted = args.get(2).is_none_or(|v| v.is_nil());
        if let Some(n) = event_to_int(&event) {
            if runtime.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                runtime.set_read_command_keys(vec![event]);
            }
            return Ok(Value::fixnum(n));
        }
        runtime.replace_unread_command_event_with_singleton(event);
        runtime.record_input_event(event);
        return Err(non_character_input_event_error());
    }

    Ok(Value::NIL)
}

/// `(read-key &optional PROMPT)`
///
/// Read a key from the command input.
/// In batch mode, returns next `unread-command-events` event, else nil.
/// In interactive mode, blocks on the input channel via `read_char()`.
pub(crate) fn builtin_read_key(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("read-key"), Value::fixnum(args.len() as i64)],
        ));
    }
    expect_optional_prompt_string(&args)?;

    // 1. Check unread-command-events first
    if let Some(event) = eval.pop_unread_command_event() {
        eval.record_nonmenu_input_event(event);
        eval.set_read_command_keys(vec![event]);
        if let Some(n) = event_to_int(&event) {
            return Ok(Value::fixnum(n));
        }
        return Ok(event);
    }

    // 2. Interactive mode: block on input channel
    if eval.input_rx.is_some() {
        let event = eval.read_char()?;
        eval.record_nonmenu_input_event(event);
        eval.set_read_command_keys(vec![event]);
        if let Some(n) = event_to_int(&event) {
            return Ok(Value::fixnum(n));
        }
        return Ok(event);
    }

    // 3. Batch mode: no input
    eval.clear_read_command_keys();
    Ok(Value::NIL)
}

// ---------------------------------------------------------------------------
// 18. read-key-sequence
// ---------------------------------------------------------------------------

/// `(read-key-sequence PROMPT &optional ...)`
///
/// Read a sequence of keystrokes that forms a complete key binding.
/// In batch mode, consumes one queued event. In interactive mode, uses the
/// evaluator's `read_key_sequence()` to accumulate keys through prefix keymaps.
pub(crate) fn builtin_read_key_sequence(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(value) = builtin_read_key_sequence_in_runtime(eval, &args)? {
        return Ok(value);
    }

    finish_read_key_sequence_in_eval(eval, &args)
}

pub(crate) fn finish_read_key_sequence_in_eval(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> EvalResult {
    finish_read_key_sequence_interactive_in_runtime(eval, read_key_sequence_options_from_args(args))
}

pub(crate) fn finish_read_key_sequence_interactive_in_runtime(
    runtime: &mut impl KeyboardInputRuntime,
    options: crate::keyboard::ReadKeySequenceOptions,
) -> EvalResult {
    if runtime.has_input_receiver() {
        let (keys, _binding) = runtime.read_key_sequence_blocking(options)?;
        let mut chars_only = true;
        let mut s = String::new();
        for k in &keys {
            if let Some(c) = event_to_char(k) {
                s.push(c);
            } else {
                chars_only = false;
                break;
            }
        }
        if chars_only && !keys.is_empty() {
            return Ok(Value::string(s));
        }
        return Ok(Value::vector(keys));
    }

    runtime.clear_read_command_keys();
    Ok(Value::string(""))
}

/// `(read-key-sequence-vector PROMPT)`
///
/// Batch mode: returns next `unread-command-events` event as a single-element
/// vector when present, otherwise an empty vector.
pub(crate) fn builtin_read_key_sequence_vector(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(value) = builtin_read_key_sequence_vector_in_runtime(eval, &args)? {
        return Ok(value);
    }
    finish_read_key_sequence_vector_interactive_in_runtime(
        eval,
        read_key_sequence_options_from_args(&args),
    )
}

pub(crate) fn finish_read_key_sequence_vector_interactive_in_runtime(
    runtime: &mut impl KeyboardInputRuntime,
    options: crate::keyboard::ReadKeySequenceOptions,
) -> EvalResult {
    if runtime.has_input_receiver() {
        let (keys, _binding) = runtime.read_key_sequence_blocking(options)?;
        return Ok(Value::vector(keys));
    }

    runtime.clear_read_command_keys();
    Ok(Value::vector(vec![]))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "reader_test.rs"]
mod tests;
