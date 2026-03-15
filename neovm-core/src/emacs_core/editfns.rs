//! Editing-function builtins — point/mark queries, insertion, deletion,
//! substring extraction, and miscellaneous user/system info.
//!
//! Emacs Lisp uses **1-based character positions** while the internal
//! `Buffer` stores **0-based byte positions**.  Every Lisp↔Buffer boundary
//! must convert:
//!
//! - Lisp char pos  →  byte pos:  `buf.text.char_to_byte(lisp_pos - 1)`
//! - byte pos       →  Lisp char: `buf.text.byte_to_char(byte_pos) + 1`

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::symbol::Obarray;
use super::value::*;
use crate::buffer::{Buffer, BufferManager};
#[cfg(unix)]
use std::ffi::CStr;

// ---------------------------------------------------------------------------
// Argument helpers
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

/// Extract an integer (or char-as-integer) from a Value, signalling
/// `wrong-type-argument` on type mismatch.
fn expect_integer(_name: &str, val: &Value) -> Result<i64, Flow> {
    val.as_int().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *val],
        )
    })
}

/// Convert a Lisp 1-based character position to a 0-based byte position,
/// clamping to the accessible region `[begv, zv]`.
pub(crate) fn lisp_pos_to_byte(buf: &crate::buffer::Buffer, lisp_pos: i64) -> usize {
    buf.lisp_pos_to_accessible_byte(lisp_pos)
}

fn dynamic_buffer_or_global_symbol_value(
    obarray: &Obarray,
    dynamic: &[OrderedSymMap],
    buf: Option<&Buffer>,
    name: &str,
) -> Option<Value> {
    let name_id = intern(name);
    for frame in dynamic.iter().rev() {
        if let Some(value) = frame.get(&name_id) {
            return Some(*value);
        }
    }

    if let Some(buf) = buf
        && let Some(value) = buf.get_buffer_local(name)
    {
        return Some(*value);
    }

    obarray.symbol_value(name).copied()
}

pub(crate) fn buffer_read_only_active_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedSymMap],
    buf: &Buffer,
) -> bool {
    let inhibit_name_id = intern("inhibit-read-only");
    for frame in dynamic.iter().rev() {
        if let Some(value) = frame.get(&inhibit_name_id)
            && value.is_truthy()
        {
            return false;
        }
    }

    if let Some(value) = buf.get_buffer_local("inhibit-read-only")
        && value.is_truthy()
    {
        return false;
    }

    if obarray
        .symbol_value("inhibit-read-only")
        .is_some_and(|value| value.is_truthy())
    {
        return false;
    }

    if buf.read_only {
        return true;
    }

    dynamic_buffer_or_global_symbol_value(obarray, dynamic, Some(buf), "buffer-read-only")
        .is_some_and(|value| value.is_truthy())
}

fn buffer_read_only_active(eval: &super::eval::Evaluator, buf: &crate::buffer::Buffer) -> bool {
    buffer_read_only_active_in_state(&eval.obarray, &eval.dynamic, buf)
}

pub(crate) fn ensure_current_buffer_writable_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedSymMap],
    buffers: &BufferManager,
) -> Result<(), Flow> {
    if let Some(buf) = buffers.current_buffer()
        && buffer_read_only_active_in_state(obarray, dynamic, buf)
    {
        return Err(signal("buffer-read-only", vec![Value::Buffer(buf.id)]));
    }
    Ok(())
}

pub(crate) fn ensure_current_buffer_writable(eval: &super::eval::Evaluator) -> Result<(), Flow> {
    ensure_current_buffer_writable_in_state(&eval.obarray, &eval.dynamic, &eval.buffers)
}

fn expect_integer_or_marker_in_buffers(
    buffers: &BufferManager,
    value: &Value,
) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other if super::marker::is_marker(other) => {
            super::marker::marker_position_as_int_with_buffers(buffers, other)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn current_buffer_accessible_char_region_in_buffers(
    buffers: &BufferManager,
    start_arg: &Value,
    end_arg: &Value,
) -> Result<Option<(usize, usize)>, Flow> {
    let Some(buf) = buffers.current_buffer() else {
        return Ok(None);
    };

    let start = expect_integer_or_marker_in_buffers(buffers, start_arg)?;
    let end = expect_integer_or_marker_in_buffers(buffers, end_arg)?;
    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if start < point_min || start > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Buffer(buf.id), *start_arg, *end_arg],
        ));
    }

    let (from, to) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    Ok(Some((
        buf.lisp_pos_to_accessible_byte(from),
        buf.lisp_pos_to_accessible_byte(to),
    )))
}

// ---------------------------------------------------------------------------
// Eval-dependent builtins (need &mut Evaluator for buffer access)
// ---------------------------------------------------------------------------

/// Collect the insertable text from a mixed list of strings and characters.
pub(crate) fn collect_insert_text(_name: &str, args: &[Value]) -> Result<String, Flow> {
    let mut text = String::new();
    for arg in args {
        match arg {
            Value::Str(id) => {
                let s = with_heap(|h| h.get_string(*id).to_owned());
                text.push_str(&s);
            }
            Value::Char(c) => text.push(*c),
            Value::Int(n) => {
                // Emacs treats integers as character codes.
                if let Some(ch) = char::from_u32(*n as u32) {
                    text.push(ch);
                } else {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("characterp"), *arg],
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
    Ok(text)
}

/// `(insert &rest ARGS)` — insert strings or characters at point.
pub(crate) fn builtin_insert(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    let text = collect_insert_text("insert", &args)?;
    ensure_current_buffer_writable(eval)?;
    if let Some(id) = eval.buffers.current_buffer_id() {
        let _ = eval.buffers.insert_into_buffer(id, &text);
    }
    Ok(Value::Nil)
}

/// `(insert-before-markers &rest ARGS)` — insert at point, advancing ALL
/// markers at that position past the inserted text (regardless of their
/// InsertionType).
pub(crate) fn builtin_insert_before_markers(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_insert_before_markers_in_state(&eval.obarray, &eval.dynamic, &mut eval.buffers, args)
}

pub(crate) fn builtin_insert_before_markers_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedSymMap],
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    let text = collect_insert_text("insert-before-markers", &args)?;
    ensure_current_buffer_writable_in_state(obarray, dynamic, buffers)?;
    if let Some(id) = buffers.current_buffer_id() {
        let _ = buffers.insert_into_buffer_before_markers(id, &text);
    }
    Ok(Value::Nil)
}

/// `(delete-char N &optional KILLFLAG)` — delete N characters forward.
pub(crate) fn builtin_delete_char(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_delete_char_in_state(&eval.obarray, &eval.dynamic, &mut eval.buffers, args)
}

pub(crate) fn builtin_delete_char_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedSymMap],
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("delete-char", &args, 1)?;
    expect_max_args("delete-char", &args, 2)?;
    let n = expect_integer("delete-char", &args[0])?;
    ensure_current_buffer_writable_in_state(obarray, dynamic, buffers)?;
    if let Some(current_id) = buffers.current_buffer_id() {
        let Some((start, end)) = ({
            let Some(buf) = buffers.get(current_id) else {
                return Ok(Value::Nil);
            };
            let pt = buf.pt;
            if n > 0 {
                // Delete N characters forward from point.
                let mut end = pt;
                for _ in 0..n {
                    if end >= buf.zv {
                        return Err(signal("end-of-buffer", vec![]));
                    }
                    match buf.char_after(end) {
                        Some(ch) => end += ch.len_utf8(),
                        None => {
                            return Err(signal("end-of-buffer", vec![]));
                        }
                    }
                }
                Some((pt, end))
            } else if n < 0 {
                // Delete |N| characters backward from point.
                let mut start = pt;
                for _ in 0..(-n) {
                    if start <= buf.begv {
                        return Err(signal("beginning-of-buffer", vec![]));
                    }
                    match buf.char_before(start) {
                        Some(ch) => start -= ch.len_utf8(),
                        None => {
                            return Err(signal("beginning-of-buffer", vec![]));
                        }
                    }
                }
                Some((start, pt))
            } else {
                None
            }
        }) else {
            return Ok(Value::Nil);
        };
        let _ = buffers.delete_buffer_region(current_id, start, end);
    }
    Ok(Value::Nil)
}

/// `(buffer-substring START END)` — return text between START and END.
pub(crate) fn builtin_buffer_substring(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("buffer-substring", &args, 2)?;
    let start_pos = expect_integer("buffer-substring", &args[0])?;
    let end_pos = expect_integer("buffer-substring", &args[1])?;
    match eval.buffers.current_buffer() {
        Some(buf) => {
            let start_byte = lisp_pos_to_byte(buf, start_pos);
            let end_byte = lisp_pos_to_byte(buf, end_pos);
            let (lo, hi) = if start_byte <= end_byte {
                (start_byte, end_byte)
            } else {
                (end_byte, start_byte)
            };
            Ok(Value::string(buf.buffer_substring(lo, hi)))
        }
        None => Ok(Value::string("")),
    }
}

/// `(buffer-substring-no-properties START END)` — same as buffer-substring
/// (text properties not yet implemented at the Lisp value level).
pub(crate) fn builtin_buffer_substring_no_properties(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_buffer_substring_no_properties_in_state(&eval.buffers, args)
}

pub(crate) fn builtin_buffer_substring_no_properties_in_state(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("buffer-substring-no-properties", &args, 2)?;
    let Some((start_byte, end_byte)) =
        current_buffer_accessible_char_region_in_buffers(buffers, &args[0], &args[1])?
    else {
        return Ok(Value::string(""));
    };
    let Some(buf) = buffers.current_buffer() else {
        return Ok(Value::string(""));
    };
    Ok(Value::string(buf.buffer_substring(start_byte, end_byte)))
}

/// `(following-char)` — return character after point (0 if at end).
pub(crate) fn builtin_following_char(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_following_char_in_state(&eval.buffers, args)
}

pub(crate) fn builtin_following_char_in_state(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("following-char", &args, 0)?;
    match buffers.current_buffer() {
        Some(buf) => match (buf.pt < buf.zv).then(|| buf.char_after(buf.pt)).flatten() {
            Some(ch) => Ok(Value::Int(ch as i64)),
            None => Ok(Value::Int(0)),
        },
        None => Ok(Value::Int(0)),
    }
}

/// `(preceding-char)` — return character before point (0 if at beginning).
pub(crate) fn builtin_preceding_char(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_preceding_char_in_state(&eval.buffers, args)
}

pub(crate) fn builtin_preceding_char_in_state(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("preceding-char", &args, 0)?;
    match buffers.current_buffer() {
        Some(buf) => match (buf.pt > buf.begv)
            .then(|| buf.char_before(buf.pt))
            .flatten()
        {
            Some(ch) => Ok(Value::Int(ch as i64)),
            None => Ok(Value::Int(0)),
        },
        None => Ok(Value::Int(0)),
    }
}

// ---------------------------------------------------------------------------
// Pure builtins (no evaluator needed)
// ---------------------------------------------------------------------------

/// `(user-uid)` — return effective user ID.
/// Uses the `id -u` command on Unix; falls back to 1000.
pub(crate) fn builtin_user_uid(args: Vec<Value>) -> EvalResult {
    expect_args("user-uid", &args, 0)?;
    Ok(Value::Int(get_uid()))
}

/// `(file-user-uid)` — return the UID used for file ownership.
pub(crate) fn builtin_file_user_uid(args: Vec<Value>) -> EvalResult {
    expect_args("file-user-uid", &args, 0)?;
    Ok(Value::Int(get_uid()))
}

/// `(user-real-uid)` — return real user ID.
pub(crate) fn builtin_user_real_uid(args: Vec<Value>) -> EvalResult {
    expect_args("user-real-uid", &args, 0)?;
    Ok(Value::Int(get_uid()))
}

/// `(group-gid)` — return the effective group ID.
pub(crate) fn builtin_group_gid(args: Vec<Value>) -> EvalResult {
    expect_args("group-gid", &args, 0)?;
    Ok(Value::Int(get_gid()))
}

/// `(file-group-gid)` — return the GID used for file ownership.
pub(crate) fn builtin_file_group_gid(args: Vec<Value>) -> EvalResult {
    expect_args("file-group-gid", &args, 0)?;
    Ok(Value::Int(get_gid()))
}

/// `(group-real-gid)` — return the real group ID.
pub(crate) fn builtin_group_real_gid(args: Vec<Value>) -> EvalResult {
    expect_args("group-real-gid", &args, 0)?;
    Ok(Value::Int(get_gid()))
}

/// `(group-name GID)` — return the group name for numeric GID.
pub(crate) fn builtin_group_name(args: Vec<Value>) -> EvalResult {
    expect_args("group-name", &args, 1)?;
    let gid = match &args[0] {
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        _ => {
            return Err(signal(
                "error",
                vec![Value::string("Invalid GID specification")],
            ));
        }
    };
    if gid < 0 || gid > u32::MAX as i64 {
        return Err(signal(
            "error",
            vec![Value::string("Invalid GID specification")],
        ));
    }
    let Some(name) = lookup_group_name(gid as u32) else {
        return Err(signal(
            "error",
            vec![Value::string("Invalid GID specification")],
        ));
    };
    Ok(Value::string(name))
}

/// `(load-average &optional USE-FLOATS)` — return load averages.
///
/// With USE-FLOATS non-nil, returns 3 floats.
/// With USE-FLOATS nil/omitted, returns 3 integers scaled by 100.
pub(crate) fn builtin_load_average(args: Vec<Value>) -> EvalResult {
    expect_max_args("load-average", &args, 1)?;
    let use_floats = args.first().is_some_and(|value| value.is_truthy());
    let loads = read_load_average().unwrap_or([0.0, 0.0, 0.0]);
    if use_floats {
        Ok(Value::list(vec![
            Value::Float(loads[0], next_float_id()),
            Value::Float(loads[1], next_float_id()),
            Value::Float(loads[2], next_float_id()),
        ]))
    } else {
        Ok(Value::list(vec![
            Value::Int((loads[0] * 100.0) as i64),
            Value::Int((loads[1] * 100.0) as i64),
            Value::Int((loads[2] * 100.0) as i64),
        ]))
    }
}

/// `(logcount INTEGER)` — return the number of 1 bits for nonnegative integers,
/// or the number of 0 bits in two's-complement form for negative integers.
pub(crate) fn builtin_logcount(args: Vec<Value>) -> EvalResult {
    expect_args("logcount", &args, 1)?;
    let n = match &args[0] {
        Value::Int(v) => *v,
        Value::Char(c) => *c as i64,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("integerp"), args[0]],
            ));
        }
    };
    let bits = if n >= 0 {
        (n as u64).count_ones() as i64
    } else {
        ((!n) as u64).count_ones() as i64
    };
    Ok(Value::Int(bits))
}

// ---------------------------------------------------------------------------
// OS helpers (avoid libc dependency)
// ---------------------------------------------------------------------------

/// Retrieve the effective UID via `id -u`, falling back to 1000.
fn get_uid() -> i64 {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(1000)
}

/// Retrieve the effective GID via `id -g`, falling back to 1000.
fn get_gid() -> i64 {
    std::process::Command::new("id")
        .arg("-g")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(1000)
}

#[cfg(unix)]
fn lookup_group_name(gid: u32) -> Option<String> {
    let group = unsafe { libc::getgrgid(gid as libc::gid_t) };
    if group.is_null() {
        return None;
    }
    let name_ptr = unsafe { (*group).gr_name };
    if name_ptr.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(name_ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(not(unix))]
fn lookup_group_name(_gid: u32) -> Option<String> {
    None
}

#[cfg(unix)]
fn read_load_average() -> Option<[f64; 3]> {
    let mut values = [0.0f64; 3];
    let result = unsafe { libc::getloadavg(values.as_mut_ptr(), 3) };
    if result == 3 { Some(values) } else { None }
}

#[cfg(not(unix))]
fn read_load_average() -> Option<[f64; 3]> {
    None
}
#[cfg(test)]
#[path = "editfns_test.rs"]
mod tests;
