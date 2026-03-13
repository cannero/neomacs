//! Reader/printer builtins: read-from-string, read, prin1-to-string (enhanced),
//! format-spec, and various interactive-input stubs.

use super::error::{EvalResult, Flow, signal};
use super::expr::Expr;
use super::intern::{SymId, intern, resolve_sym};
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

    let value = if let Some(bytecode) = first_form_byte_code_literal_value(eval, &expr) {
        bytecode
    } else if let Some(hash_table) = first_form_hash_table_literal_value(eval, &expr) {
        hash_table
    } else {
        eval.quote_to_runtime_value(&expr)
    };
    let absolute_end = start + end_pos;

    Ok(Value::cons(value, Value::Int(absolute_end as i64)))
}

fn first_form_byte_code_literal_value(
    eval: &mut super::eval::Evaluator,
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
        .map(|value| eval.quote_to_runtime_value(value))
        .collect();
    Some(Value::vector(values))
}

fn first_form_hash_table_literal_value(
    eval: &mut super::eval::Evaluator,
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
        let value = eval.quote_to_runtime_value(&spec[i + 1]);
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
                    let key_value = eval.quote_to_runtime_value(&data_items[idx]);
                    let val_value = eval.quote_to_runtime_value(&data_items[idx + 1]);
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
            let result = builtin_read_from_string(eval, args)?;
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
                let buf = eval
                    .buffers
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
            let value = if let Some(bytecode) = first_form_byte_code_literal_value(eval, &expr) {
                bytecode
            } else if let Some(hash_table) = first_form_hash_table_literal_value(eval, &expr) {
                hash_table
            } else {
                eval.quote_to_runtime_value(&expr)
            };
            // Advance point past the read form
            let new_pt = pt + end_offset;
            if let Some(buf) = eval.buffers.get_mut(buf_id) {
                buf.pt = new_pt;
            }
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
    expect_min_args("read-from-minibuffer", &args, 1)?;
    expect_max_args("read-from-minibuffer", &args, 7)?;
    let prompt = expect_string(&args[0])?;
    if let Some(initial) = args.get(1) {
        expect_initial_input_stringish(initial)?;
    }

    // Interactive mode: use the minibuffer with recursive edit
    if eval.input_rx.is_some() {
        return read_from_minibuffer_interactive(eval, &prompt, &args);
    }

    // Batch mode: signal end-of-file
    if eval.peek_unread_command_event().is_some() {
        return Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
}

/// Interactive read-from-minibuffer implementation.
///
/// Mirrors GNU Emacs `read_minibuf()` in minibuf.c:
/// 1. Save current buffer/keymap state
/// 2. Set up *Minibuf-N* buffer with prompt
/// 3. Set local keymap (minibuffer-local-map or KEYMAP arg)
/// 4. Enter recursive edit (command loop runs in minibuffer)
/// 5. User presses RET → exit-minibuffer → throw 'exit
/// 6. Read buffer contents after prompt, restore state, return string
fn read_from_minibuffer_interactive(
    eval: &mut super::eval::Evaluator,
    prompt: &str,
    args: &[Value],
) -> EvalResult {
    // Extract optional arguments
    let initial_input = args.get(1).and_then(|v| match v {
        Value::Str(id) => Some(super::value::with_heap(|h| h.get_string(*id).to_owned())),
        _ => None,
    });
    let keymap_arg = args.get(2).copied().unwrap_or(Value::Nil);
    let read_arg = args.get(3).copied().unwrap_or(Value::Nil);
    let default_val = args.get(5).copied().unwrap_or(Value::Nil);

    // Save state
    let saved_local_map = eval.current_local_map;
    let saved_buffer_id = eval.buffer_manager().current_buffer().map(|b| b.id);

    // Find or create *Minibuf-N* buffer
    let depth = eval.command_loop.recursive_depth;
    let minibuf_name = format!(" *Minibuf-{}*", depth);
    let minibuf_id = eval
        .buffer_manager()
        .find_buffer_by_name(&minibuf_name)
        .unwrap_or_else(|| eval.buffer_manager_mut().create_buffer(&minibuf_name));

    // Clear the minibuffer buffer and insert prompt + initial input
    let prompt_byte_len;
    {
        let buf = eval.buffer_manager_mut().get_mut(minibuf_id).unwrap();
        let text_len = buf.text.len();
        if text_len > 0 {
            buf.text.delete_range(0, text_len);
        }
        buf.text.insert_str(0, prompt);
        prompt_byte_len = prompt.len();
        if let Some(ref initial) = initial_input {
            buf.text.insert_str(prompt_byte_len, initial);
        }
        let total_len = buf.text.len();
        buf.begv = 0;
        buf.zv = total_len;
        buf.pt = total_len; // cursor at end of initial input
    }

    // Switch to minibuffer buffer
    eval.buffer_manager_mut().set_current(minibuf_id);

    // Set local keymap: use KEYMAP arg if provided, otherwise minibuffer-local-map
    let minibuf_keymap = if !keymap_arg.is_nil() {
        keymap_arg
    } else {
        eval.obarray()
            .symbol_value("minibuffer-local-map")
            .copied()
            .unwrap_or(Value::Nil)
    };
    eval.current_local_map = minibuf_keymap;

    // Set minibuffer-related variables
    eval.assign("minibuffer-prompt", Value::string(prompt));
    let prev_depth = eval
        .obarray()
        .symbol_value("minibuffer-depth")
        .copied()
        .unwrap_or(Value::Int(0));
    eval.assign("minibuffer-depth", Value::Int(depth as i64));

    // Enter recursive edit — the command loop runs until exit-minibuffer throws 'exit
    let edit_result = eval.recursive_edit_inner();

    // Read the minibuffer contents (everything after the prompt)
    let result_string = if let Some(buf) = eval.buffer_manager().get(minibuf_id) {
        let total_len = buf.text.len();
        if total_len > prompt_byte_len {
            buf.buffer_substring(prompt_byte_len, total_len)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // Restore state
    eval.current_local_map = saved_local_map;
    if let Some(buf_id) = saved_buffer_id {
        eval.buffer_manager_mut().set_current(buf_id);
    }
    eval.assign("minibuffer-depth", prev_depth);

    // Handle the recursive edit result
    match edit_result {
        Ok(_) | Err(Flow::Throw { .. }) => {
            // Normal exit (throw 'exit from exit-minibuffer)
            // If READ arg is non-nil, evaluate the result as a Lisp expression
            if !read_arg.is_nil() && !result_string.is_empty() {
                // READ is non-nil: parse the result string as a Lisp expression
                // (like calling (read STRING)) and return the parsed object.
                let read_result =
                    builtin_read_from_string(eval, vec![Value::string(&result_string)])?;
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
    expect_min_args("read-string", &args, 1)?;
    expect_max_args("read-string", &args, 5)?;
    let prompt = args[0];
    if let Some(initial) = args.get(1) {
        expect_initial_input_stringish(initial)?;
    }

    // Build args for read-from-minibuffer:
    // (read-from-minibuffer PROMPT INITIAL nil nil HIST DEFAULT INHERIT-INPUT-METHOD)
    let initial = args.get(1).copied().unwrap_or(Value::Nil);
    let history = args.get(2).copied().unwrap_or(Value::Nil);
    let default = args.get(3).copied().unwrap_or(Value::Nil);
    let inherit = args.get(4).copied().unwrap_or(Value::Nil);

    builtin_read_from_minibuffer(
        eval,
        vec![
            prompt,
            initial,
            Value::Nil,
            Value::Nil,
            history,
            default,
            inherit,
        ],
    )
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
    expect_min_args("read-number", &args, 1)?;
    expect_max_args("read-number", &args, 3)?;
    let prompt = args[0];
    expect_string(&prompt)?;
    if let Some(default) = args.get(1) {
        if !default.is_nil() {
            expect_number(default)?;
        }
    }

    // Interactive mode: use read-from-minibuffer with READ=t
    if eval.input_rx.is_some() {
        let default_val = args.get(1).copied().unwrap_or(Value::Nil);
        let result = builtin_read_from_minibuffer(
            eval,
            vec![
                prompt,
                Value::Nil,
                Value::Nil,
                Value::True,
                Value::Nil,
                default_val,
            ],
        )?;
        // Validate result is a number
        match result {
            Value::Int(_) | Value::Float(..) => return Ok(result),
            _ => {
                return Err(signal("error", vec![Value::string("Not a number")]));
            }
        }
    }

    // Batch mode
    if eval.peek_unread_command_event().is_some() {
        return Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
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
    expect_min_args("completing-read", &args, 2)?;
    expect_max_args("completing-read", &args, 8)?;
    let prompt = args[0];
    expect_string(&prompt)?;
    if let Some(initial) = args.get(4) {
        expect_completing_read_initial_input(initial)?;
    }

    // Interactive mode: use read-from-minibuffer with completion keymap
    if eval.input_rx.is_some() {
        let require_match = args.get(3).copied().unwrap_or(Value::Nil);
        let initial_input = args.get(4).copied().unwrap_or(Value::Nil);
        let hist = args.get(5).copied().unwrap_or(Value::Nil);
        let default_val = args.get(6).copied().unwrap_or(Value::Nil);
        let inherit = args.get(7).copied().unwrap_or(Value::Nil);

        // Choose keymap: must-match or completion
        let keymap = if !require_match.is_nil() {
            eval.obarray()
                .symbol_value("minibuffer-local-must-match-map")
                .copied()
                .unwrap_or(Value::Nil)
        } else {
            eval.obarray()
                .symbol_value("minibuffer-local-completion-map")
                .copied()
                .unwrap_or(Value::Nil)
        };

        // Store completion table for TAB completion (minibuffer-completion-table)
        let collection = args[1];
        eval.assign("minibuffer-completion-table", collection);
        let predicate = args.get(2).copied().unwrap_or(Value::Nil);
        eval.assign("minibuffer-completion-predicate", predicate);

        let result = builtin_read_from_minibuffer(
            eval,
            vec![
                prompt,
                initial_input,
                keymap,
                Value::Nil,
                hist,
                default_val,
                inherit,
            ],
        );

        // Clean up completion state
        eval.assign("minibuffer-completion-table", Value::Nil);
        eval.assign("minibuffer-completion-predicate", Value::Nil);

        return result;
    }

    // Batch mode
    if eval.peek_unread_command_event().is_some() {
        return Err(signal(
            "end-of-file",
            vec![Value::string("Error reading from stdin")],
        ));
    }
    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
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
    expect_max_args("input-pending-p", &args, 1)?;
    Ok(Value::bool(eval.peek_unread_command_event().is_some()))
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
    expect_args("discard-input", &args, 0)?;
    eval.assign("unread-command-events", Value::Nil);
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
    expect_args("current-input-mode", &args, 0)?;
    let (interrupt, flow, meta, quit) = eval.current_input_mode_tuple();
    Ok(Value::list(vec![
        Value::bool(interrupt),
        Value::bool(flow),
        Value::bool(meta),
        Value::Int(quit),
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
    expect_args("yes-or-no-p", &args, 1)?;
    if !matches!(args[0], Value::Str(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        ));
    }

    // Interactive mode: read "yes" or "no" from minibuffer
    if eval.input_rx.is_some() {
        let prompt_str = if let Value::Str(id) = &args[0] {
            super::value::with_heap(|h| h.get_string(*id).to_owned())
        } else {
            String::new()
        };
        loop {
            let full_prompt = format!("{} (yes or no) ", prompt_str);
            let result = builtin_read_from_minibuffer(eval, vec![Value::string(&full_prompt)])?;
            if let Value::Str(id) = result {
                let answer = super::value::with_heap(|h| h.get_string(id).to_owned());
                match answer.trim() {
                    "yes" => return Ok(Value::True),
                    "no" => return Ok(Value::Nil),
                    _ => continue, // Ask again
                }
            }
        }
    }

    Err(signal(
        "end-of-file",
        vec![Value::string("Error reading from stdin")],
    ))
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
    if args.len() > 3 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("read-char"), Value::Int(args.len() as i64)],
        ));
    }
    expect_optional_prompt_string(&args)?;
    let seconds_is_nil_or_omitted = args.get(2).is_none_or(Value::is_nil);

    // 1. Check unread-command-events first (both batch and interactive)
    if let Some(event) = eval.peek_unread_command_event() {
        if let Some(n) = event_to_int(&event) {
            let _ = eval.pop_unread_command_event();
            if eval.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                eval.set_read_command_keys(vec![event]);
            }
            return Ok(Value::Int(n));
        }
        eval.assign("unread-command-events", Value::list(vec![event]));
        eval.record_input_event(event);
        return Err(non_character_input_event_error());
    }

    // 2. Interactive mode: block on input channel
    if eval.input_rx.is_some() {
        let event = eval.read_char()?;
        if let Some(n) = event_to_int(&event) {
            if eval.read_command_keys().is_empty() && seconds_is_nil_or_omitted {
                eval.set_read_command_keys(vec![event]);
            }
            return Ok(Value::Int(n));
        }
        // Non-character event: push back and signal error
        eval.assign("unread-command-events", Value::list(vec![event]));
        eval.record_input_event(event);
        return Err(non_character_input_event_error());
    }

    // 3. Batch mode: no input available
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
    expect_min_args("read-key-sequence", &args, 1)?;
    expect_max_args("read-key-sequence", &args, 6)?;
    expect_optional_prompt_string(&args)?;

    // 1. Check unread-command-events first
    if let Some(event) = eval.pop_unread_command_event() {
        eval.record_nonmenu_input_event(event);
        eval.set_read_command_keys(vec![event]);
        if let Some(c) = event_to_char(&event) {
            return Ok(Value::string(c.to_string()));
        }
        return Ok(Value::vector(vec![event]));
    }

    // 2. Interactive mode: use the full key sequence reader
    if eval.input_rx.is_some() {
        let (keys, _binding) = eval.read_key_sequence()?;
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

    // 3. Batch mode: no input
    eval.clear_read_command_keys();
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
    expect_min_args("read-key-sequence-vector", &args, 1)?;
    expect_max_args("read-key-sequence-vector", &args, 6)?;
    expect_optional_prompt_string(&args)?;
    if let Some(event) = eval.pop_unread_command_event() {
        eval.record_nonmenu_input_event(event);
        eval.set_read_command_keys(vec![event]);
        if let Some(n) = event_to_int(&event) {
            return Ok(Value::vector(vec![Value::Int(n)]));
        }
        return Ok(Value::vector(vec![event]));
    }
    eval.clear_read_command_keys();
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

    let mut frame = OrderedSymMap::new();
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
