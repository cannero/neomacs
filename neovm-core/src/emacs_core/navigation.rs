//! Buffer navigation, line operations, and mark/region management builtins.
//!
//! All functions here take `(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult`
//! and are dispatched from `builtins.rs` via `dispatch_builtin`.

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::value::{Value, lexenv_lookup, read_cons, with_heap};
use crate::buffer::BufferManager;

// ---------------------------------------------------------------------------
// Argument helpers (duplicated from builtins.rs — they are not `pub`)
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

fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

/// Get a no-current-buffer signal flow.
fn no_buffer() -> Flow {
    signal("error", vec![Value::string("No current buffer")])
}

fn current_buffer_in_manager(buffers: &BufferManager) -> Result<&crate::buffer::Buffer, Flow> {
    buffers.current_buffer().ok_or_else(no_buffer)
}

fn dynamic_or_global_symbol_value(eval: &super::eval::Evaluator, name: &str) -> Option<Value> {
    let name_id = intern(name);
    if eval.lexical_binding() && !eval.obarray.is_special(name) {
        if let Some(v) = lexenv_lookup(eval.lexenv, name_id) {
            return Some(v);
        }
    }

    for frame in eval.dynamic.iter().rev() {
        if let Some(v) = frame.get(&name_id) {
            return Some(*v);
        }
    }

    if let Some(buf) = eval.buffers.current_buffer() {
        if let Some(v) = buf.get_buffer_local(name) {
            return Some(*v);
        }
    }

    eval.obarray.symbol_value(name).cloned()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Convert a 1-based Emacs char position to a 0-based byte position in the
/// current buffer.  Clamps to valid range.
fn char_pos_to_byte(buf: &crate::buffer::Buffer, pos: i64) -> usize {
    buf.lisp_pos_to_byte(pos)
}

/// Convert a 0-based byte position to a 1-based Emacs char position.
fn byte_to_char_pos(buf: &crate::buffer::Buffer, byte_pos: usize) -> i64 {
    buf.text.byte_to_char(byte_pos) as i64 + 1
}

/// Return the full buffer text as a String.
fn buffer_text(buf: &crate::buffer::Buffer) -> String {
    buf.text.to_string()
}

/// Find the byte position of the beginning of the line containing `byte_pos`.
fn line_beginning_byte(text: &str, byte_pos: usize) -> usize {
    // Search backwards for '\n'.
    let pos = byte_pos.min(text.len());
    match text[..pos].rfind('\n') {
        Some(nl) => nl + 1,
        None => 0,
    }
}

/// Find the byte position of the end of the line containing `byte_pos`
/// (position of the '\n', or text length if no trailing newline).
fn line_end_byte(text: &str, byte_pos: usize) -> usize {
    let pos = byte_pos.min(text.len());
    match text[pos..].find('\n') {
        Some(offset) => pos + offset,
        None => text.len(),
    }
}

/// Count newlines in the byte range [start, end).
fn count_newlines(text: &str, start: usize, end: usize) -> usize {
    let s = start.min(text.len());
    let e = end.min(text.len());
    text[s..e].chars().filter(|&c| c == '\n').count()
}

/// Move from `byte_pos` by `n` lines.  Positive = forward, negative = backward.
/// Returns the byte position at the beginning of the destination line and the
/// number of lines actually moved (may be fewer than requested at buffer edges).
fn move_by_lines(text: &str, byte_pos: usize, n: i64) -> (usize, i64) {
    move_by_lines_narrowed(text, byte_pos, n, 0, text.len())
}

/// Like `move_by_lines` but confined to the narrowed region `[begv, zv)`.
fn move_by_lines_narrowed(
    text: &str,
    byte_pos: usize,
    n: i64,
    begv: usize,
    zv: usize,
) -> (usize, i64) {
    let zv = zv.min(text.len());
    let mut pos = byte_pos.clamp(begv, zv);
    let mut moved: i64 = 0;
    if n >= 0 {
        if n == 0 {
            return (line_beginning_byte_narrowed(text, pos, begv), 0);
        }
        for _ in 0..n {
            match text[pos..zv].find('\n') {
                Some(offset) => {
                    pos += offset + 1;
                    moved += 1;
                }
                None => {
                    pos = zv;
                    break;
                }
            }
        }
    } else {
        for _ in 0..(-n) {
            let bol = line_beginning_byte_narrowed(text, pos, begv);
            if bol <= begv {
                pos = begv;
                break;
            }
            pos = line_beginning_byte_narrowed(text, bol - 1, begv);
            moved -= 1;
        }
    }
    (pos, moved)
}

/// Find the beginning of the line containing `byte_pos`, but not before `begv`.
fn line_beginning_byte_narrowed(text: &str, byte_pos: usize, begv: usize) -> usize {
    let pos = byte_pos.min(text.len());
    let start = begv.min(pos);
    match text[start..pos].rfind('\n') {
        Some(offset) => start + offset + 1,
        None => start,
    }
}

/// Find the end of the line containing `byte_pos`, but not past `zv`.
fn line_end_byte_narrowed(text: &str, byte_pos: usize, zv: usize) -> usize {
    let pos = byte_pos.min(text.len());
    let end = zv.min(text.len());
    match text[pos..end].find('\n') {
        Some(offset) => pos + offset,
        None => end,
    }
}

// ===========================================================================
// Position predicates
// ===========================================================================

/// (bobp) -- at beginning of buffer?
pub(crate) fn builtin_bobp(eval: &mut super::eval::Evaluator, _args: Vec<Value>) -> EvalResult {
    builtin_bobp_in_manager(&eval.buffers, _args)
}

/// (eobp) -- at end of buffer?
pub(crate) fn builtin_eobp(eval: &mut super::eval::Evaluator, _args: Vec<Value>) -> EvalResult {
    builtin_eobp_in_manager(&eval.buffers, _args)
}

/// (bolp) -- at beginning of line?
pub(crate) fn builtin_bolp(eval: &mut super::eval::Evaluator, _args: Vec<Value>) -> EvalResult {
    builtin_bolp_in_manager(&eval.buffers, _args)
}

/// (eolp) -- at end of line?
pub(crate) fn builtin_eolp(eval: &mut super::eval::Evaluator, _args: Vec<Value>) -> EvalResult {
    builtin_eolp_in_manager(&eval.buffers, _args)
}

// ===========================================================================
// Line operations
// ===========================================================================

/// (line-beginning-position &optional N)
pub(crate) fn builtin_line_beginning_position(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_line_beginning_position_in_manager(&eval.buffers, args)
}

pub(crate) fn builtin_bobp_in_manager(buffers: &BufferManager, args: Vec<Value>) -> EvalResult {
    expect_args("bobp", &args, 0)?;
    let buf = current_buffer_in_manager(buffers)?;
    Ok(Value::bool(buf.pt == buf.begv))
}

pub(crate) fn builtin_eobp_in_manager(buffers: &BufferManager, args: Vec<Value>) -> EvalResult {
    expect_args("eobp", &args, 0)?;
    let buf = current_buffer_in_manager(buffers)?;
    Ok(Value::bool(buf.pt == buf.zv))
}

pub(crate) fn builtin_bolp_in_manager(buffers: &BufferManager, args: Vec<Value>) -> EvalResult {
    expect_args("bolp", &args, 0)?;
    let buf = current_buffer_in_manager(buffers)?;
    if buf.pt == buf.begv {
        return Ok(Value::True);
    }
    let text = buffer_text(buf);
    let at_bol = buf.pt > 0 && buf.pt <= text.len() && text.as_bytes()[buf.pt - 1] == b'\n';
    Ok(Value::bool(buf.pt == 0 || at_bol))
}

pub(crate) fn builtin_eolp_in_manager(buffers: &BufferManager, args: Vec<Value>) -> EvalResult {
    expect_args("eolp", &args, 0)?;
    let buf = current_buffer_in_manager(buffers)?;
    if buf.pt == buf.zv {
        return Ok(Value::True);
    }
    match buf.char_after(buf.pt) {
        Some('\n') => Ok(Value::True),
        _ => Ok(Value::Nil),
    }
}

pub(crate) fn builtin_line_beginning_position_in_manager(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("line-beginning-position", &args, 1)?;
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let buf = current_buffer_in_manager(buffers)?;
    let text = buffer_text(buf);
    let begv = buf.begv;
    let zv = buf.zv;
    let mut pos = buf.pt;
    if n != 1 {
        let delta = n - 1;
        let (new_pos, _) = move_by_lines_narrowed(&text, pos, delta, begv, zv);
        pos = new_pos;
    }
    let bol = line_beginning_byte_narrowed(&text, pos, begv);
    Ok(Value::Int(byte_to_char_pos(buf, bol)))
}

/// (line-end-position &optional N)
pub(crate) fn builtin_line_end_position(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_line_end_position_in_manager(&eval.buffers, args)
}

pub(crate) fn builtin_line_end_position_in_manager(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("line-end-position", &args, 1)?;
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let buf = current_buffer_in_manager(buffers)?;
    let text = buffer_text(buf);
    let begv = buf.begv;
    let zv = buf.zv;
    let mut pos = buf.pt;
    let mut moved = 0;
    if n != 1 {
        let delta = n - 1;
        let (new_pos, actual_moved) = move_by_lines_narrowed(&text, pos, delta, begv, zv);
        pos = new_pos;
        moved = actual_moved;
    }
    if n != 1 && moved != n - 1 && pos == begv {
        return Ok(Value::Int(byte_to_char_pos(buf, begv)));
    }
    let eol = line_end_byte_narrowed(&text, pos, zv);
    Ok(Value::Int(byte_to_char_pos(buf, eol)))
}

/// (line-number-at-pos &optional POS ABSOLUTE)
pub(crate) fn builtin_line_number_at_pos(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    let byte_pos = if args.is_empty() || args[0].is_nil() {
        buf.pt
    } else {
        char_pos_to_byte(buf, expect_int(&args[0])?)
    };
    let _absolute = args.get(1).is_some_and(|v| v.is_truthy());
    // Count newlines from start of buffer to byte_pos.
    let text = buffer_text(buf);
    let start = if _absolute { 0 } else { buf.begv };
    let line_num = count_newlines(&text, start, byte_pos) + 1;
    Ok(Value::Int(line_num as i64))
}

/// (count-lines BEG END)
pub(crate) fn builtin_count_lines(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("count-lines", &args, 2)?;
    expect_max_args("count-lines", &args, 3)?;
    let beg = expect_int(&args[0])?;
    let end = expect_int(&args[1])?;
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    let byte_beg = char_pos_to_byte(buf, beg);
    let byte_end = char_pos_to_byte(buf, end);
    let (s, e) = if byte_beg <= byte_end {
        (byte_beg, byte_end)
    } else {
        (byte_end, byte_beg)
    };
    let text = buffer_text(buf);
    let mut n = count_newlines(&text, s, e);
    // GNU Emacs: "can be one more if START is not equal to END and the
    // greater of them is not at the start of a line."
    // i.e., if the region is non-empty and the char before `e` is not '\n'.
    if s != e && e > 0 && e <= text.len() && text.as_bytes()[e - 1] != b'\n' {
        n += 1;
    }
    Ok(Value::Int(n as i64))
}

/// (forward-line &optional N) -> integer
pub(crate) fn builtin_forward_line(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_forward_line_in_manager(&mut eval.buffers, args)
}

pub(crate) fn builtin_forward_line_in_manager(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let current_id = buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (text, begv, zv, pt) = {
        let buf = buffers.get(current_id).ok_or_else(no_buffer)?;
        (buffer_text(buf), buf.begv, buf.zv, buf.pt)
    };
    let (new_pos, moved) = move_by_lines_narrowed(&text, pt, n, begv, zv);
    let _ = buffers.goto_buffer_byte(current_id, new_pos);
    Ok(Value::Int(n - moved))
}

/// (beginning-of-line &optional N)
pub(crate) fn builtin_beginning_of_line(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_beginning_of_line_in_manager(&mut eval.buffers, args)
}

pub(crate) fn builtin_beginning_of_line_in_manager(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let current_id = buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (text, begv, zv, pt) = {
        let buf = buffers.get(current_id).ok_or_else(no_buffer)?;
        (buffer_text(buf), buf.begv, buf.zv, buf.pt)
    };
    let mut pos = pt;
    if n != 1 {
        let delta = n - 1;
        let (new_pos, _) = move_by_lines_narrowed(&text, pos, delta, begv, zv);
        pos = new_pos;
    }
    let bol = line_beginning_byte_narrowed(&text, pos, begv);
    let _ = buffers.goto_buffer_byte(current_id, bol);
    Ok(Value::Nil)
}

/// (end-of-line &optional N)
pub(crate) fn builtin_end_of_line(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_end_of_line_in_manager(&mut eval.buffers, args)
}

pub(crate) fn builtin_end_of_line_in_manager(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let current_id = buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (text, begv, zv, pt) = {
        let buf = buffers.get(current_id).ok_or_else(no_buffer)?;
        (buffer_text(buf), buf.begv, buf.zv, buf.pt)
    };
    let mut pos = pt;
    let mut moved = 0;
    if n != 1 {
        let delta = n - 1;
        let (new_pos, actual_moved) = move_by_lines_narrowed(&text, pos, delta, begv, zv);
        pos = new_pos;
        moved = actual_moved;
    }
    if n != 1 && moved != n - 1 && pos == begv {
        let _ = buffers.goto_buffer_byte(current_id, begv);
        return Ok(Value::Nil);
    }
    let eol = line_end_byte_narrowed(&text, pos, zv);
    let _ = buffers.goto_buffer_byte(current_id, eol);
    Ok(Value::Nil)
}

// ===========================================================================
// Character movement
// ===========================================================================

/// (forward-char &optional N)
pub(crate) fn builtin_forward_char(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_forward_char_in_manager(&mut eval.buffers, args)
}

pub(crate) fn builtin_forward_char_in_manager(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let current_id = buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (cur_char, total_chars, new_byte) = {
        let buf = buffers.get(current_id).ok_or_else(no_buffer)?;
        let cur_char = buf.point_char();
        let total_chars = buf.text.char_count();
        let new_char = if n >= 0 {
            let nc = cur_char.saturating_add(n as usize);
            nc.min(total_chars)
        } else {
            let abs_n = (-n) as usize;
            cur_char.saturating_sub(abs_n)
        };
        (cur_char, total_chars, buf.text.char_to_byte(new_char))
    };
    let _ = buffers.goto_buffer_byte(current_id, new_byte);
    // Signal if we couldn't move the full distance
    let desired = cur_char as i64 + n;
    if desired < 0 {
        return Err(signal("beginning-of-buffer", vec![]));
    }
    if desired > total_chars as i64 {
        return Err(signal("end-of-buffer", vec![]));
    }
    Ok(Value::Nil)
}

/// (backward-char &optional N)
pub(crate) fn builtin_backward_char(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_backward_char_in_manager(&mut eval.buffers, args)
}

pub(crate) fn builtin_backward_char_in_manager(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    // backward-char N == forward-char (- N)
    builtin_forward_char_in_manager(buffers, vec![Value::Int(-n)])
}

/// Parse a skip-chars character set string into a set of chars.
/// Supports ranges like "a-z" and negation with "^" prefix.
fn parse_skip_chars_set(s: &str) -> (bool, Vec<char>) {
    let mut chars: Vec<char> = Vec::new();
    let mut negate = false;
    let mut iter = s.chars().peekable();

    if iter.peek() == Some(&'^') {
        negate = true;
        iter.next();
    }

    let mut prev: Option<char> = None;
    while let Some(c) = iter.next() {
        if c == '-' {
            if let (Some(start), Some(end)) = (prev, iter.peek().copied()) {
                iter.next();
                for ch in start..=end {
                    if !chars.contains(&ch) {
                        chars.push(ch);
                    }
                }
                prev = Some(end);
                continue;
            }
        }
        if !chars.contains(&c) {
            chars.push(c);
        }
        prev = Some(c);
    }
    (negate, chars)
}

/// (skip-chars-forward STRING &optional LIM)
pub(crate) fn builtin_skip_chars_forward(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_skip_chars_forward_in_manager(&mut eval.buffers, args)
}

pub(crate) fn builtin_skip_chars_forward_in_manager(
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("skip-chars-forward", &args, 1)?;
    let set_str = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let (negate, char_set) = parse_skip_chars_set(&set_str);
    let current_id = buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (start_pos, pos, limit, moved_chars) = {
        let buf = buffers.get(current_id).ok_or_else(no_buffer)?;
        let lim_byte = if args.len() > 1 && !args[1].is_nil() {
            char_pos_to_byte(buf, expect_int(&args[1])?)
        } else {
            buf.zv
        };
        let text = buffer_text(buf);
        let start_pos = buf.pt;
        let mut pos = buf.pt;
        let limit = lim_byte.min(text.len());

        while pos < limit {
            if let Some(ch) = buf.text.char_at(pos) {
                let in_set = char_set.contains(&ch);
                if negate {
                    if in_set {
                        break;
                    }
                } else if !in_set {
                    break;
                }
                pos += ch.len_utf8();
            } else {
                break;
            }
        }

        let moved_chars =
            buf.text.byte_to_char(pos) as i64 - buf.text.byte_to_char(start_pos) as i64;
        (start_pos, pos, limit, moved_chars)
    };

    debug_assert!(pos >= start_pos || limit <= start_pos);
    let _ = buffers.goto_buffer_byte(current_id, pos);
    Ok(Value::Int(moved_chars))
}

/// (skip-chars-backward STRING &optional LIM)
pub(crate) fn builtin_skip_chars_backward(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_skip_chars_backward_in_manager(&mut eval.buffers, args)
}

pub(crate) fn builtin_skip_chars_backward_in_manager(
    buffers: &mut crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("skip-chars-backward", &args, 1)?;
    let set_str = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let (negate, char_set) = parse_skip_chars_set(&set_str);
    let current_id = buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (pos, moved_chars) = {
        let buf = buffers.get(current_id).ok_or_else(no_buffer)?;
        let limit = if args.len() > 1 && !args[1].is_nil() {
            char_pos_to_byte(buf, expect_int(&args[1])?)
        } else {
            buf.begv
        };
        let start_pos = buf.pt;
        let mut pos = buf.pt;

        while pos > limit {
            // Find the character before `pos`.
            if let Some(ch) = buf.char_before(pos) {
                let in_set = char_set.contains(&ch);
                if negate {
                    if in_set {
                        break;
                    }
                } else if !in_set {
                    break;
                }
                pos -= ch.len_utf8();
            } else {
                break;
            }
        }

        let moved_chars =
            buf.text.byte_to_char(pos) as i64 - buf.text.byte_to_char(start_pos) as i64;
        (pos, moved_chars)
    };
    let _ = buffers.goto_buffer_byte(current_id, pos);
    Ok(Value::Int(moved_chars))
}

// ===========================================================================
// Mark and region
// ===========================================================================

/// (mark &optional FORCE) -> integer or signal
pub(crate) fn builtin_mark_nav(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    let _force = args.first().is_some_and(|v| v.is_truthy());
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    match buf.mark() {
        Some(byte_pos) => Ok(Value::Int(byte_to_char_pos(buf, byte_pos))),
        None => Ok(Value::Nil),
    }
}

/// (region-beginning) -> integer
pub(crate) fn builtin_region_beginning(
    eval: &mut super::eval::Evaluator,
    _args: Vec<Value>,
) -> EvalResult {
    builtin_region_beginning_in_manager(&eval.buffers, _args)
}

pub(crate) fn builtin_region_beginning_in_manager(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("region-beginning", &args, 0)?;
    let buf = buffers.current_buffer().ok_or_else(no_buffer)?;
    let mark = buf.mark().ok_or_else(|| {
        signal(
            "error",
            vec![Value::string(
                "The mark is not set now, so there is no region",
            )],
        )
    })?;
    let pt = buf.pt;
    let start = pt.min(mark);
    Ok(Value::Int(byte_to_char_pos(buf, start)))
}

/// (region-end) -> integer
pub(crate) fn builtin_region_end(
    eval: &mut super::eval::Evaluator,
    _args: Vec<Value>,
) -> EvalResult {
    builtin_region_end_in_manager(&eval.buffers, _args)
}

pub(crate) fn builtin_region_end_in_manager(
    buffers: &BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("region-end", &args, 0)?;
    let buf = buffers.current_buffer().ok_or_else(no_buffer)?;
    let mark = buf.mark().ok_or_else(|| {
        signal(
            "error",
            vec![Value::string(
                "The mark is not set now, so there is no region",
            )],
        )
    })?;
    let pt = buf.pt;
    let end = pt.max(mark);
    Ok(Value::Int(byte_to_char_pos(buf, end)))
}

// ===========================================================================
// transient-mark-mode  (define-minor-mode in GNU simple.el)
// ===========================================================================

/// `(transient-mark-mode &optional ARG)` — toggle transient-mark-mode.
///
/// Matches GNU's define-minor-mode toggle logic:
/// - no arg or nil  → enable (set to t)
/// - positive number → enable
/// - zero or negative → disable (set to nil)
/// - 'toggle         → flip current value
pub(crate) fn builtin_transient_mark_mode(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("transient-mark-mode"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let sym_id = intern("transient-mark-mode");
    let current = eval
        .obarray
        .symbol_value("transient-mark-mode")
        .cloned()
        .unwrap_or(Value::Nil);

    let new_val = if args.is_empty() || args[0].is_nil() {
        // No arg or nil → enable
        Value::True
    } else if args[0].is_symbol_named("toggle") {
        // 'toggle → flip
        if current.is_truthy() {
            Value::Nil
        } else {
            Value::True
        }
    } else {
        // Numeric arg: positive → enable, zero/negative → disable.
        // Floats are truncated to integer first (GNU define-minor-mode behavior).
        match &args[0] {
            Value::Int(n) => {
                if *n > 0 {
                    Value::True
                } else {
                    Value::Nil
                }
            }
            Value::Float(f, _) => {
                let truncated = *f as i64;
                if truncated > 0 {
                    Value::True
                } else {
                    Value::Nil
                }
            }
            _ => Value::True,
        }
    };

    eval.obarray.set_symbol_value_id(sym_id, new_val);
    Ok(new_val)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "navigation_test.rs"]
mod tests;
