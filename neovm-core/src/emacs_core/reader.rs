//! Reader/printer builtins: read-from-string, read, prin1-to-string (enhanced),
//! format-spec, and various interactive-input stubs.

use super::custom::CustomManager;
use super::error::{EvalResult, Flow, signal};
use super::expr::Expr;
use super::intern::{SymId, intern, resolve_sym};
use super::symbol::Obarray;
use super::value::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn expect_number(value: &Value) -> Result<(), Flow> {
    match value {
        Value::Int(_) | Value::Float(_, _) | Value::Char(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *other],
        )),
    }
}

fn expect_initial_input_stringish(value: &Value) -> Result<(), Flow> {
    match value {
        Value::Nil | Value::Str(_) => Ok(()),
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            if !matches!(pair.car, Value::Str(_)) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), pair.car],
                ));
            }
            Ok(())
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn expect_completing_read_initial_input(value: &Value) -> Result<(), Flow> {
    match value {
        Value::Nil | Value::Str(_) => Ok(()),
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            if !matches!(pair.car, Value::Str(_)) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), pair.car],
                ));
            }
            if !matches!(pair.cdr, Value::Int(_) | Value::Char(_)) {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), pair.cdr],
                ));
            }
            Ok(())
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
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
    buffers.set_current(minibuf_id);
    *minibuffer_selected_window = Some(previous_selected_window);
    *active_minibuffer_window = Some(minibuffer_window_id);
    Some(saved)
}

fn activate_minibuffer_window(
    eval: &mut super::eval::Evaluator,
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

fn restore_minibuffer_window(
    eval: &mut super::eval::Evaluator,
    saved: ActiveMinibufferWindowState,
) {
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
        vec![Value::string(message), Value::Int(line), Value::Int(column)],
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_read_from_string_in_state(&eval.obarray, args)
}

pub(crate) fn builtin_read_from_string_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("read-from-string", &args, 1)?;
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("read-from-string"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let full_string = expect_string(&args[0])?;

    let start_arg = args.get(1).cloned().unwrap_or(Value::Nil);
    let end_arg = args.get(2).cloned().unwrap_or(Value::Nil);
    let to_index = |value: &Value| -> Result<usize, Flow> {
        match value {
            Value::Nil => Ok(0),
            Value::Int(n) => {
                let idx = if *n < 0 {
                    (full_string.len() as i64) + *n
                } else {
                    *n
                };
                if idx < 0 || idx > full_string.len() as i64 {
                    return Err(signal(
                        "args-out-of-range",
                        vec![args[0], start_arg, end_arg],
                    ));
                }
                Ok(idx as usize)
            }
            other => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), *other],
            )),
        }
    };
    let start = if args.len() > 1 {
        to_index(&start_arg)?
    } else {
        0
    };
    let end = if args.len() > 2 {
        to_index(&end_arg)?
    } else {
        full_string.len()
    };

    if start > end {
        return Err(signal(
            "args-out-of-range",
            vec![args[0], start_arg, end_arg],
        ));
    }

    let substring = &full_string[start..end];
    if starts_with_hash_skip_dispatch(substring) {
        return Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        ));
    }
    let (expr, end_pos) = super::parser::parse_form(substring)
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

    let value = if let Some(bytecode) = first_form_byte_code_literal_value(obarray, &expr) {
        bytecode
    } else if let Some(hash_table) = first_form_hash_table_literal_value(obarray, &expr) {
        hash_table
    } else {
        super::eval::Evaluator::quote_to_runtime_value_in_state(obarray, &expr)
    };
    let absolute_end = start + end_pos;

    Ok(Value::cons(value, Value::Int(absolute_end as i64)))
}

fn first_form_byte_code_literal_value(
    obarray: &crate::emacs_core::symbol::Obarray,
    expr: &Expr,
) -> Option<Value> {
    let Expr::List(items) = expr else {
        return None;
    };
    if items.len() != 2 {
        return None;
    }
    let Expr::Symbol(id) = &items[0] else {
        return None;
    };
    if resolve_sym(*id) != "byte-code-literal" {
        return None;
    }
    let Expr::Vector(values) = &items[1] else {
        return None;
    };
    let values = values
        .iter()
        .map(|value| super::eval::Evaluator::quote_to_runtime_value_in_state(obarray, value))
        .collect();
    Some(Value::vector(values))
}

fn first_form_hash_table_literal_value(
    obarray: &crate::emacs_core::symbol::Obarray,
    expr: &Expr,
) -> Option<Value> {
    let Expr::List(items) = expr else {
        return None;
    };
    if items.len() != 2 {
        return None;
    }
    let Expr::Symbol(id) = &items[0] else {
        return None;
    };
    if resolve_sym(*id) != "make-hash-table-from-literal" {
        return None;
    }
    let Expr::List(quoted) = &items[1] else {
        return None;
    };
    if quoted.len() != 2 {
        return None;
    }
    if !matches!(&quoted[0], Expr::Symbol(id) if resolve_sym(*id) == "quote") {
        return None;
    }
    let Expr::List(spec) = &quoted[1] else {
        return None;
    };
    if !matches!(spec.first(), Some(Expr::Symbol(id)) if resolve_sym(*id) == "hash-table") {
        return None;
    }

    let mut test = HashTableTest::Eql;
    let mut test_name: Option<SymId> = None;
    let mut size = 0_i64;
    let mut weakness: Option<HashTableWeakness> = None;
    let mut rehash_size = 1.5_f64;
    let mut rehash_threshold = 0.8125_f64;
    let mut data_expr: Option<&Expr> = None;

    let mut i = 1_usize;
    while i + 1 < spec.len() {
        let Expr::Symbol(key_id) = &spec[i] else {
            i += 1;
            continue;
        };
        let value = super::eval::Evaluator::quote_to_runtime_value_in_state(obarray, &spec[i + 1]);
        match resolve_sym(*key_id) {
            "size" => {
                size = value.as_int()?;
            }
            "test" => {
                let name = value.as_symbol_name()?;
                test = match name {
                    "eq" => HashTableTest::Eq,
                    "eql" => HashTableTest::Eql,
                    "equal" => HashTableTest::Equal,
                    _ => return None,
                };
                test_name = Some(intern(name));
            }
            "weakness" => {
                weakness = match value.as_symbol_name() {
                    Some("key") => Some(HashTableWeakness::Key),
                    Some("value") => Some(HashTableWeakness::Value),
                    Some("key-or-value") => Some(HashTableWeakness::KeyOrValue),
                    Some("key-and-value") => Some(HashTableWeakness::KeyAndValue),
                    Some("nil") | None => None,
                    _ => return None,
                };
            }
            "rehash-size" => {
                rehash_size = value.as_float().unwrap_or(value.as_int()? as f64);
            }
            "rehash-threshold" => {
                rehash_threshold = value.as_float().unwrap_or(value.as_int()? as f64);
            }
            "data" => {
                data_expr = Some(&spec[i + 1]);
            }
            _ => {}
        }
        i += 2;
    }

    let table_value =
        Value::hash_table_with_options(test, size, weakness, rehash_size, rehash_threshold);
    if let Value::HashTable(table_ref) = &table_value {
        with_heap_mut(|h| {
            let table = h.get_hash_table_mut(*table_ref);
            table.test_name = test_name;
            if let Some(Expr::List(data_items)) = data_expr {
                let mut idx = 0_usize;
                while idx + 1 < data_items.len() {
                    let key_value = super::eval::Evaluator::quote_to_runtime_value_in_state(
                        obarray,
                        &data_items[idx],
                    );
                    let val_value = super::eval::Evaluator::quote_to_runtime_value_in_state(
                        obarray,
                        &data_items[idx + 1],
                    );
                    let key = key_value.to_hash_key(&table.test);
                    let inserting_new_key = !table.data.contains_key(&key);
                    table.data.insert(key.clone(), val_value);
                    if inserting_new_key {
                        table.key_snapshots.insert(key.clone(), key_value);
                        table.insertion_order.push(key);
                    }
                    idx += 2;
                }
            }
        });
    }
    Some(table_value)
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
pub(crate) fn builtin_read(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    builtin_read_in_state(&eval.obarray, &mut eval.buffers, args)
}

pub(crate) fn builtin_read_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("read", &args, 1)?;

    if args.is_empty() || args[0].is_nil() {
        // In batch/non-interactive runs, stdin-backed read signals EOF.
        return Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        ));
    }

    match &args[0] {
        Value::Str(_) => {
            // Read from string
            let result = builtin_read_from_string_in_state(obarray, args)?;
            // Return just the car (the parsed object)
            match &result {
                Value::Cons(cell) => {
                    let pair = read_cons(*cell);
                    Ok(pair.car)
                }
                _ => Ok(result),
            }
        }
        Value::Buffer(id) => {
            // Read from buffer at point
            let buf_id = *id;
            let (text, pt) = {
                let buf = buffers
                    .get(buf_id)
                    .ok_or_else(|| signal("error", vec![Value::string("Buffer does not exist")]))?;
                (buf.buffer_string(), buf.pt)
            };
            // Buffer point is a 0-based byte offset.
            let start = pt;
            if start >= text.len() {
                return Err(signal(
                    "end-of-file",
                    vec![Value::string("End of file during parsing")],
                ));
            }
            let substring = &text[start..];
            let (expr, end_offset) = super::parser::parse_form(substring)
                .map_err(|e| {
                    if e.message.contains("unterminated") || e.message.contains("end of input") {
                        signal(
                            "end-of-file",
                            vec![Value::string("End of file during parsing")],
                        )
                    } else {
                        signal_invalid_read_syntax_in_buffer(&text, start + e.position, e.message)
                    }
                })?
                .ok_or_else(|| {
                    signal(
                        "end-of-file",
                        vec![Value::string("End of file during parsing")],
                    )
                })?;
            let value = if let Some(bytecode) = first_form_byte_code_literal_value(obarray, &expr) {
                bytecode
            } else if let Some(hash_table) = first_form_hash_table_literal_value(obarray, &expr) {
                hash_table
            } else {
                super::eval::Evaluator::quote_to_runtime_value_in_state(obarray, &expr)
            };
            // Advance point past the read form
            let new_pt = pt + end_offset;
            let _ = buffers.goto_buffer_byte(buf_id, new_pt);
            Ok(value)
        }
        Value::Symbol(id) => Err(signal(
            "void-function",
            vec![Value::symbol(resolve_sym(*id))],
        )),
        Value::True => Err(signal(
            "end-of-file",
            vec![Value::string("End of file during parsing")],
        )),
        Value::Keyword(id) => Err(signal(
            "void-function",
            vec![Value::symbol(resolve_sym(*id))],
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_read_from_minibuffer_in_runtime(eval, &args)?;
    finish_read_from_minibuffer_in_eval(eval, &args)
}

pub(crate) fn finish_read_from_minibuffer_in_eval(
    eval: &mut super::eval::Evaluator,
    args: &[Value],
) -> EvalResult {
    let eval_ptr = std::ptr::NonNull::from(&mut *eval);
    finish_read_from_minibuffer_in_state_with_recursive_edit(
        &mut eval.obarray,
        &mut eval.buffers,
        &mut eval.frames,
        &mut eval.minibuffers,
        &mut eval.current_local_map,
        &mut eval.minibuffer_selected_window,
        &mut eval.active_minibuffer_window,
        eval.command_loop.recursive_depth,
        args,
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
    current_local_map: &mut Value,
    minibuffer_selected_window: &mut Option<crate::window::WindowId>,
    active_minibuffer_window: &mut Option<crate::window::WindowId>,
    recursive_depth: usize,
    args: &[Value],
    mut run_recursive_edit: impl FnMut() -> EvalResult,
) -> EvalResult {
    let prompt = expect_string(&args[0])?;
    // Extract optional arguments
    let initial_input = args.get(1).and_then(|v| match v {
        Value::Str(id) => Some(super::value::with_heap(|h| h.get_string(*id).to_owned())),
        _ => None,
    });
    let keymap_arg = args.get(2).copied().unwrap_or(Value::Nil);
    let read_arg = args.get(3).copied().unwrap_or(Value::Nil);
    let history_name = minibuffer_history_name(args.get(4));
    let default_val = args.get(5).copied().unwrap_or(Value::Nil);

    // Save state
    let saved_local_map = *current_local_map;
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
        prompt_byte_len = prompt.len();
        if let Some(ref initial) = initial_input {
            buf.text.insert_str(prompt_byte_len, initial);
        }
        let total_len = buf.text.len();
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
        buffers.set_current(minibuf_id);
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
        .unwrap_or(Value::Nil)
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
            .unwrap_or(Value::Nil)
    };
    *current_local_map = minibuf_keymap;

    // Set minibuffer-related variables
    obarray.set_symbol_value("minibuffer-prompt", Value::string(prompt));
    obarray.set_symbol_value("minibuffer-depth", Value::Int(minibuf_depth as i64));

    // Enter recursive edit — the command loop runs until exit-minibuffer throws 'exit.
    let edit_result = run_recursive_edit();

    // Read the minibuffer contents (everything after the prompt)
    let result_string = if let Some(buf) = buffers.get(minibuf_id) {
        let total_len = buf.text.len();
        if total_len > prompt_byte_len {
            buf.buffer_substring(prompt_byte_len, total_len)
        } else {
            String::new()
        }
    } else {
        String::new()
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
    *current_local_map = saved_local_map;
    if let Some(saved) = active_window_state {
        restore_minibuffer_window_in_state(
            frames,
            minibuffer_selected_window,
            active_minibuffer_window,
            saved,
        );
    }
    if let Some(buf_id) = saved_buffer_id {
        buffers.set_current(buf_id);
    }
    tracing::debug!(
        "read-from-minibuffer: restored current_buffer={:?} active_window={:?} selected_window={:?}",
        buffers.current_buffer_id(),
        *active_minibuffer_window,
        frames.selected_frame().map(|frame| frame.selected_window)
    );
    obarray.set_symbol_value("minibuffer-depth", Value::Int(minibuffers.depth() as i64));

    // Handle the recursive edit result
    match edit_result {
        Ok(_) | Err(Flow::Throw { .. }) => {
            // Normal exit (throw 'exit from exit-minibuffer)
            // If READ arg is non-nil, evaluate the result as a Lisp expression
            if !read_arg.is_nil() && !result_string.is_empty() {
                // READ is non-nil: parse the result string as a Lisp expression
                // (like calling (read STRING)) and return the parsed object.
                let read_result = builtin_read_from_string_in_state(
                    obarray,
                    vec![Value::string(&result_string)],
                )?;
                // read-from-string returns (OBJECT . END-POS), extract OBJECT
                if let Value::Cons(id) = read_result {
                    let snap = super::value::read_cons(id);
                    return Ok(snap.car);
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
    match hist_arg.copied().unwrap_or(Value::Nil) {
        Value::Symbol(id) => Some(resolve_sym(id).to_string()),
        Value::Cons(id) => read_cons(id).car.as_symbol_name().map(str::to_string),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 6. read-string
// ---------------------------------------------------------------------------

/// `(read-string PROMPT &optional INITIAL HISTORY DEFAULT INHERIT-INPUT-METHOD)`
///
/// Read a string from the minibuffer.  Delegates to `read-from-minibuffer`.
pub(crate) fn builtin_read_string(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_read_string_in_runtime(eval, &args)?;
    finish_read_string_in_eval(eval, &args)
}

pub(crate) fn finish_read_string_in_eval(
    eval: &mut super::eval::Evaluator,
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

    let initial = args.get(1).copied().unwrap_or(Value::Nil);
    let history = args.get(2).copied().unwrap_or(Value::Nil);
    let default = args.get(3).copied().unwrap_or(Value::Nil);
    let inherit = args.get(4).copied().unwrap_or(Value::Nil);
    let minibuffer_args = [
        prompt,
        initial,
        Value::Nil,
        Value::Nil,
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
    let initial = args.get(1).copied().unwrap_or(Value::Nil);
    let history = args.get(2).copied().unwrap_or(Value::Nil);
    let default = args.get(3).copied().unwrap_or(Value::Nil);
    let inherit = args.get(4).copied().unwrap_or(Value::Nil);

    let minibuffer_args = [
        prompt,
        initial,
        Value::Nil,
        Value::Nil,
        history,
        default,
        inherit,
    ];
    read_from_minibuffer(&minibuffer_args)
}

pub(crate) fn finish_read_string_in_vm_runtime(
    shared: &mut super::eval::VmSharedState<'_>,
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
pub(crate) fn builtin_read_number(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
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
    let default_val = args.get(1).copied().unwrap_or(Value::Nil);
    [
        prompt,
        Value::Nil,
        Value::Nil,
        Value::True,
        Value::Nil,
        default_val,
    ]
}

fn validate_read_number_result(result: Value) -> EvalResult {
    match result {
        Value::Int(_) | Value::Float(..) => Ok(result),
        _ => Err(signal("error", vec![Value::string("Not a number")])),
    }
}

pub(crate) fn finish_read_number_with_minibuffer(
    args: &[Value],
    mut read_from_minibuffer: impl FnMut(&[Value]) -> EvalResult,
) -> EvalResult {
    let minibuffer_args = read_number_minibuffer_args(args);
    validate_read_number_result(read_from_minibuffer(&minibuffer_args)?)
}

pub(crate) fn finish_read_number_in_eval(
    eval: &mut super::eval::Evaluator,
    args: &[Value],
) -> EvalResult {
    finish_read_number_with_minibuffer(args, |minibuffer_args| {
        finish_read_from_minibuffer_in_eval(eval, minibuffer_args)
    })
}

pub(crate) fn finish_read_number_in_vm_runtime(
    shared: &mut super::eval::VmSharedState<'_>,
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_completing_read_in_runtime(eval, &args)?;
    finish_completing_read_in_eval(eval, &args)
}

pub(crate) fn finish_completing_read_in_eval(
    eval: &mut super::eval::Evaluator,
    args: &[Value],
) -> EvalResult {
    let minibuffer_args = completing_read_minibuffer_args(eval.obarray(), args);
    let collection = args[1];
    eval.assign("minibuffer-completion-table", collection);
    let predicate = args.get(2).copied().unwrap_or(Value::Nil);
    eval.assign("minibuffer-completion-predicate", predicate);

    let result = finish_read_from_minibuffer_in_eval(eval, &minibuffer_args);

    eval.assign("minibuffer-completion-table", Value::Nil);
    eval.assign("minibuffer-completion-predicate", Value::Nil);

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
    args: &[Value],
    mut read_from_minibuffer: impl FnMut(&[Value]) -> EvalResult,
) -> EvalResult {
    let minibuffer_args = completing_read_minibuffer_args(obarray, args);
    let _ = crate::emacs_core::eval::set_runtime_binding_in_state(
        obarray,
        dynamic,
        buffers,
        custom,
        intern("minibuffer-completion-table"),
        args[1],
    );
    let _ = crate::emacs_core::eval::set_runtime_binding_in_state(
        obarray,
        dynamic,
        buffers,
        custom,
        intern("minibuffer-completion-predicate"),
        args.get(2).copied().unwrap_or(Value::Nil),
    );

    let result = read_from_minibuffer(&minibuffer_args);

    let _ = crate::emacs_core::eval::set_runtime_binding_in_state(
        obarray,
        dynamic,
        buffers,
        custom,
        intern("minibuffer-completion-table"),
        Value::Nil,
    );
    let _ = crate::emacs_core::eval::set_runtime_binding_in_state(
        obarray,
        dynamic,
        buffers,
        custom,
        intern("minibuffer-completion-predicate"),
        Value::Nil,
    );

    result
}

pub(crate) fn finish_read_from_minibuffer_in_vm_runtime(
    shared: &mut super::eval::VmSharedState<'_>,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_read_from_minibuffer_in_runtime(shared, args)?;
    let prompt = expect_string(&args[0])?;
    let initial_input = args.get(1).and_then(|v| match v {
        Value::Str(id) => Some(super::value::with_heap(|h| h.get_string(*id).to_owned())),
        _ => None,
    });
    let keymap_arg = args.get(2).copied().unwrap_or(Value::Nil);
    let read_arg = args.get(3).copied().unwrap_or(Value::Nil);
    let history_name = minibuffer_history_name(args.get(4));
    let default_val = args.get(5).copied().unwrap_or(Value::Nil);

    let saved_local_map = *shared.current_local_map;
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
        prompt_byte_len = prompt.len();
        if let Some(ref initial) = initial_input {
            buf.text.insert_str(prompt_byte_len, initial);
        }
        let total_len = buf.text.len();
        buf.widen();
        buf.goto_byte(total_len);
    }

    let active_window_state = activate_minibuffer_window_in_state(
        &mut *shared.frames,
        &mut *shared.buffers,
        &mut *shared.minibuffer_selected_window,
        &mut *shared.active_minibuffer_window,
        minibuf_id,
    );
    if active_window_state.is_none() {
        shared.buffers.set_current(minibuf_id);
    }
    tracing::debug!(
        "read-from-minibuffer: prompt={:?} minibuf_id={:?} current_buffer={:?} active_window={:?} selected_window={:?}",
        prompt,
        minibuf_id,
        shared.buffers.current_buffer_id(),
        *shared.active_minibuffer_window,
        shared
            .frames
            .selected_frame()
            .map(|frame| frame.selected_window)
    );

    let enable_recursive = shared
        .obarray
        .symbol_value("enable-recursive-minibuffers")
        .copied()
        .unwrap_or(Value::Nil)
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
            .unwrap_or(Value::Nil)
    };
    *shared.current_local_map = minibuf_keymap;
    shared
        .obarray
        .set_symbol_value("minibuffer-prompt", Value::string(&prompt));
    shared
        .obarray
        .set_symbol_value("minibuffer-depth", Value::Int(minibuf_depth as i64));

    let extra_roots = args.to_vec();
    let edit_result = shared.with_parent_evaluator_vm_roots(vm_gc_roots, &extra_roots, |eval| {
        eval.minibuffer_command_loop_inner()
    });

    let result_string = if let Some(buf) = shared.buffers.get(minibuf_id) {
        let total_len = buf.text.len();
        if total_len > prompt_byte_len {
            buf.buffer_substring(prompt_byte_len, total_len)
        } else {
            String::new()
        }
    } else {
        String::new()
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

    *shared.current_local_map = saved_local_map;
    if let Some(saved) = active_window_state {
        restore_minibuffer_window_in_state(
            &mut *shared.frames,
            &mut *shared.minibuffer_selected_window,
            &mut *shared.active_minibuffer_window,
            saved,
        );
    }
    if let Some(buf_id) = saved_buffer_id {
        shared.buffers.set_current(buf_id);
    }
    tracing::debug!(
        "read-from-minibuffer: restored current_buffer={:?} active_window={:?} selected_window={:?}",
        shared.buffers.current_buffer_id(),
        *shared.active_minibuffer_window,
        shared
            .frames
            .selected_frame()
            .map(|frame| frame.selected_window)
    );
    shared.obarray.set_symbol_value(
        "minibuffer-depth",
        Value::Int(shared.minibuffers.depth() as i64),
    );

    match edit_result {
        Ok(_) | Err(Flow::Throw { .. }) => {
            if !read_arg.is_nil() && !result_string.is_empty() {
                let read_result = builtin_read_from_string_in_state(
                    shared.obarray,
                    vec![Value::string(&result_string)],
                )?;
                if let Value::Cons(id) = read_result {
                    let snap = super::value::read_cons(id);
                    return Ok(snap.car);
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
    shared: &mut super::eval::VmSharedState<'_>,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> EvalResult {
    builtin_completing_read_in_runtime(shared, args)?;
    let minibuffer_args = completing_read_minibuffer_args(&*shared.obarray, args);
    let _ = crate::emacs_core::eval::set_runtime_binding_in_state(
        shared.obarray,
        shared.dynamic.as_mut_slice(),
        shared.buffers,
        &*shared.custom,
        intern("minibuffer-completion-table"),
        args[1],
    );
    let _ = crate::emacs_core::eval::set_runtime_binding_in_state(
        shared.obarray,
        shared.dynamic.as_mut_slice(),
        shared.buffers,
        &*shared.custom,
        intern("minibuffer-completion-predicate"),
        args.get(2).copied().unwrap_or(Value::Nil),
    );
    let result = finish_read_from_minibuffer_in_vm_runtime(shared, vm_gc_roots, &minibuffer_args);
    let _ = crate::emacs_core::eval::set_runtime_binding_in_state(
        shared.obarray,
        shared.dynamic.as_mut_slice(),
        shared.buffers,
        &*shared.custom,
        intern("minibuffer-completion-table"),
        Value::Nil,
    );
    let _ = crate::emacs_core::eval::set_runtime_binding_in_state(
        shared.obarray,
        shared.dynamic.as_mut_slice(),
        shared.buffers,
        &*shared.custom,
        intern("minibuffer-completion-predicate"),
        Value::Nil,
    );
    result
}

pub(crate) fn completing_read_minibuffer_args(obarray: &Obarray, args: &[Value]) -> [Value; 7] {
    let prompt = args[0];
    let require_match = args.get(3).copied().unwrap_or(Value::Nil);
    let initial_input = args.get(4).copied().unwrap_or(Value::Nil);
    let hist = args.get(5).copied().unwrap_or(Value::Nil);
    let default_val = args.get(6).copied().unwrap_or(Value::Nil);
    let inherit = args.get(7).copied().unwrap_or(Value::Nil);

    let keymap = if !require_match.is_nil() {
        obarray
            .symbol_value("minibuffer-local-must-match-map")
            .copied()
            .unwrap_or(Value::Nil)
    } else {
        obarray
            .symbol_value("minibuffer-local-completion-map")
            .copied()
            .unwrap_or(Value::Nil)
    };

    [
        prompt,
        initial_input,
        keymap,
        Value::Nil,
        hist,
        default_val,
        inherit,
    ]
}

fn event_to_int(event: &Value) -> Option<i64> {
    match event {
        Value::Int(n) => Some(*n),
        Value::Char(c) => Some(*c as i64),
        _ => None,
    }
}

fn event_to_char(event: &Value) -> Option<char> {
    match event {
        Value::Char(c) => Some(*c),
        Value::Int(n) if *n >= 0 => char::from_u32(*n as u32),
        _ => None,
    }
}

fn expect_optional_prompt_string(args: &[Value]) -> Result<(), Flow> {
    if args.is_empty() || args[0].is_nil() || matches!(args[0], Value::Str(_)) {
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
    fn read_key_sequence_blocking(&mut self) -> Result<(Vec<Value>, Value), Flow>;
}

impl KeyboardInputRuntime for super::eval::Evaluator {
    fn pop_unread_command_event(&mut self) -> Option<Value> {
        super::eval::Evaluator::pop_unread_command_event(self)
    }

    fn peek_unread_command_event(&self) -> Option<Value> {
        super::eval::Evaluator::peek_unread_command_event(self)
    }

    fn replace_unread_command_event_with_singleton(&mut self, event: Value) {
        super::eval::Evaluator::replace_unread_command_event_with_singleton(self, event);
    }

    fn record_input_event(&mut self, event: Value) {
        super::eval::Evaluator::record_input_event(self, event);
    }

    fn record_nonmenu_input_event(&mut self, event: Value) {
        super::eval::Evaluator::record_nonmenu_input_event(self, event);
    }

    fn set_read_command_keys(&mut self, keys: Vec<Value>) {
        super::eval::Evaluator::set_read_command_keys(self, keys);
    }

    fn clear_read_command_keys(&mut self) {
        super::eval::Evaluator::clear_read_command_keys(self);
    }

    fn read_command_keys(&self) -> &[Value] {
        super::eval::Evaluator::read_command_keys(self)
    }

    fn has_input_receiver(&self) -> bool {
        super::eval::Evaluator::has_input_receiver(self)
    }

    fn read_char_blocking(&mut self) -> Result<Value, Flow> {
        super::eval::Evaluator::read_char(self)
    }

    fn read_key_sequence_blocking(&mut self) -> Result<(Vec<Value>, Value), Flow> {
        super::eval::Evaluator::read_key_sequence(self)
    }
}

impl KeyboardInputRuntime for super::eval::VmSharedState<'_> {
    fn pop_unread_command_event(&mut self) -> Option<Value> {
        super::eval::VmSharedState::pop_unread_command_event(self)
    }

    fn peek_unread_command_event(&self) -> Option<Value> {
        super::eval::VmSharedState::peek_unread_command_event(self)
    }

    fn replace_unread_command_event_with_singleton(&mut self, event: Value) {
        super::eval::VmSharedState::replace_unread_command_event_with_singleton(self, event);
    }

    fn record_input_event(&mut self, event: Value) {
        super::eval::VmSharedState::record_input_event(self, event);
    }

    fn record_nonmenu_input_event(&mut self, event: Value) {
        super::eval::VmSharedState::record_nonmenu_input_event(self, event);
    }

    fn set_read_command_keys(&mut self, keys: Vec<Value>) {
        super::eval::VmSharedState::set_read_command_keys(self, keys);
    }

    fn clear_read_command_keys(&mut self) {
        super::eval::VmSharedState::clear_read_command_keys(self);
    }

    fn read_command_keys(&self) -> &[Value] {
        super::eval::VmSharedState::read_command_keys(self)
    }

    fn has_input_receiver(&self) -> bool {
        super::eval::VmSharedState::has_input_receiver(self)
    }

    fn read_char_blocking(&mut self) -> Result<Value, Flow> {
        super::eval::VmSharedState::with_parent_evaluator(self, |eval| eval.read_char())
    }

    fn read_key_sequence_blocking(&mut self) -> Result<(Vec<Value>, Value), Flow> {
        super::eval::VmSharedState::with_parent_evaluator(self, |eval| eval.read_key_sequence())
    }
}

// ---------------------------------------------------------------------------
// 10. input-pending-p
// ---------------------------------------------------------------------------

/// `(input-pending-p &optional CHECK-TIMERS)`
///
/// Return non-nil when `unread-command-events` has at least one pending event.
/// `CHECK-TIMERS` is currently accepted for arity compatibility and ignored.
pub(crate) fn builtin_input_pending_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_input_pending_p_in_state(&eval.obarray, eval.dynamic.as_slice(), args)
}

pub(crate) fn builtin_input_pending_p_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("input-pending-p", &args, 1)?;
    Ok(Value::bool(
        peek_unread_command_event_in_state(obarray, dynamic).is_some(),
    ))
}

// ---------------------------------------------------------------------------
// 11. discard-input
// ---------------------------------------------------------------------------

/// `(discard-input)`
///
/// Discard pending unread command events for the current scope.
pub(crate) fn builtin_discard_input(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_discard_input_in_state(
        &mut eval.obarray,
        eval.dynamic.as_mut_slice(),
        &mut eval.buffers,
        &eval.custom,
        args,
    )
}

pub(crate) fn builtin_discard_input_in_state(
    obarray: &mut Obarray,
    dynamic: &mut [OrderedRuntimeBindingMap],
    buffers: &mut crate::buffer::BufferManager,
    custom: &CustomManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("discard-input", &args, 0)?;
    super::eval::set_runtime_binding_in_state(
        obarray,
        dynamic,
        buffers,
        custom,
        intern("unread-command-events"),
        Value::Nil,
    );
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// 12. current-input-mode / set-input-mode
// ---------------------------------------------------------------------------

/// `(current-input-mode)` -> `(INTERRUPT FLOW META QUIT)`
pub(crate) fn builtin_current_input_mode(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let (interrupt, _flow, _meta, _quit) = eval.current_input_mode_tuple();
    builtin_current_input_mode_in_state(interrupt, args)
}

pub(crate) fn builtin_current_input_mode_in_state(
    input_mode_interrupt: bool,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-input-mode", &args, 0)?;
    Ok(Value::list(vec![
        Value::bool(input_mode_interrupt),
        Value::Nil,
        Value::True,
        Value::Int(7),
    ]))
}

/// `(set-input-mode INTERRUPT FLOW META QUIT)`
///
/// Batch-compatible behavior currently tracks only INTERRUPT and ignores
/// FLOW/META/QUIT while preserving arity/return-value semantics.
pub(crate) fn builtin_set_input_mode(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-input-mode", &args, 3)?;
    expect_max_args("set-input-mode", &args, 4)?;
    eval.set_input_mode_interrupt(args[0].is_truthy());
    Ok(Value::Nil)
}

pub(crate) fn builtin_set_input_mode_in_state(
    input_mode_interrupt: &mut bool,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-input-mode", &args, 3)?;
    expect_max_args("set-input-mode", &args, 4)?;
    *input_mode_interrupt = args[0].is_truthy();
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// 13. input mode helper setters
// ---------------------------------------------------------------------------

/// `(set-input-interrupt-mode INTERRUPT)`
pub(crate) fn builtin_set_input_interrupt_mode(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-input-interrupt-mode", &args, 1)?;
    eval.set_input_mode_interrupt(args[0].is_truthy());
    Ok(Value::Nil)
}

pub(crate) fn builtin_set_input_interrupt_mode_in_state(
    input_mode_interrupt: &mut bool,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-input-interrupt-mode", &args, 1)?;
    *input_mode_interrupt = args[0].is_truthy();
    Ok(Value::Nil)
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
        Some(Value::Cons(cell)) => Some(read_cons(cell).car),
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
            vec![Value::symbol("read-char"), Value::Int(args.len() as i64)],
        ));
    }
    expect_optional_prompt_string(args)?;
    let seconds_is_nil_or_omitted = args.get(2).is_none_or(Value::is_nil);

    if let Some(event) = runtime.peek_unread_command_event() {
        if let Some(n) = event_to_int(&event) {
            let event = runtime
                .pop_unread_command_event()
                .expect("peeked unread event should still be present");
            if runtime.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                runtime.set_read_command_keys(vec![event]);
            }
            return Ok(Some(Value::Int(n)));
        }
        runtime.replace_unread_command_event_with_singleton(event);
        runtime.record_input_event(event);
        return Err(non_character_input_event_error());
    }

    if runtime.has_input_receiver() {
        Ok(None)
    } else {
        Ok(Some(Value::Nil))
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
            return Ok(Some(Value::vector(vec![Value::Int(n)])));
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
    Ok(Value::Nil)
}

/// `(set-output-flow-control FLOW)`
///
/// Batch-compatible behavior: accepts one argument and returns nil.
pub(crate) fn builtin_set_output_flow_control(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-output-flow-control", &args, 1)?;
    expect_max_args("set-output-flow-control", &args, 2)?;
    Ok(Value::Nil)
}

/// `(set-quit-char CHAR)`
///
/// Batch-compatible behavior: accepts one argument and returns nil.
pub(crate) fn builtin_set_quit_char(args: Vec<Value>) -> EvalResult {
    expect_args("set-quit-char", &args, 1)?;
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// 14. waiting-for-user-input-p
// ---------------------------------------------------------------------------

/// `(waiting-for-user-input-p)`
///
/// Batch-mode compatibility: always returns nil.
pub(crate) fn builtin_waiting_for_user_input_p(args: Vec<Value>) -> EvalResult {
    expect_args("waiting-for-user-input-p", &args, 0)?;
    Ok(Value::Nil)
}

pub(crate) fn builtin_waiting_for_user_input_p_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_waiting_for_user_input_p_in_state(eval.waiting_for_user_input(), args)
}

pub(crate) fn builtin_waiting_for_user_input_p_in_state(
    waiting_for_user_input: bool,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("waiting-for-user-input-p", &args, 0)?;
    Ok(Value::bool(waiting_for_user_input))
}

// ---------------------------------------------------------------------------
// 15. y-or-n-p
// ---------------------------------------------------------------------------

/// `(y-or-n-p PROMPT)`
///
/// Ask user a yes-or-no question. Returns t for 'y', nil for 'n'.
/// In interactive mode, reads a single character.
/// In batch mode, signals end-of-file.
pub(crate) fn builtin_y_or_n_p(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("y-or-n-p", &args, 1)?;
    match &args[0] {
        Value::Str(_) | Value::Vector(_) | Value::Nil => {}
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("sequencep"), *other],
            ));
        }
    }

    // Interactive mode: read single character
    if eval.input_rx.is_some() {
        // Display prompt in echo area (message)
        if let Value::Str(id) = &args[0] {
            let prompt_str = super::value::with_heap(|h| h.get_string(*id).to_owned());
            let msg = format!("{} (y or n) ", prompt_str);
            eval.assign("minibuffer-message", Value::string(&msg));
        }
        loop {
            let event = eval.read_char()?;
            if let Some(n) = event_to_int(&event) {
                let ch = char::from_u32(n as u32).unwrap_or('\0');
                match ch {
                    'y' | 'Y' => return Ok(Value::True),
                    'n' | 'N' => return Ok(Value::Nil),
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
pub(crate) fn builtin_yes_or_no_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_yes_or_no_p_in_runtime(eval, &args)?;
    finish_yes_or_no_p_in_eval(eval, &args)
}

pub(crate) fn finish_yes_or_no_p_in_eval(
    eval: &mut super::eval::Evaluator,
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
    let prompt_str = if let Value::Str(id) = &args[0] {
        super::value::with_heap(|h| h.get_string(*id).to_owned())
    } else {
        String::new()
    };
    loop {
        let full_prompt = format!("{} (yes or no) ", prompt_str);
        let result = read_from_minibuffer(&[Value::string(&full_prompt)])?;
        if let Value::Str(id) = result {
            let answer = super::value::with_heap(|h| h.get_string(id).to_owned());
            match answer.trim() {
                "yes" => return Ok(Value::True),
                "no" => return Ok(Value::Nil),
                _ => continue,
            }
        }
    }
}

pub(crate) fn finish_yes_or_no_p_in_vm_runtime(
    shared: &mut super::eval::VmSharedState<'_>,
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
    if !matches!(args[0], Value::Str(_)) {
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
pub(crate) fn builtin_read_char(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    if let Some(value) = builtin_read_char_in_runtime(eval, &args)? {
        return Ok(value);
    }

    finish_read_char_in_eval(eval, &args)
}

pub(crate) fn finish_read_char_in_eval(
    eval: &mut super::eval::Evaluator,
    args: &[Value],
) -> EvalResult {
    finish_read_char_interactive_in_runtime(eval, args)
}

pub(crate) fn finish_read_char_interactive_in_runtime(
    runtime: &mut impl KeyboardInputRuntime,
    args: &[Value],
) -> EvalResult {
    if runtime.has_input_receiver() {
        let event = runtime.read_char_blocking()?;
        let seconds_is_nil_or_omitted = args.get(2).is_none_or(Value::is_nil);
        if let Some(n) = event_to_int(&event) {
            if runtime.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                runtime.set_read_command_keys(vec![event]);
            }
            return Ok(Value::Int(n));
        }
        runtime.replace_unread_command_event_with_singleton(event);
        runtime.record_input_event(event);
        return Err(non_character_input_event_error());
    }

    Ok(Value::Nil)
}

/// `(read-key &optional PROMPT)`
///
/// Read a key from the command input.
/// In batch mode, returns next `unread-command-events` event, else nil.
/// In interactive mode, blocks on the input channel via `read_char()`.
pub(crate) fn builtin_read_key(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("read-key"), Value::Int(args.len() as i64)],
        ));
    }
    expect_optional_prompt_string(&args)?;

    // 1. Check unread-command-events first
    if let Some(event) = eval.pop_unread_command_event() {
        eval.record_nonmenu_input_event(event);
        eval.set_read_command_keys(vec![event]);
        if let Some(n) = event_to_int(&event) {
            return Ok(Value::Int(n));
        }
        return Ok(event);
    }

    // 2. Interactive mode: block on input channel
    if eval.input_rx.is_some() {
        let event = eval.read_char()?;
        eval.record_nonmenu_input_event(event);
        eval.set_read_command_keys(vec![event]);
        if let Some(n) = event_to_int(&event) {
            return Ok(Value::Int(n));
        }
        return Ok(event);
    }

    // 3. Batch mode: no input
    eval.clear_read_command_keys();
    Ok(Value::Nil)
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(value) = builtin_read_key_sequence_in_runtime(eval, &args)? {
        return Ok(value);
    }

    finish_read_key_sequence_in_eval(eval)
}

pub(crate) fn finish_read_key_sequence_in_eval(eval: &mut super::eval::Evaluator) -> EvalResult {
    finish_read_key_sequence_interactive_in_runtime(eval)
}

pub(crate) fn finish_read_key_sequence_interactive_in_runtime(
    runtime: &mut impl KeyboardInputRuntime,
) -> EvalResult {
    if runtime.has_input_receiver() {
        let (keys, _binding) = runtime.read_key_sequence_blocking()?;
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
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if let Some(value) = builtin_read_key_sequence_vector_in_runtime(eval, &args)? {
        return Ok(value);
    }
    finish_read_key_sequence_vector_interactive_in_runtime(eval)
}

pub(crate) fn finish_read_key_sequence_vector_interactive_in_runtime(
    runtime: &mut impl KeyboardInputRuntime,
) -> EvalResult {
    if runtime.has_input_receiver() {
        let (keys, _binding) = runtime.read_key_sequence_blocking()?;
        return Ok(Value::vector(keys));
    }

    runtime.clear_read_command_keys();
    Ok(Value::vector(vec![]))
}

// ---------------------------------------------------------------------------
// 14. with-output-to-string (special form)
// ---------------------------------------------------------------------------

/// Special form: `(with-output-to-string BODY...)`
///
/// Evaluate BODY, capturing output from print functions into a temporary
/// buffer bound through `standard-output`.
pub(crate) fn sf_with_output_to_string(
    eval: &mut super::eval::Evaluator,
    tail: &[Expr],
) -> EvalResult {
    let temp_name = eval
        .buffers
        .generate_new_buffer_name(" *with-output-to-string*");
    let temp_id = eval.buffers.create_buffer(&temp_name);

    let mut frame = OrderedRuntimeBindingMap::new();
    frame.insert(intern("standard-output"), Value::Buffer(temp_id));
    eval.dynamic.push(frame);

    let body_result = eval.sf_progn(tail);
    let captured = eval
        .buffers
        .get(temp_id)
        .map(|buf| buf.buffer_string())
        .unwrap_or_default();

    let _ = eval.dynamic.pop();
    eval.buffers.kill_buffer(temp_id);

    body_result.map(|_| Value::string(captured))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "reader_test.rs"]
mod tests;
