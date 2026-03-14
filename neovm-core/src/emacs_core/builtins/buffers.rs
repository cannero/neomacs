use super::*;

// ===========================================================================
// Buffer operations (require evaluator for BufferManager access)
// ===========================================================================

use crate::buffer::BufferId;

pub(super) fn expect_buffer_id(value: &Value) -> Result<BufferId, Flow> {
    match value {
        Value::Buffer(id) => Ok(*id),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("bufferp"), *other],
        )),
    }
}

fn point_char_pos(buf: &crate::buffer::Buffer, byte_pos: usize) -> i64 {
    buf.text.byte_to_char(byte_pos) as i64 + 1
}

fn canonicalize_or_self(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// (get-buffer-create NAME) → buffer
pub(crate) fn builtin_get_buffer_create(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("get-buffer-create", &args, 1)?;
    expect_max_args("get-buffer-create", &args, 2)?;
    let name = expect_string(&args[0])?;
    if let Some(id) = eval.buffers.find_buffer_by_name(&name) {
        Ok(Value::Buffer(id))
    } else {
        let id = eval.buffers.create_buffer(&name);
        Ok(Value::Buffer(id))
    }
}

/// (get-buffer NAME-OR-BUFFER) → buffer or nil
pub(crate) fn builtin_get_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("get-buffer", &args, 1)?;
    match &args[0] {
        Value::Buffer(_) => Ok(args[0]),
        Value::Str(id) => {
            let s = with_heap(|h| h.get_string(*id).to_owned());
            if let Some(buf_id) = eval.buffers.find_buffer_by_name(&s) {
                Ok(Value::Buffer(buf_id))
            } else {
                Ok(Value::Nil)
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

/// `(find-buffer VARIABLE VALUE)` -> buffer or nil.
///
/// Returns the first live buffer whose VARIABLE value is `eq` to VALUE.
/// Buffer-local bindings take precedence; otherwise dynamic/global bindings are
/// used as fallback. Signals `void-variable` when VARIABLE is unbound.
pub(crate) fn builtin_find_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("find-buffer", &args, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let target_value = args[1];

    let name_id = intern(name);
    let fallback_value = eval
        .dynamic
        .iter()
        .rev()
        .find_map(|frame| frame.get(&name_id).cloned())
        .or_else(|| eval.obarray().symbol_value(name).cloned())
        .ok_or_else(|| signal("void-variable", vec![Value::symbol(name)]))?;

    let mut scan_order = Vec::new();
    let current_id = eval.buffers.current_buffer().map(|buf| buf.id);
    if let Some(id) = current_id {
        scan_order.push(id);
    }
    for id in eval.buffers.buffer_list() {
        if Some(id) != current_id {
            scan_order.push(id);
        }
    }

    for id in scan_order {
        let Some(buf) = eval.buffers.get(id) else {
            continue;
        };
        let observed = buf
            .get_buffer_local(name)
            .cloned()
            .unwrap_or(fallback_value);
        if eq_value(&observed, &target_value) {
            return Ok(Value::Buffer(id));
        }
    }

    Ok(Value::Nil)
}

/// `(delete-all-overlays &optional BUFFER)` -> nil
///
/// Removes every overlay from BUFFER (or the current buffer when omitted/nil).
pub(crate) fn builtin_delete_all_overlays(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("delete-all-overlays", &args, 1)?;
    let target = if args.is_empty() || args[0].is_nil() {
        eval.buffers.current_buffer().map(|buf| buf.id)
    } else {
        Some(expect_buffer_id(&args[0])?)
    };

    let Some(target_id) = target else {
        return Ok(Value::Nil);
    };
    if eval.buffers.get(target_id).is_none() {
        // GNU Emacs treats dead buffers as a no-op.
        return Ok(Value::Nil);
    }
    let _ = eval.buffers.delete_all_buffer_overlays(target_id);
    Ok(Value::Nil)
}

/// (buffer-live-p OBJECT) -> t or nil
pub(crate) fn builtin_buffer_live_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("buffer-live-p", &args, 1)?;
    match &args[0] {
        Value::Buffer(id) => Ok(Value::bool(eval.buffers.get(*id).is_some())),
        _ => Ok(Value::Nil),
    }
}

/// (get-file-buffer FILENAME) -> buffer or nil
pub(crate) fn builtin_get_file_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("get-file-buffer", &args, 1)?;
    let filename = expect_string(&args[0])?;
    let resolved = super::fileio::resolve_filename_for_eval(eval, &filename);
    let resolved_true = canonicalize_or_self(&resolved);

    for id in eval.buffers.buffer_list() {
        let Some(buf) = eval.buffers.get(id) else {
            continue;
        };
        let Some(file_name) = &buf.file_name else {
            continue;
        };

        let candidate = super::fileio::resolve_filename_for_eval(eval, file_name);
        if candidate == resolved {
            return Ok(Value::Buffer(id));
        }
        if canonicalize_or_self(&candidate) == resolved_true {
            return Ok(Value::Buffer(id));
        }
    }

    Ok(Value::Nil)
}

/// (kill-buffer &optional BUFFER-OR-NAME) → t or nil
pub(crate) fn builtin_kill_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("kill-buffer", &args, 1)?;
    let id = match args.first() {
        None | Some(Value::Nil) => match eval.buffers.current_buffer() {
            Some(buf) => buf.id,
            None => return Ok(Value::Nil),
        },
        Some(Value::Buffer(id)) => {
            if eval.buffers.get(*id).is_none() {
                return Ok(Value::Nil);
            }
            *id
        }
        Some(Value::Str(name_id)) => {
            let name = with_heap(|h| h.get_string(*name_id).to_owned());
            match eval.buffers.find_buffer_by_name(&name) {
                Some(id) => id,
                None => {
                    return Err(signal(
                        "error",
                        vec![Value::string(format!("No buffer named {name}"))],
                    ));
                }
            }
        }
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    let was_current = eval.buffers.current_buffer().map(|buf| buf.id) == Some(id);
    let replacement = if was_current {
        match builtin_other_buffer(eval, vec![Value::Buffer(id)])? {
            Value::Buffer(next) if next != id => Some(next),
            _ => None,
        }
    } else {
        None
    };

    if !eval.buffers.kill_buffer(id) {
        return Ok(Value::Nil);
    }

    // Ensure dead-buffer windows continue to point at a live fallback buffer.
    let scratch = eval
        .buffers
        .find_buffer_by_name("*scratch*")
        .unwrap_or_else(|| eval.buffers.create_buffer("*scratch*"));
    eval.frames.replace_buffer_in_windows(id, scratch);

    if was_current {
        if let Some(next) = replacement {
            if eval.buffers.get(next).is_some() {
                eval.buffers.set_current(next);
            }
        }
        if eval.buffers.current_buffer().is_none() {
            if let Some(next) = eval.buffers.buffer_list().into_iter().next() {
                eval.buffers.set_current(next);
            } else {
                eval.buffers.set_current(scratch);
            }
        }
    }

    Ok(Value::True)
}

/// (set-buffer BUFFER-OR-NAME) → buffer
pub(crate) fn builtin_set_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-buffer", &args, 1)?;
    let id = match &args[0] {
        Value::Buffer(id) => {
            if eval.buffers.get(*id).is_none() {
                return Err(signal(
                    "error",
                    vec![Value::string("Selecting deleted buffer")],
                ));
            }
            *id
        }
        Value::Str(str_id) => {
            let s = with_heap(|h| h.get_string(*str_id).to_owned());
            eval.buffers.find_buffer_by_name(&s).ok_or_else(|| {
                signal("error", vec![Value::string(format!("No buffer named {s}"))])
            })?
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    eval.buffers.set_current(id);
    Ok(Value::Buffer(id))
}

/// (current-buffer) → buffer
pub(crate) fn builtin_current_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-buffer", &args, 0)?;
    match eval.buffers.current_buffer() {
        Some(buf) => Ok(Value::Buffer(buf.id)),
        None => Ok(Value::Nil),
    }
}

/// (buffer-name &optional BUFFER) → string
pub(crate) fn builtin_buffer_name(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-name", &args, 1)?;
    let id = if args.is_empty() || matches!(args[0], Value::Nil) {
        match eval.buffers.current_buffer() {
            Some(b) => b.id,
            None => return Ok(Value::Nil),
        }
    } else {
        expect_buffer_id(&args[0])?
    };
    match eval.buffers.get(id) {
        Some(buf) => Ok(Value::string(&buf.name)),
        None => Ok(Value::Nil),
    }
}

/// (buffer-file-name &optional BUFFER) → string or nil
pub(crate) fn builtin_buffer_file_name(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-file-name", &args, 1)?;
    let id = if args.is_empty() || matches!(args[0], Value::Nil) {
        match eval.buffers.current_buffer() {
            Some(b) => b.id,
            None => return Ok(Value::Nil),
        }
    } else {
        expect_buffer_id(&args[0])?
    };
    match eval.buffers.get(id) {
        Some(buf) => match &buf.file_name {
            Some(f) => Ok(Value::string(f)),
            None => Ok(Value::Nil),
        },
        None => Ok(Value::Nil),
    }
}

/// (buffer-base-buffer &optional BUFFER) → buffer or nil
pub(crate) fn builtin_buffer_base_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-base-buffer", &args, 1)?;
    let target = if args.is_empty() || matches!(args[0], Value::Nil) {
        match eval.buffers.current_buffer() {
            Some(buf) => buf.id,
            None => return Ok(Value::Nil),
        }
    } else {
        expect_buffer_id(&args[0])?
    };

    Ok(eval
        .buffers
        .get(target)
        .and_then(|buf| buf.base_buffer)
        .map(Value::Buffer)
        .unwrap_or(Value::Nil))
}

/// (buffer-last-name &optional BUFFER) → string or nil
pub(crate) fn builtin_buffer_last_name(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-last-name", &args, 1)?;
    let target = if args.is_empty() || matches!(args[0], Value::Nil) {
        match eval.buffers.current_buffer() {
            Some(buf) => buf.id,
            None => return Ok(Value::Nil),
        }
    } else {
        expect_buffer_id(&args[0])?
    };

    if let Some(buf) = eval.buffers.get(target) {
        if buf.name == "*scratch*" {
            return Ok(Value::Nil);
        }
        return Ok(Value::string(&buf.name));
    }
    if let Some(name) = eval.buffers.dead_buffer_last_name(target) {
        return Ok(Value::string(name));
    }
    Ok(Value::Nil)
}

/// (buffer-string) → string
pub(crate) fn builtin_buffer_string(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("buffer-string", &args, 0)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let byte_start = buf.point_min();
    let byte_end = buf.point_max();
    let result = Value::string(buf.buffer_string());
    // Copy buffer text properties to the result string
    if !buf.text_props.is_empty() {
        if let Value::Str(new_id) = &result {
            let sliced = buf.text_props.slice(byte_start, byte_end);
            if !sliced.is_empty() {
                set_string_text_properties_table(*new_id, sliced);
            }
        }
    }
    Ok(result)
}

/// (buffer-substring START END) → string
pub(crate) fn builtin_buffer_substring(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("buffer-substring", &args, 2)?;
    let start = expect_int(&args[0])?;
    let end = expect_int(&args[1])?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let point_min = buf.text.byte_to_char(buf.point_min()) as i64 + 1;
    let point_max = buf.text.byte_to_char(buf.point_max()) as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Buffer(buf.id), Value::Int(start), Value::Int(end)],
        ));
    }
    let start = start as usize;
    let end = end as usize;
    // Emacs uses 1-based positions, convert to 0-based byte positions
    let s = if start > 0 { start - 1 } else { 0 };
    let e = if end > 0 { end - 1 } else { 0 };
    // Convert char positions to byte positions
    let byte_start = buf.text.char_to_byte(s);
    let byte_end = buf.text.char_to_byte(e);
    let (byte_lo, byte_hi) = if byte_start <= byte_end {
        (byte_start, byte_end)
    } else {
        (byte_end, byte_start)
    };
    let result = Value::string(buf.buffer_substring(byte_lo, byte_hi));
    // Copy buffer text properties to the result string
    if !buf.text_props.is_empty() {
        if let Value::Str(new_id) = &result {
            let sliced = buf.text_props.slice(byte_lo, byte_hi);
            if !sliced.is_empty() {
                set_string_text_properties_table(*new_id, sliced);
            }
        }
    }
    Ok(result)
}

fn resolve_buffer_designator_allow_nil_current(
    eval: &mut super::eval::Evaluator,
    arg: &Value,
) -> Result<Option<BufferId>, Flow> {
    match arg {
        Value::Nil => eval
            .buffers
            .current_buffer()
            .map(|buf| Some(buf.id))
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")])),
        Value::Buffer(id) => Ok(eval.buffers.get(*id).map(|_| *id)),
        Value::Str(name_id) => {
            let name = with_heap(|h| h.get_string(*name_id).to_owned());
            eval.buffers
                .find_buffer_by_name(&name)
                .map(Some)
                .ok_or_else(|| {
                    signal(
                        "error",
                        vec![Value::string(format!("No buffer named {name}"))],
                    )
                })
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn buffer_slice_for_char_region(
    eval: &super::eval::Evaluator,
    buffer_id: Option<BufferId>,
    start: i64,
    end: i64,
) -> String {
    let Some(buffer_id) = buffer_id else {
        return String::new();
    };
    let Some(buf) = eval.buffers.get(buffer_id) else {
        return String::new();
    };

    let (from, to) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let from_char = if from > 0 { from as usize - 1 } else { 0 };
    let to_char = if to > 0 { to as usize - 1 } else { 0 };
    let char_count = buf.text.char_count();
    let from_byte = buf.text.char_to_byte(from_char.min(char_count));
    let to_byte = buf.text.char_to_byte(to_char.min(char_count));
    buf.buffer_substring(from_byte, to_byte)
}

fn compare_buffer_substring_strings(left: &str, right: &str) -> i64 {
    let mut pos = 1i64;
    let mut left_iter = left.chars();
    let mut right_iter = right.chars();

    loop {
        match (left_iter.next(), right_iter.next()) {
            (Some(a), Some(b)) => {
                if a != b {
                    return if a < b { -pos } else { pos };
                }
                pos += 1;
            }
            (Some(_), None) => return pos,
            (None, Some(_)) => return -pos,
            (None, None) => return 0,
        }
    }
}

/// `(buffer-line-statistics &optional BUFFER-OR-NAME)` -> (LINES MAX-LEN AVG-LEN)
pub(crate) fn builtin_buffer_line_statistics(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-line-statistics", &args, 1)?;
    let buffer_id = if args.is_empty() {
        resolve_buffer_designator_allow_nil_current(eval, &Value::Nil)?
    } else {
        resolve_buffer_designator_allow_nil_current(eval, &args[0])?
    };

    let text = buffer_id
        .and_then(|id| eval.buffers.get(id).map(|buf| buf.buffer_string()))
        .unwrap_or_default();

    if text.is_empty() {
        return Ok(Value::list(vec![
            Value::Int(0),
            Value::Int(0),
            Value::Float(0.0, next_float_id()),
        ]));
    }

    let mut line_count = 0usize;
    let mut max_len = 0usize;
    let mut total_len = 0usize;
    for line in text.lines() {
        line_count += 1;
        let width = line.chars().count();
        max_len = max_len.max(width);
        total_len += width;
    }

    if line_count == 0 {
        return Ok(Value::list(vec![
            Value::Int(0),
            Value::Int(0),
            Value::Float(0.0, next_float_id()),
        ]));
    }

    Ok(Value::list(vec![
        Value::Int(line_count as i64),
        Value::Int(max_len as i64),
        Value::Float(total_len as f64 / line_count as f64, next_float_id()),
    ]))
}

fn replace_region_contents_type_predicate() -> Value {
    Value::list(vec![
        Value::symbol("or"),
        Value::symbol("stringp"),
        Value::symbol("bufferp"),
        Value::symbol("vectorp"),
    ])
}

fn replace_region_source_text(
    eval: &super::eval::Evaluator,
    source: &Value,
) -> Result<String, Flow> {
    match source {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        Value::Buffer(id) => Ok(eval
            .buffers
            .get(*id)
            .map(|buf| buf.buffer_string())
            .unwrap_or_default()),
        Value::Vector(id) => {
            let items = with_heap(|h| h.get_vector(*id).clone());
            if items.len() != 3 {
                return Err(signal(
                    "wrong-type-argument",
                    vec![replace_region_contents_type_predicate(), *source],
                ));
            }
            let buffer_id = expect_buffer_id(&items[0])?;
            let start = expect_integer_or_marker(&items[1])?;
            let end = expect_integer_or_marker(&items[2])?;
            Ok(buffer_slice_for_char_region(
                eval,
                Some(buffer_id),
                start,
                end,
            ))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![replace_region_contents_type_predicate(), *other],
        )),
    }
}

/// `(buffer-swap-text OTHER-BUFFER)` -> nil
pub(crate) fn builtin_buffer_swap_text(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("buffer-swap-text", &args, 1)?;
    let other_id = expect_buffer_id(&args[0])?;
    if eval.buffers.get(other_id).is_none() {
        return Ok(Value::Nil);
    }

    let current_id = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .id;

    if current_id == other_id {
        return Ok(Value::Nil);
    }

    let current_text = eval
        .buffers
        .get(current_id)
        .map(|buf| buf.buffer_string())
        .unwrap_or_default();
    let other_text = eval
        .buffers
        .get(other_id)
        .map(|buf| buf.buffer_string())
        .unwrap_or_default();

    let _ = eval
        .buffers
        .replace_buffer_contents(current_id, &other_text);
    let _ = eval
        .buffers
        .replace_buffer_contents(other_id, &current_text);

    Ok(Value::Nil)
}

/// `(insert-and-inherit &rest ARGS)` -> nil
///
/// Insert text and inherit text properties from the character immediately
/// before the insertion point.  Properties listed in `rear-nonsticky` at
/// that position are NOT inherited.
pub(crate) fn builtin_insert_and_inherit(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    use super::value::list_to_vec;

    let text = super::editfns::collect_insert_text("insert-and-inherit", &args)?;
    super::editfns::ensure_current_buffer_writable(eval)?;

    let current_id = match eval.buffers.current_buffer_id() {
        Some(id) => id,
        None => return Ok(Value::Nil),
    };
    let old_pt = eval.buffers.get(current_id).map(|buf| buf.pt).unwrap_or(0);
    let _ = eval.buffers.insert_into_buffer(current_id, &text);
    let text_len = text.len();

    if text_len > 0 && old_pt > 0 {
        let props = eval
            .buffers
            .get(current_id)
            .map(|buf| buf.text_props.get_properties(old_pt - 1))
            .unwrap_or_default();

        if !props.is_empty() {
            let nonsticky = props.get("rear-nonsticky").copied();
            let inherit_all = match nonsticky {
                None => true,
                Some(Value::Nil) => true,
                Some(val) if val.is_truthy() && list_to_vec(&val).is_none() => false,
                _ => true,
            };

            if inherit_all || nonsticky.is_some() {
                let nonsticky_names: Vec<String> = match nonsticky {
                    Some(ref val) => {
                        if let Some(items) = list_to_vec(val) {
                            items
                                .iter()
                                .filter_map(|v| v.as_symbol_name().map(|s| s.to_string()))
                                .collect()
                        } else {
                            Vec::new()
                        }
                    }
                    None => Vec::new(),
                };

                for (name, value) in &props {
                    if name == "rear-nonsticky" || !inherit_all || nonsticky_names.contains(name) {
                        continue;
                    }
                    let _ = eval.buffers.put_buffer_text_property(
                        current_id,
                        old_pt,
                        old_pt + text_len,
                        name,
                        *value,
                    );
                }
            }
        }
    }
    Ok(Value::Nil)
}

/// `(insert-before-markers-and-inherit &rest ARGS)` -> nil
///
/// Text property inheritance is currently equivalent to plain insertion.
pub(crate) fn builtin_insert_before_markers_and_inherit(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    super::editfns::builtin_insert_before_markers(eval, args)
}

/// `(insert-buffer-substring BUFFER &optional START END)` -> nil
pub(crate) fn builtin_insert_buffer_substring(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("insert-buffer-substring", &args, 1, 3)?;
    let buffer_id = resolve_buffer_designator_allow_nil_current(eval, &args[0])?;
    let default_end = buffer_id
        .and_then(|id| {
            eval.buffers
                .get(id)
                .map(|buf| buf.text.char_count() as i64 + 1)
        })
        .unwrap_or(1);
    let start = if args.len() > 1 && !args[1].is_nil() {
        expect_integer_or_marker(&args[1])?
    } else {
        1
    };
    let end = if args.len() > 2 && !args[2].is_nil() {
        expect_integer_or_marker(&args[2])?
    } else {
        default_end
    };

    let text = buffer_slice_for_char_region(eval, buffer_id, start, end);
    builtin_insert(eval, vec![Value::string(text)])
}

/// `(kill-all-local-variables &optional KILL-PERMANENT)` -> nil
pub(crate) fn builtin_kill_all_local_variables(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("kill-all-local-variables", &args, 0, 1)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.clear_buffer_local_properties(current_id);
    Ok(Value::Nil)
}

/// `(ntake N LIST)` -> LIST
pub(crate) fn builtin_ntake(args: Vec<Value>) -> EvalResult {
    expect_args("ntake", &args, 2)?;
    let n = expect_int(&args[0])?;
    if n <= 0 {
        return Ok(Value::Nil);
    }

    let head = args[1];
    if matches!(head, Value::Nil) {
        return Ok(Value::Nil);
    }
    if !matches!(head, Value::Cons(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), head],
        ));
    }

    let mut cursor = head;
    for _ in 1..n {
        match cursor {
            Value::Cons(cell) => {
                let next = with_heap(|h| h.cons_cdr(cell));
                match next {
                    Value::Cons(_) => cursor = next,
                    Value::Nil => return Ok(head),
                    other => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), other],
                        ));
                    }
                }
            }
            Value::Nil => return Ok(head),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), other],
                ));
            }
        }
    }

    match cursor {
        Value::Cons(cell) => {
            with_heap_mut(|h| h.set_cdr(cell, Value::Nil));
            Ok(head)
        }
        Value::Nil => Ok(head),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), other],
        )),
    }
}

/// `(replace-buffer-contents SOURCE &optional MAX-SECS MAX-COSTS)` -> t
pub(crate) fn builtin_replace_buffer_contents_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("replace-buffer-contents", &args, 1, 3)?;
    let source_id = resolve_buffer_designator_allow_nil_current(eval, &args[0])?;
    let source_text = source_id
        .and_then(|id| eval.buffers.get(id).map(|buf| buf.buffer_string()))
        .unwrap_or_default();

    let read_only_buffer_name = eval.buffers.current_buffer().and_then(|buf| {
        if buffer_read_only_active(eval, buf) {
            Some(buf.name.clone())
        } else {
            None
        }
    });
    if let Some(name) = read_only_buffer_name {
        return Err(signal("buffer-read-only", vec![Value::string(name)]));
    }

    let current_id = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
        .id;

    let _ = eval
        .buffers
        .replace_buffer_contents(current_id, &source_text);

    Ok(Value::True)
}

/// `(replace-region-contents BEG END SOURCE &optional MAX-SECS MAX-COSTS INHERIT)` -> t
pub(crate) fn builtin_replace_region_contents_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("replace-region-contents", &args, 3, 6)?;
    let start = expect_integer_or_marker(&args[0])?;
    let end = expect_integer_or_marker(&args[1])?;
    let source_text = replace_region_source_text(eval, &args[2])?;

    let read_only_buffer_name = eval.buffers.current_buffer().and_then(|buf| {
        if buffer_read_only_active(eval, buf) {
            Some(buf.name.clone())
        } else {
            None
        }
    });
    if let Some(name) = read_only_buffer_name {
        return Err(signal("buffer-read-only", vec![Value::string(name)]));
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let start_byte = super::editfns::lisp_pos_to_byte(buf, start);
    let end_byte = super::editfns::lisp_pos_to_byte(buf, end);
    let (lo, hi) = if start_byte <= end_byte {
        (start_byte, end_byte)
    } else {
        (end_byte, start_byte)
    };
    let _ = eval.buffers.delete_buffer_region(current_id, lo, hi);
    let _ = eval.buffers.goto_buffer_byte(current_id, lo);
    if !source_text.is_empty() {
        let _ = eval.buffers.insert_into_buffer(current_id, &source_text);
    }

    Ok(Value::True)
}

/// `(set-buffer-multibyte FLAG)` -> FLAG
pub(crate) fn builtin_set_buffer_multibyte_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-buffer-multibyte", &args, 1)?;
    let flag = args[0].is_truthy();
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.set_buffer_multibyte_flag(current_id, flag);
    Ok(args[0])
}

/// `(split-window-internal WINDOW SIZE SIDE NORMALIZE)` -> window
pub(crate) fn builtin_split_window_internal(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("split-window-internal", &args, 4, 5)?;
    if !args[0].is_nil() {
        let windowp = super::window_cmds::builtin_windowp(eval, vec![args[0]])?;
        if windowp.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("windowp"), args[0]],
            ));
        }
    }
    if !args[1].is_nil() {
        let _ = expect_fixnum(&args[1])?;
    }
    if !args[2].is_nil() && !args[2].is_symbol() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[2]],
        ));
    }

    // NORMALIZE and REFER are accepted for arity compatibility and ignored in this subset.
    let _ = &args[3];
    if let Some(refer) = args.get(4) {
        let _ = refer;
    }
    super::window_cmds::split_window_internal_impl(eval, args[0], args[2])
}

/// `(buffer-text-pixel-size &optional BUFFER WINDOW X-LIMIT Y-LIMIT)` -> (WIDTH . HEIGHT)
pub(crate) fn builtin_buffer_text_pixel_size(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("buffer-text-pixel-size", &args, 0, 4)?;

    let buffer_id = if args.is_empty() {
        resolve_buffer_designator_allow_nil_current(eval, &Value::Nil)?
    } else {
        resolve_buffer_designator_allow_nil_current(eval, &args[0])?
    };

    if args.len() > 1 {
        let window = &args[1];
        if !window.is_nil() && !matches!(window, Value::Window(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *window],
            ));
        }
    }

    let limit_from_value = |value: &Value| -> Result<Option<usize>, Flow> {
        match value {
            Value::Nil | Value::True => Ok(None),
            Value::Int(n) if *n >= 0 => Ok(Some(*n as usize)),
            _ => Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("natnump"), *value],
            )),
        }
    };

    let x_limit = if args.len() > 2 {
        limit_from_value(&args[2])?
    } else {
        None
    };
    let y_limit = if args.len() > 3 {
        limit_from_value(&args[3])?
    } else {
        None
    };

    let text = if let Some(id) = buffer_id {
        if let Some(buf) = eval.buffers.get(id) {
            let default_from = 1i64;
            let default_to = buf.text.char_count() as i64 + 1;
            buffer_slice_for_char_region(eval, Some(id), default_from, default_to)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    if text.is_empty() {
        return Ok(Value::cons(Value::Int(0), Value::Int(0)));
    }

    let mut height = 0usize;
    let mut width = 0usize;
    for line in text.lines() {
        if y_limit.is_some_and(|limit| height >= limit) {
            break;
        }

        let mut line_width = 0usize;
        for ch in line.chars() {
            if ch == '\t' {
                let tab_width = 8usize;
                line_width += tab_width - (line_width % tab_width);
            } else {
                line_width += crate::encoding::char_width(ch);
            }

            if let Some(limit) = x_limit {
                if line_width >= limit {
                    line_width = limit;
                    break;
                }
            }
        }

        height += 1;
        width = width.max(line_width);
    }

    if height == 0 {
        return Ok(Value::cons(Value::Int(0), Value::Int(0)));
    }
    Ok(Value::cons(
        Value::Int(width as i64),
        Value::Int(height as i64),
    ))
}

/// `(compare-buffer-substrings BUF1 START1 END1 BUF2 START2 END2)` -> integer
pub(crate) fn builtin_compare_buffer_substrings(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("compare-buffer-substrings", &args, 6)?;

    let left_buffer = resolve_buffer_designator_allow_nil_current(eval, &args[0])?;
    let left_start = expect_integer_or_marker(&args[1])?;
    let left_end = expect_integer_or_marker(&args[2])?;
    let right_buffer = resolve_buffer_designator_allow_nil_current(eval, &args[3])?;
    let right_start = expect_integer_or_marker(&args[4])?;
    let right_end = expect_integer_or_marker(&args[5])?;

    let left = buffer_slice_for_char_region(eval, left_buffer, left_start, left_end);
    let right = buffer_slice_for_char_region(eval, right_buffer, right_start, right_end);
    Ok(Value::Int(compare_buffer_substring_strings(&left, &right)))
}

/// `(compute-motion FROM FROMPOS TO TOPOS WIDTH OFFSETS WINDOW)` -> motion tuple
pub(crate) fn builtin_compute_motion(args: Vec<Value>) -> EvalResult {
    expect_args("compute-motion", &args, 7)?;

    let from = expect_integer_or_marker(&args[0])?;
    if !matches!(&args[1], Value::Cons(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), args[1]],
        ));
    }
    let to = expect_integer_or_marker(&args[2])?;
    if !args[3].is_nil() && !matches!(&args[3], Value::Cons(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), args[3]],
        ));
    }
    if !args[4].is_nil() {
        let _ = expect_fixnum(&args[4])?;
    }
    if !args[5].is_nil() && !matches!(&args[5], Value::Cons(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("consp"), args[5]],
        ));
    }
    if !args[6].is_nil() && !matches!(&args[6], Value::Window(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-live-p"), args[6]],
        ));
    }

    let result = if args[3].is_nil() {
        vec![
            Value::Int(to),
            Value::Int(1),
            Value::Int(0),
            Value::Int(1),
            Value::Nil,
        ]
    } else {
        vec![
            Value::Int(from),
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
            Value::Nil,
        ]
    };
    Ok(Value::list(result))
}

/// `(coordinates-in-window-p COORDINATES WINDOW)` -> COORDINATES or nil.
///
/// Batch compatibility: returns the coordinate pair when it's inside WINDOW's
/// current character bounds, otherwise nil.
pub(crate) fn builtin_coordinates_in_window_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("coordinates-in-window-p", &args, 2)?;

    let (x, y) = match &args[0] {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            let x = match &pair.car {
                Value::Int(n) => *n as f64,
                Value::Float(f, _) => *f,
                other => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("numberp"), *other],
                    ));
                }
            };
            let y = match &pair.cdr {
                Value::Int(n) => *n as f64,
                Value::Float(f, _) => *f,
                other => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("numberp"), *other],
                    ));
                }
            };
            (x, y)
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("consp"), *other],
            ));
        }
    };

    expect_optional_live_window_designator(&args[1], eval)?;
    let window_arg = args[1];
    let width = match super::window_cmds::builtin_window_total_width(eval, vec![window_arg])? {
        Value::Int(n) => n as f64,
        _ => 0.0,
    };
    let height = match super::window_cmds::builtin_window_total_height(eval, vec![window_arg])? {
        Value::Int(n) => n as f64,
        _ => 0.0,
    };

    if x >= 0.0 && y >= 0.0 && x < width && y < height {
        Ok(args[0])
    } else {
        Ok(Value::Nil)
    }
}

/// `(constrain-to-field NEW-POS OLD-POS &optional ESCAPE-FROM-EDGE ONLY-IN-LINE INHIBIT-CAPTURE-PROPERTY)`
pub(crate) fn builtin_constrain_to_field(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("constrain-to-field", &args, 2, 5)?;
    let new_pos = if args[0].is_nil() {
        let current = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        current.point_char() as i64 + 1
    } else {
        expect_integer_or_marker(&args[0])?
    };
    let _ = expect_integer_or_marker(&args[1])?;
    Ok(Value::Int(new_pos))
}

fn resolve_field_position(
    eval: &super::eval::Evaluator,
    position_value: Option<&Value>,
) -> Result<(i64, i64, i64), Flow> {
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let point_min = buf.text.byte_to_char(buf.point_min()) as i64 + 1;
    let point_max = buf.text.byte_to_char(buf.point_max()) as i64 + 1;
    let pos = match position_value {
        None | Some(Value::Nil) => buf.text.byte_to_char(buf.pt) as i64 + 1,
        Some(value) => expect_integer_or_marker(value)?,
    };
    if pos < point_min || pos > point_max {
        return Err(signal("args-out-of-range", vec![Value::Int(pos)]));
    }
    Ok((pos, point_min, point_max))
}

/// `(field-beginning &optional POS ESCAPE-FROM-EDGE LIMIT)` -> position
pub(crate) fn builtin_field_beginning(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("field-beginning", &args, 3)?;
    let (_pos, point_min, _point_max) = resolve_field_position(eval, args.first())?;
    if let Some(limit_value) = args.get(2) {
        if !limit_value.is_nil() {
            let limit = expect_integer_or_marker(limit_value)?;
            if limit <= 0 {
                return Err(signal("args-out-of-range", vec![Value::Int(limit)]));
            }
            return Ok(Value::Int(point_min.max(limit)));
        }
    }
    Ok(Value::Int(point_min))
}

/// `(field-end &optional POS ESCAPE-FROM-EDGE LIMIT)` -> position
pub(crate) fn builtin_field_end(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_max_args("field-end", &args, 3)?;
    let (_pos, _point_min, point_max) = resolve_field_position(eval, args.first())?;
    if let Some(limit_value) = args.get(2) {
        if !limit_value.is_nil() {
            let limit = expect_integer_or_marker(limit_value)?;
            return Ok(Value::Int(point_max.min(limit)));
        }
    }
    Ok(Value::Int(point_max))
}

/// `(field-string &optional POS)` -> field text at POS.
pub(crate) fn builtin_field_string(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("field-string", &args, 1)?;
    let (_pos, point_min, point_max) = resolve_field_position(eval, args.first())?;
    builtin_buffer_substring(eval, vec![Value::Int(point_min), Value::Int(point_max)])
}

/// `(field-string-no-properties &optional POS)` -> field text at POS.
pub(crate) fn builtin_field_string_no_properties(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("field-string-no-properties", &args, 1)?;
    let (_pos, point_min, point_max) = resolve_field_position(eval, args.first())?;
    super::editfns::builtin_buffer_substring_no_properties(
        eval,
        vec![Value::Int(point_min), Value::Int(point_max)],
    )
}

/// `(delete-field &optional POS)` -> nil
pub(crate) fn builtin_delete_field(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("delete-field", &args, 1)?;
    let (_pos, point_min, point_max) = resolve_field_position(eval, args.first())?;
    builtin_delete_region(eval, vec![Value::Int(point_min), Value::Int(point_max)])
}

/// `(clear-string STRING)` -> nil
/// Zeroes out every byte in STRING (fills with null characters).
pub(crate) fn builtin_clear_string(args: Vec<Value>) -> EvalResult {
    expect_args("clear-string", &args, 1)?;
    let _ = expect_strict_string(&args[0])?;
    if let Value::Str(id) = &args[0] {
        with_heap_mut(|h| {
            let s = h.get_string_mut(*id);
            let len = s.len();
            s.clear();
            // Fill with len null bytes (same as GNU Emacs memset 0)
            for _ in 0..len {
                s.push('\0');
            }
        });
    }
    Ok(Value::Nil)
}

/// `(command-error-default-function DATA CONTEXT CALLER)` -> nil
pub(crate) fn builtin_command_error_default_function(args: Vec<Value>) -> EvalResult {
    expect_args("command-error-default-function", &args, 3)?;
    Ok(Value::Nil)
}

/// (point) → integer
pub(crate) fn builtin_point(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("point", &args, 0)?;
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    // Return 1-based char position
    Ok(Value::Int(buf.point_char() as i64 + 1))
}

/// (point-min) → integer
pub(crate) fn builtin_point_min(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("point-min", &args, 0)?;
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::Int(
        buf.text.byte_to_char(buf.point_min()) as i64 + 1,
    ))
}

/// (point-max) → integer
pub(crate) fn builtin_point_max(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("point-max", &args, 0)?;
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    Ok(Value::Int(
        buf.text.byte_to_char(buf.point_max()) as i64 + 1,
    ))
}

/// (goto-char POS) → POS
pub(crate) fn builtin_goto_char(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("goto-char", &args, 1)?;
    let pos = expect_integer_or_marker(&args[0])?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    // Convert 1-based char pos to 0-based byte pos
    let char_pos = if pos > 0 { pos as usize - 1 } else { 0 };
    let byte_pos = buf.text.char_to_byte(char_pos.min(buf.text.char_count()));
    let _ = eval.buffers.goto_buffer_byte(current_id, byte_pos);
    Ok(args[0])
}

/// (insert &rest ARGS) → nil
pub(crate) fn builtin_insert(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    let read_only_buffer_name = eval.buffers.current_buffer().and_then(|buf| {
        if buffer_read_only_active(eval, buf) {
            Some(buf.name.clone())
        } else {
            None
        }
    });
    if let Some(name) = read_only_buffer_name {
        return Err(signal("buffer-read-only", vec![Value::string(name)]));
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    for arg in &args {
        match arg {
            Value::Str(id) => {
                let s = with_heap(|h| h.get_string(*id).to_owned());
                let insert_pos = eval.buffers.get(current_id).map(|buf| buf.pt).unwrap_or(0);
                let _ = eval.buffers.insert_into_buffer(current_id, &s);
                // Transfer string text properties to buffer
                if let Some(str_table) = get_string_text_properties_table(*id) {
                    let _ = eval
                        .buffers
                        .append_buffer_text_properties(current_id, &str_table, insert_pos);
                }
            }
            Value::Char(c) => {
                let mut tmp = [0u8; 4];
                let _ = eval
                    .buffers
                    .insert_into_buffer(current_id, c.encode_utf8(&mut tmp));
            }
            Value::Int(n) => {
                if !(0..=KEY_CHAR_CODE_MASK).contains(n) {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("char-or-string-p"), Value::Int(*n)],
                    ));
                }
                if let Some(c) = char::from_u32(*n as u32) {
                    let mut tmp = [0u8; 4];
                    let _ = eval
                        .buffers
                        .insert_into_buffer(current_id, c.encode_utf8(&mut tmp));
                } else if let Some(encoded) = encode_nonunicode_char_for_storage(*n as u32) {
                    let _ = eval.buffers.insert_into_buffer(current_id, &encoded);
                } else {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("char-or-string-p"), Value::Int(*n)],
                    ));
                }
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("char-or-string-p"), *other],
                ));
            }
        }
    }
    Ok(Value::Nil)
}

pub(super) fn insert_char_code_from_value(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Char(c) => Ok(*c as i64),
        Value::Int(n) if *n < 0 || *n > KEY_CHAR_CODE_MASK => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *value],
        )),
        Value::Int(n) => Ok(*n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *other],
        )),
    }
}

/// `(insert-char CHARACTER &optional COUNT INHERIT)` -> nil
pub(crate) fn builtin_insert_char(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("insert-char", &args, 1, 3)?;
    let char_code = insert_char_code_from_value(&args[0])?;
    let count = if args.len() > 1 {
        expect_fixnum(&args[1])?
    } else {
        1
    };

    if count <= 0 {
        return Ok(Value::Nil);
    }

    let read_only_buffer_name = eval.buffers.current_buffer().and_then(|buf| {
        if buffer_read_only_active(eval, buf) {
            Some(buf.name.clone())
        } else {
            None
        }
    });
    if let Some(name) = read_only_buffer_name {
        return Err(signal("buffer-read-only", vec![Value::string(name)]));
    }

    let to_insert = if let Some(ch) = char::from_u32(char_code as u32) {
        ch.to_string().repeat(count as usize)
    } else if let Some(encoded) = encode_nonunicode_char_for_storage(char_code as u32) {
        encoded.repeat(count as usize)
    } else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), args[0]],
        ));
    };
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.insert_into_buffer(current_id, &to_insert);
    Ok(Value::Nil)
}

/// `(insert-byte BYTE COUNT &optional INHERIT)` -> nil
pub(crate) fn builtin_insert_byte(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("insert-byte", &args, 2, 3)?;
    let byte = expect_fixnum(&args[0])?;
    if !(0..=255).contains(&byte) {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Int(byte), Value::Int(0), Value::Int(255)],
        ));
    }
    let count = expect_fixnum(&args[1])?;
    if count <= 0 {
        return Ok(Value::Nil);
    }

    let read_only_buffer_name = eval.buffers.current_buffer().and_then(|buf| {
        if buffer_read_only_active(eval, buf) {
            Some(buf.name.clone())
        } else {
            None
        }
    });
    if let Some(name) = read_only_buffer_name {
        return Err(signal("buffer-read-only", vec![Value::string(name)]));
    }

    let ch = char::from_u32(byte as u32).expect("byte range maps to a valid codepoint");
    let to_insert = ch.to_string().repeat(count as usize);
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.insert_into_buffer(current_id, &to_insert);
    Ok(Value::Nil)
}

/// (delete-region START END) → nil
pub(crate) fn builtin_delete_region(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("delete-region", &args, 2)?;
    let start = expect_int(&args[0])?;
    let end = expect_int(&args[1])?;
    let read_only_buffer_name = eval.buffers.current_buffer().and_then(|buf| {
        if buffer_read_only_active(eval, buf) {
            Some(buf.name.clone())
        } else {
            None
        }
    });
    if let Some(name) = read_only_buffer_name {
        return Err(signal("buffer-read-only", vec![Value::string(name)]));
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let point_min = buf.text.byte_to_char(buf.point_min()) as i64 + 1;
    let point_max = buf.text.byte_to_char(buf.point_max()) as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Buffer(buf.id), Value::Int(start), Value::Int(end)],
        ));
    }
    let start = start as usize;
    let end = end as usize;
    // Convert 1-based to 0-based char positions, then to byte positions
    let s = if start > 0 { start - 1 } else { 0 };
    let e = if end > 0 { end - 1 } else { 0 };
    let byte_start = buf.text.char_to_byte(s);
    let byte_end = buf.text.char_to_byte(e);
    let _ = eval
        .buffers
        .delete_buffer_region(current_id, byte_start, byte_end);
    Ok(Value::Nil)
}

/// `(delete-and-extract-region START END)` -> deleted text
pub(crate) fn builtin_delete_and_extract_region(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("delete-and-extract-region", &args, 2)?;
    let start = expect_integer_or_marker(&args[0])?;
    let end = expect_integer_or_marker(&args[1])?;

    let (point_min, point_max, current_buffer) = {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        (
            buf.text.byte_to_char(buf.point_min()) as i64 + 1,
            buf.text.byte_to_char(buf.point_max()) as i64 + 1,
            Value::Buffer(buf.id),
        )
    };

    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![current_buffer, Value::Int(start), Value::Int(end)],
        ));
    }

    let lo = start.min(end);
    let hi = start.max(end);
    let deleted = builtin_buffer_substring(eval, vec![Value::Int(lo), Value::Int(hi)])?;
    let _ = builtin_delete_region(eval, vec![Value::Int(lo), Value::Int(hi)])?;
    Ok(deleted)
}

/// (erase-buffer) → nil
pub(crate) fn builtin_erase_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("erase-buffer", &args, 0)?;
    let read_only_buffer_name = eval.buffers.current_buffer().and_then(|buf| {
        if buffer_read_only_active(eval, buf) {
            Some(buf.name.clone())
        } else {
            None
        }
    });
    if let Some(name) = read_only_buffer_name {
        return Err(signal("buffer-read-only", vec![Value::string(name)]));
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.replace_buffer_contents(current_id, "");
    Ok(Value::Nil)
}

/// (buffer-enable-undo &optional BUFFER) -> nil
pub(crate) fn builtin_buffer_enable_undo(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("buffer-enable-undo"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let id = if args.is_empty() || matches!(args[0], Value::Nil) {
        eval.buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
            .id
    } else {
        match &args[0] {
            Value::Buffer(id) => {
                if eval.buffers.get(*id).is_none() {
                    return Ok(Value::Nil);
                }
                *id
            }
            Value::Str(name_id) => {
                let name = with_heap(|h| h.get_string(*name_id).to_owned());
                eval.buffers.find_buffer_by_name(&name).ok_or_else(|| {
                    signal(
                        "error",
                        vec![Value::string(format!("No buffer named {name}"))],
                    )
                })?
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *other],
                ));
            }
        }
    };
    let buf = eval
        .buffers
        .get_mut(id)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    buf.undo_list.set_enabled(true);
    buf.set_buffer_local("buffer-undo-list", Value::Nil);
    Ok(Value::Nil)
}

/// (buffer-disable-undo &optional BUFFER) -> t
pub(crate) fn builtin_buffer_disable_undo(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("buffer-disable-undo"),
                Value::Int(args.len() as i64),
            ],
        ));
    }

    let id = if args.is_empty() || matches!(args[0], Value::Nil) {
        eval.buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?
            .id
    } else {
        match &args[0] {
            Value::Buffer(id) => {
                if eval.buffers.get(*id).is_none() {
                    return Err(signal(
                        "error",
                        vec![Value::string("Selecting deleted buffer")],
                    ));
                }
                *id
            }
            Value::Str(name_id) => {
                let name = with_heap(|h| h.get_string(*name_id).to_owned());
                match eval.buffers.find_buffer_by_name(&name) {
                    Some(id) => id,
                    None => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("stringp"), Value::Nil],
                        ));
                    }
                }
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *other],
                ));
            }
        }
    };
    let buf = eval
        .buffers
        .get_mut(id)
        .ok_or_else(|| signal("error", vec![Value::string("Selecting deleted buffer")]))?;
    buf.undo_list.set_enabled(false);
    buf.set_buffer_local("buffer-undo-list", Value::True);
    Ok(Value::True)
}

/// (buffer-size &optional BUFFER) → integer
pub(crate) fn builtin_buffer_size(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-size", &args, 1)?;
    if args.is_empty() || matches!(args[0], Value::Nil) {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        return Ok(Value::Int(buf.text.char_count() as i64));
    }

    let id = expect_buffer_id(&args[0])?;
    if let Some(buf) = eval.buffers.get(id) {
        Ok(Value::Int(buf.text.char_count() as i64))
    } else {
        Ok(Value::Int(0))
    }
}

/// (narrow-to-region START END) → nil
pub(crate) fn builtin_narrow_to_region(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("narrow-to-region", &args, 2)?;
    let start = expect_int(&args[0])?;
    let end = expect_int(&args[1])?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let point_min = buf.text.byte_to_char(buf.point_min()) as i64 + 1;
    let point_max = buf.text.byte_to_char(buf.point_max()) as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Int(start), Value::Int(end)],
        ));
    }
    let start = start as usize;
    let end = end as usize;
    let s = if start > 0 { start - 1 } else { 0 };
    let e = if end > 0 { end - 1 } else { 0 };
    let byte_start = buf.text.char_to_byte(s);
    let byte_end = buf.text.char_to_byte(e);
    let _ = eval
        .buffers
        .narrow_buffer_to_region(current_id, byte_start, byte_end);
    Ok(Value::Nil)
}

/// (widen) → nil
pub(crate) fn builtin_widen(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("widen", &args, 0)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.widen_buffer(current_id);
    Ok(Value::Nil)
}

/// (buffer-modified-p &optional BUFFER) → t or nil
pub(crate) fn builtin_buffer_modified_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-modified-p", &args, 1)?;
    if args.is_empty() || matches!(args[0], Value::Nil) {
        let buf = eval
            .buffers
            .current_buffer()
            .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
        return Ok(Value::bool(buf.is_modified()));
    }

    let id = expect_buffer_id(&args[0])?;
    if let Some(buf) = eval.buffers.get(id) {
        Ok(Value::bool(buf.is_modified()))
    } else {
        Ok(Value::Nil)
    }
}

/// (set-buffer-modified-p FLAG) → FLAG
pub(crate) fn builtin_set_buffer_modified_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-buffer-modified-p", &args, 1)?;
    let flag = args[0].is_truthy();
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.set_buffer_modified_flag(current_id, flag);
    Ok(args[0])
}

fn optional_buffer_tick_target(
    eval: &super::eval::Evaluator,
    name: &str,
    args: &[Value],
) -> Result<Option<BufferId>, Flow> {
    expect_max_args(name, args, 1)?;
    if args.is_empty() || matches!(args[0], Value::Nil) {
        Ok(eval.buffers.current_buffer().map(|buf| buf.id))
    } else {
        Ok(Some(expect_buffer_id(&args[0])?))
    }
}

/// (buffer-modified-tick &optional BUFFER) → integer
pub(crate) fn builtin_buffer_modified_tick(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let target = optional_buffer_tick_target(eval, "buffer-modified-tick", &args)?;
    if let Some(id) = target {
        if let Some(buf) = eval.buffers.get(id) {
            return Ok(Value::Int(buf.modified_tick));
        }
    }
    Ok(Value::Int(1))
}

/// (buffer-chars-modified-tick &optional BUFFER) → integer
pub(crate) fn builtin_buffer_chars_modified_tick(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let target = optional_buffer_tick_target(eval, "buffer-chars-modified-tick", &args)?;
    if let Some(id) = target {
        if let Some(buf) = eval.buffers.get(id) {
            return Ok(Value::Int(buf.chars_modified_tick));
        }
    }
    Ok(Value::Int(1))
}

/// (buffer-list) → list of buffers
pub(crate) fn builtin_buffer_list(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("buffer-list", &args, 1)?;
    let ids = eval.buffers.buffer_list();
    let vals: Vec<Value> = ids.into_iter().map(Value::Buffer).collect();
    Ok(Value::list(vals))
}

/// (other-buffer &optional BUFFER VISIBLE-OK FRAME) → buffer
///
/// Batch-friendly behavior:
/// - prefers `*Messages*` when available and distinct from BUFFER
/// - otherwise returns a live buffer distinct from BUFFER when possible
/// - falls back to BUFFER/current buffer when no alternative exists
pub(crate) fn builtin_other_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("other-buffer", &args, 3)?;

    let current_id = eval.buffers.current_buffer().map(|buf| buf.id);
    let avoid_id = match args.first() {
        None | Some(Value::Nil) => current_id,
        Some(Value::Buffer(id)) => Some(*id),
        Some(Value::Str(name_id)) => {
            let name = with_heap(|h| h.get_string(*name_id).to_owned());
            eval.buffers.find_buffer_by_name(&name)
        }
        // GNU Emacs is permissive for non-buffer designators here; treat as
        // unspecified and still return a live buffer.
        Some(_) => current_id,
    };

    if let Some(messages_id) = eval.buffers.find_buffer_by_name("*Messages*") {
        if Some(messages_id) != avoid_id {
            return Ok(Value::Buffer(messages_id));
        }
    }

    if let Some(id) = eval
        .buffers
        .buffer_list()
        .into_iter()
        .find(|id| Some(*id) != avoid_id)
    {
        return Ok(Value::Buffer(id));
    }

    if let Some(id) = avoid_id.or(current_id) {
        return Ok(Value::Buffer(id));
    }

    Ok(Value::Nil)
}

/// (generate-new-buffer-name BASE) → string
pub(crate) fn builtin_generate_new_buffer_name(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("generate-new-buffer-name", &args, 1)?;
    expect_max_args("generate-new-buffer-name", &args, 2)?;
    if args.len() == 2
        && !matches!(
            &args[1],
            Value::Nil | Value::True | Value::Str(_) | Value::Symbol(_) | Value::Keyword(_)
        )
    {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[1]],
        ));
    }
    let base = expect_string(&args[0])?;
    Ok(Value::string(eval.buffers.generate_new_buffer_name(&base)))
}

/// (bufferp OBJECT) → t or nil
pub(crate) fn builtin_bufferp(args: Vec<Value>) -> EvalResult {
    expect_args("bufferp", &args, 1)?;
    Ok(Value::bool(matches!(args[0], Value::Buffer(_))))
}

/// (char-after &optional POS) → integer or nil
pub(crate) fn builtin_char_after(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let byte_pos = if args.is_empty() || matches!(args[0], Value::Nil) {
        (buf.point() < buf.zv).then_some(buf.point())
    } else {
        let pos = expect_int(&args[0])?;
        if pos <= 0 {
            return Ok(Value::Nil);
        }
        let point_min = point_char_pos(buf, buf.begv);
        let point_max = point_char_pos(buf, buf.zv);
        if pos < point_min || pos >= point_max {
            return Ok(Value::Nil);
        }
        Some(buf.text.char_to_byte((pos - 1) as usize))
    };
    match byte_pos.and_then(|pos| buf.char_after(pos)) {
        Some(c) => Ok(Value::Int(c as i64)),
        None => Ok(Value::Nil),
    }
}

/// (char-before &optional POS) → integer or nil
pub(crate) fn builtin_char_before(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let byte_pos = if args.is_empty() || matches!(args[0], Value::Nil) {
        (buf.point() > buf.begv).then_some(buf.point())
    } else {
        let pos = expect_int(&args[0])?;
        if pos <= 0 {
            return Ok(Value::Nil);
        }
        let point_min = point_char_pos(buf, buf.begv);
        let point_max = point_char_pos(buf, buf.zv);
        if pos <= point_min || pos > point_max {
            return Ok(Value::Nil);
        }
        Some(buf.text.char_to_byte((pos - 1) as usize))
    };
    match byte_pos.and_then(|pos| buf.char_before(pos)) {
        Some(c) => Ok(Value::Int(c as i64)),
        None => Ok(Value::Nil),
    }
}

fn is_unibyte_storage_string(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|ch| (0xE300..=0xE3FF).contains(&(ch as u32)))
}

fn get_byte_from_multibyte_char_code(code: u32) -> EvalResult {
    if code <= 0x7F {
        return Ok(Value::Int(code as i64));
    }
    if (0x3FFF80..=0x3FFFFF).contains(&code) {
        return Ok(Value::Int((code - 0x3FFF00) as i64));
    }
    Err(signal(
        "error",
        vec![Value::string(format!(
            "Not an ASCII nor an 8-bit character: {code}"
        ))],
    ))
}

/// `(byte-to-position BYTEPOS)` -- map a 1-based byte position to 1-based char position.
pub(crate) fn builtin_byte_to_position(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("byte-to-position", &args, 1)?;
    let byte_pos = expect_fixnum(&args[0])?;
    if byte_pos <= 0 {
        return Ok(Value::Nil);
    }

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let byte_len = buf.text.len();
    let byte_pos0 = (byte_pos - 1) as usize;
    if byte_pos0 > byte_len {
        return Ok(Value::Nil);
    }

    // Emacs maps interior UTF-8 continuation bytes to the containing character.
    let mut boundary = byte_pos0;
    while boundary > 0 && boundary < byte_len {
        let b = buf.text.byte_at(boundary);
        if (b & 0b1100_0000) != 0b1000_0000 {
            break;
        }
        boundary -= 1;
    }

    Ok(Value::Int(buf.text.byte_to_char(boundary) as i64 + 1))
}

/// `(position-bytes POSITION)` -- map a 1-based char position to a 1-based byte position.
pub(crate) fn builtin_position_bytes(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("position-bytes", &args, 1)?;
    let pos = expect_integer_or_marker(&args[0])?;

    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let max_char_pos = buf.text.char_count() as i64 + 1;
    if pos <= 0 || pos > max_char_pos {
        return Ok(Value::Nil);
    }

    let byte_pos = buf.text.char_to_byte((pos - 1) as usize);
    Ok(Value::Int(byte_pos as i64 + 1))
}

/// `(get-byte &optional POSITION STRING)` -- return a byte value at point or in STRING.
pub(crate) fn builtin_get_byte(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_max_args("get-byte", &args, 2)?;

    // STRING path: POSITION is a zero-based character index.
    if args.get(1).is_some_and(|v| !v.is_nil()) {
        let string_value = args[1];
        let s = expect_string(&args[1])?;
        let pos = if args.is_empty() || args[0].is_nil() {
            0usize
        } else {
            expect_wholenump(&args[0])? as usize
        };

        let char_len = storage_char_len(&s);
        if pos >= char_len && !s.is_empty() {
            return Err(signal(
                "args-out-of-range",
                vec![string_value, Value::Int(pos as i64)],
            ));
        }

        // Emacs returns 0 for the terminating NUL when indexing an empty string.
        if char_len == 0 {
            return Ok(Value::Int(0));
        }

        let code = decode_storage_char_codes(&s)[pos];
        if is_unibyte_storage_string(&s) {
            return Ok(Value::Int((code & 0xFF) as i64));
        }
        return get_byte_from_multibyte_char_code(code);
    }

    // Buffer path: POSITION is a 1-based character position.
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let byte_pos = if args.is_empty() || args[0].is_nil() {
        buf.point()
    } else {
        let pos = expect_integer_or_marker(&args[0])?;
        let point_min = buf.text.byte_to_char(buf.point_min()) as i64 + 1;
        let point_max = buf.text.byte_to_char(buf.point_max()) as i64 + 1;
        if pos < point_min || pos >= point_max {
            return Err(signal(
                "args-out-of-range",
                vec![args[0], Value::Int(point_min), Value::Int(point_max)],
            ));
        }
        buf.text.char_to_byte((pos - 1) as usize)
    };

    if byte_pos >= buf.text.len() {
        return Ok(Value::Int(0));
    }

    if !buf.multibyte {
        return Ok(Value::Int(buf.text.byte_at(byte_pos) as i64));
    }

    let code = match buf.char_after(byte_pos) {
        Some(ch) => ch as u32,
        None => return Ok(Value::Int(0)),
    };

    if (0xE080..=0xE0FF).contains(&code) {
        return Ok(Value::Int((code - 0xE000) as i64));
    }
    if (0xE300..=0xE3FF).contains(&code) {
        return Ok(Value::Int((code - 0xE300) as i64));
    }

    get_byte_from_multibyte_char_code(code)
}

/// (buffer-local-value VARIABLE BUFFER) → value
pub(crate) fn builtin_buffer_local_value(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("buffer-local-value", &args, 2)?;
    let name = args[0].as_symbol_name().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )
    })?;
    let resolved = resolve_variable_alias_name(eval, name)?;
    let id = expect_buffer_id(&args[1])?;
    let buf = eval
        .buffers
        .get(id)
        .ok_or_else(|| signal("error", vec![Value::string("No such buffer")]))?;
    match buf.buffer_local_value(&resolved) {
        Some(v) => Ok(v),
        None if resolved == "nil" => Ok(Value::Nil),
        None if resolved == "t" => Ok(Value::True),
        None if resolved.starts_with(':') => Ok(Value::symbol(resolved)),
        None => eval
            .obarray()
            .symbol_value(&resolved)
            .cloned()
            .ok_or_else(|| signal("void-variable", vec![Value::symbol(name)])),
    }
}
