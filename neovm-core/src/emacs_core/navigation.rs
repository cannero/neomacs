//! Buffer navigation, line operations, and mark/region management builtins.
//!
//! All functions here take `(eval: &mut Context, args: Vec<Value>) -> EvalResult`
//! and are dispatched from `builtins.rs` via `dispatch_builtin`.

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::textprop::lookup_buffer_text_property;
use super::value::{Value, ValueKind, lexenv_lookup};
use crate::buffer::BufferManager;

// ---------------------------------------------------------------------------
// Argument helpers (duplicated from builtins.rs — they are not `pub`)
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

fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
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

fn dynamic_or_global_symbol_value(eval: &super::eval::Context, name: &str) -> Option<Value> {
    let name_id = intern(name);
    if eval.lexical_binding() && !eval.obarray.is_special(name) {
        if let Some(v) = lexenv_lookup(eval.lexenv, name_id) {
            return Some(v);
        }
    }

    if let Some(buf) = eval.buffers.current_buffer() {
        if let Some(v) = buf.get_buffer_local(name) {
            return Some(v);
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
// Point motion hooks and intangible support
// ===========================================================================

pub(crate) fn check_point_motion_hooks(
    eval: &mut super::eval::Context,
    old_byte: usize,
    new_byte: usize,
) -> Result<(), Flow> {
    if old_byte == new_byte {
        return Ok(());
    }
    let inhibit = eval
        .obarray
        .symbol_value("inhibit-point-motion-hooks")
        .cloned()
        .unwrap_or(Value::NIL);
    if inhibit.is_truthy() {
        return Ok(());
    }
    let current_id = match eval.buffers.current_buffer_id() {
        Some(id) => id,
        None => return Ok(()),
    };
    let (old_lisp, new_lisp, leave_before, leave_after, enter_before, enter_after) = {
        let buf = match eval.buffers.get(current_id) {
            Some(b) => b,
            None => return Ok(()),
        };
        let ol = buf.text.byte_to_char(old_byte) as i64 + 1;
        let nl = buf.text.byte_to_char(new_byte) as i64 + 1;
        let leave_before = point_motion_property(
            &eval.obarray,
            &eval.buffers,
            buf,
            old_byte,
            false,
            "point-left",
        );
        let leave_after = point_motion_property(
            &eval.obarray,
            &eval.buffers,
            buf,
            old_byte,
            true,
            "point-left",
        );
        let enter_before = point_motion_property(
            &eval.obarray,
            &eval.buffers,
            buf,
            new_byte,
            false,
            "point-entered",
        );
        let enter_after = point_motion_property(
            &eval.obarray,
            &eval.buffers,
            buf,
            new_byte,
            true,
            "point-entered",
        );
        (ol, nl, leave_before, leave_after, enter_before, enter_after)
    };

    if leave_before != enter_before && leave_before.is_truthy() {
        eval.apply(
            leave_before,
            vec![Value::fixnum(old_lisp), Value::fixnum(new_lisp)],
        )?;
    }
    if leave_after != enter_after && leave_after.is_truthy() {
        eval.apply(
            leave_after,
            vec![Value::fixnum(old_lisp), Value::fixnum(new_lisp)],
        )?;
    }
    if enter_before != leave_before && enter_before.is_truthy() {
        eval.apply(
            enter_before,
            vec![Value::fixnum(old_lisp), Value::fixnum(new_lisp)],
        )?;
    }
    if enter_after != leave_after && enter_after.is_truthy() {
        eval.apply(
            enter_after,
            vec![Value::fixnum(old_lisp), Value::fixnum(new_lisp)],
        )?;
    }
    Ok(())
}

fn point_motion_property(
    obarray: &super::symbol::Obarray,
    buffers: &BufferManager,
    buf: &crate::buffer::Buffer,
    point_byte: usize,
    after_point: bool,
    property: &str,
) -> Value {
    if after_point {
        if point_byte >= buf.zv {
            return Value::NIL;
        }
        lookup_buffer_text_property(obarray, buffers, buf, point_byte, property)
    } else {
        if point_byte <= buf.begv {
            return Value::NIL;
        }
        lookup_buffer_text_property(obarray, buffers, buf, point_byte - 1, property)
    }
}

pub(crate) fn adjust_for_intangible(
    eval: &super::eval::Context,
    pos: usize,
    direction: i32,
) -> usize {
    let inhibit = eval
        .obarray
        .symbol_value("inhibit-point-motion-hooks")
        .cloned()
        .unwrap_or(Value::NIL);
    if inhibit.is_truthy() {
        return pos;
    }
    let current_id = match eval.buffers.current_buffer_id() {
        Some(id) => id,
        None => return pos,
    };
    let buf = match eval.buffers.get(current_id) {
        Some(b) => b,
        None => return pos,
    };
    let intangible =
        lookup_buffer_text_property(&eval.obarray, &eval.buffers, buf, pos, "intangible");
    if !intangible.is_truthy() {
        return pos;
    }
    let mut cursor = pos;
    if direction >= 0 {
        loop {
            match buf.text.text_props_next_change(cursor) {
                Some(next) => {
                    let prop = lookup_buffer_text_property(
                        &eval.obarray,
                        &eval.buffers,
                        buf,
                        next,
                        "intangible",
                    );
                    cursor = next;
                    if !prop.is_truthy() {
                        break;
                    }
                }
                None => {
                    cursor = buf.zv;
                    break;
                }
            }
        }
    } else {
        loop {
            match buf.text.text_props_previous_change(cursor) {
                Some(prev) => {
                    let check = prev.saturating_sub(1);
                    let prop = lookup_buffer_text_property(
                        &eval.obarray,
                        &eval.buffers,
                        buf,
                        check,
                        "intangible",
                    );
                    cursor = prev;
                    if !prop.is_truthy() {
                        break;
                    }
                }
                None => {
                    cursor = buf.begv;
                    break;
                }
            }
        }
    }
    cursor
}

// ===========================================================================
// Position predicates
// ===========================================================================

/// (bobp) -- at beginning of buffer?
pub(crate) fn builtin_bobp(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("bobp", &args, 0)?;
    let buf = current_buffer_in_manager(&ctx.buffers)?;
    Ok(Value::bool_val(buf.pt == buf.begv))
}

/// (eobp) -- at end of buffer?
pub(crate) fn builtin_eobp(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("eobp", &args, 0)?;
    let buf = current_buffer_in_manager(&ctx.buffers)?;
    Ok(Value::bool_val(buf.pt == buf.zv))
}

/// (bolp) -- at beginning of line?
pub(crate) fn builtin_bolp(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("bolp", &args, 0)?;
    let buf = current_buffer_in_manager(&ctx.buffers)?;
    if buf.pt == buf.begv {
        return Ok(Value::T);
    }
    let text = buffer_text(buf);
    let at_bol = buf.pt > 0 && buf.pt <= text.len() && text.as_bytes()[buf.pt - 1] == b'\n';
    Ok(Value::bool_val(buf.pt == 0 || at_bol))
}

/// (eolp) -- at end of line?
pub(crate) fn builtin_eolp(ctx: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("eolp", &args, 0)?;
    let buf = current_buffer_in_manager(&ctx.buffers)?;
    if buf.pt == buf.zv {
        return Ok(Value::T);
    }
    match buf.char_after(buf.pt) {
        Some('\n') => Ok(Value::T),
        _ => Ok(Value::NIL),
    }
}

// ===========================================================================
// Line operations
// ===========================================================================

/// (line-beginning-position &optional N)
pub(crate) fn builtin_line_beginning_position(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("line-beginning-position", &args, 1)?;
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let buf = current_buffer_in_manager(&ctx.buffers)?;
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
    Ok(Value::fixnum(byte_to_char_pos(buf, bol)))
}

/// (line-end-position &optional N)
pub(crate) fn builtin_line_end_position(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("line-end-position", &args, 1)?;
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let buf = current_buffer_in_manager(&ctx.buffers)?;
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
        return Ok(Value::fixnum(byte_to_char_pos(buf, begv)));
    }
    let eol = line_end_byte_narrowed(&text, pos, zv);
    Ok(Value::fixnum(byte_to_char_pos(buf, eol)))
}

/// (line-number-at-pos &optional POS ABSOLUTE)
pub(crate) fn builtin_line_number_at_pos(
    eval: &mut super::eval::Context,
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
    Ok(Value::fixnum(line_num as i64))
}

/// (count-lines BEG END)
pub(crate) fn builtin_count_lines(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
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
    Ok(Value::fixnum(n as i64))
}

/// (forward-line &optional N) -> integer
pub(crate) fn builtin_forward_line(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let current_id = eval.buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (text, begv, zv, pt) = {
        let buf = eval.buffers.get(current_id).ok_or_else(no_buffer)?;
        (buffer_text(buf), buf.begv, buf.zv, buf.pt)
    };
    let old_byte = pt;
    let (new_pos, moved) = move_by_lines_narrowed(&text, pt, n, begv, zv);
    let direction = if n >= 0 { 1 } else { -1 };
    let adjusted = adjust_for_intangible(eval, new_pos, direction);
    let _ = eval.buffers.goto_buffer_byte(current_id, adjusted);

    let mut shortage = n - moved;
    if shortage != 0
        && n > 0
        && begv < zv
        && new_pos != pt
        && new_pos > 0
        && text.as_bytes()[new_pos - 1] != b'\n'
    {
        shortage -= 1;
    }
    check_point_motion_hooks(eval, old_byte, adjusted)?;
    Ok(Value::fixnum(shortage))
}

/// (beginning-of-line &optional N)
pub(crate) fn builtin_beginning_of_line(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let current_id = eval.buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (text, begv, zv, pt) = {
        let buf = eval.buffers.get(current_id).ok_or_else(no_buffer)?;
        (buffer_text(buf), buf.begv, buf.zv, buf.pt)
    };
    let old_byte = pt;
    let mut pos = pt;
    if n != 1 {
        let delta = n - 1;
        let (new_pos, _) = move_by_lines_narrowed(&text, pos, delta, begv, zv);
        pos = new_pos;
    }
    let bol = line_beginning_byte_narrowed(&text, pos, begv);
    let adjusted = adjust_for_intangible(eval, bol, -1);
    let _ = eval.buffers.goto_buffer_byte(current_id, adjusted);
    check_point_motion_hooks(eval, old_byte, adjusted)?;
    Ok(Value::NIL)
}

/// (end-of-line &optional N)
pub(crate) fn builtin_end_of_line(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let current_id = eval.buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (text, begv, zv, pt) = {
        let buf = eval.buffers.get(current_id).ok_or_else(no_buffer)?;
        (buffer_text(buf), buf.begv, buf.zv, buf.pt)
    };
    let old_byte = pt;
    let mut pos = pt;
    let mut moved = 0;
    if n != 1 {
        let delta = n - 1;
        let (new_pos, actual_moved) = move_by_lines_narrowed(&text, pos, delta, begv, zv);
        pos = new_pos;
        moved = actual_moved;
    }
    if n != 1 && moved != n - 1 && pos == begv {
        let adjusted = adjust_for_intangible(eval, begv, -1);
        let _ = eval.buffers.goto_buffer_byte(current_id, adjusted);
        check_point_motion_hooks(eval, old_byte, adjusted)?;
        return Ok(Value::NIL);
    }
    let eol = line_end_byte_narrowed(&text, pos, zv);
    let adjusted = adjust_for_intangible(eval, eol, 1);
    let _ = eval.buffers.goto_buffer_byte(current_id, adjusted);
    check_point_motion_hooks(eval, old_byte, adjusted)?;
    Ok(Value::NIL)
}

// ===========================================================================
// Character movement
// ===========================================================================

/// (forward-char &optional N)
///
/// Mirrors GNU `Fforward_char` (`src/cmds.c:69`) and `move_point` at
/// `src/cmds.c:36`. The accessible portion of the buffer is bounded by
/// `BEGV` / `ZV` (the narrowing region), not the absolute buffer
/// extents — `forward-char` must clamp to and signal against those
/// fields, otherwise narrowing is silently ignored (audit §7.1).
pub(crate) fn builtin_forward_char(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let current_id = eval.buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (old_byte, cur_char, begv_char, zv_char, new_byte) = {
        let buf = eval.buffers.get(current_id).ok_or_else(no_buffer)?;
        let old_byte = buf.pt;
        let cur_char = buf.point_char();
        let begv_char = buf.point_min_char();
        let zv_char = buf.point_max_char();
        let desired = cur_char as i64 + n;
        let clamped_char = desired.clamp(begv_char as i64, zv_char as i64) as usize;
        (
            old_byte,
            cur_char,
            begv_char,
            zv_char,
            buf.text.char_to_byte(clamped_char),
        )
    };
    let direction = if n >= 0 { 1 } else { -1 };
    let adjusted = adjust_for_intangible(eval, new_byte, direction);
    let _ = eval.buffers.goto_buffer_byte(current_id, adjusted);
    // GNU `move_point`: signal beginning-of-buffer / end-of-buffer when
    // the requested position falls outside the accessible portion.
    let desired = cur_char as i64 + n;
    if desired < begv_char as i64 {
        return Err(signal("beginning-of-buffer", vec![]));
    }
    if desired > zv_char as i64 {
        return Err(signal("end-of-buffer", vec![]));
    }
    check_point_motion_hooks(eval, old_byte, adjusted)?;
    Ok(Value::NIL)
}

/// (backward-char &optional N)
pub(crate) fn builtin_backward_char(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    // backward-char N == forward-char (- N)
    builtin_forward_char(eval, vec![Value::fixnum(-n)])
}

/// Parse a skip-chars set matching GNU syntax.c skip_chars behavior.
/// Handles `\` as escape character and `-` as range operator.
fn parse_skip_chars_set(s: &str) -> (bool, Vec<char>) {
    let mut chars: Vec<char> = Vec::new();
    let mut negate = false;
    let raw: Vec<char> = s.chars().collect();
    let mut i = 0;

    if i < raw.len() && raw[i] == '^' {
        negate = true;
        i += 1;
    }

    while i < raw.len() {
        // Handle backslash escape (GNU syntax.c: `\-` = literal `-`)
        let c = if raw[i] == '\\' && i + 1 < raw.len() {
            i += 1;
            raw[i]
        } else {
            raw[i]
        };
        i += 1;

        // Check for range: c followed by `-` and another char
        if i + 1 < raw.len() && raw[i] == '-' {
            i += 1; // skip '-'
            let end_c = if raw[i] == '\\' && i + 1 < raw.len() {
                i += 1;
                raw[i]
            } else {
                raw[i]
            };
            i += 1;
            if c <= end_c {
                for ch in c..=end_c {
                    if !chars.contains(&ch) {
                        chars.push(ch);
                    }
                }
            }
        } else if !chars.contains(&c) {
            chars.push(c);
        }
    }
    (negate, chars)
}

/// (skip-chars-forward STRING &optional LIM)
pub(crate) fn builtin_skip_chars_forward(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("skip-chars-forward", &args, 1)?;
    let set_str = match args[0].kind() {
        ValueKind::String => args[0].as_str().unwrap().to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };
    let (negate, char_set) = parse_skip_chars_set(&set_str);
    let current_id = ctx.buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (start_pos, pos, limit, moved_chars) = {
        let buf = ctx.buffers.get(current_id).ok_or_else(no_buffer)?;
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
    let _ = ctx.buffers.goto_buffer_byte(current_id, pos);
    Ok(Value::fixnum(moved_chars))
}

/// (skip-chars-backward STRING &optional LIM)
pub(crate) fn builtin_skip_chars_backward(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("skip-chars-backward", &args, 1)?;
    let set_str = match args[0].kind() {
        ValueKind::String => args[0].as_str().unwrap().to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };
    let (negate, char_set) = parse_skip_chars_set(&set_str);
    let current_id = ctx.buffers.current_buffer_id().ok_or_else(no_buffer)?;
    let (pos, moved_chars) = {
        let buf = ctx.buffers.get(current_id).ok_or_else(no_buffer)?;
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
    let _ = ctx.buffers.goto_buffer_byte(current_id, pos);
    Ok(Value::fixnum(moved_chars))
}

// ===========================================================================
// Mark and region
// ===========================================================================

/// (mark &optional FORCE) -> integer or signal
pub(crate) fn builtin_mark_nav(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let _force = args.first().is_some_and(|v| v.is_truthy());
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    match buf.mark() {
        Some(byte_pos) => Ok(Value::fixnum(byte_to_char_pos(buf, byte_pos))),
        None => Ok(Value::NIL),
    }
}

/// (region-beginning) -> integer
pub(crate) fn builtin_region_beginning(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("region-beginning", &args, 0)?;
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
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
    Ok(Value::fixnum(byte_to_char_pos(buf, start)))
}

/// (region-end) -> integer
pub(crate) fn builtin_region_end(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("region-end", &args, 0)?;
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
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
    Ok(Value::fixnum(byte_to_char_pos(buf, end)))
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
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("transient-mark-mode"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }
    let sym_id = intern("transient-mark-mode");
    let current = eval
        .obarray
        .symbol_value("transient-mark-mode")
        .cloned()
        .unwrap_or(Value::NIL);

    let new_val = if args.is_empty() || args[0].is_nil() {
        // No arg or nil → enable
        Value::T
    } else if args[0].is_symbol_named("toggle") {
        // 'toggle → flip
        if current.is_truthy() {
            Value::NIL
        } else {
            Value::T
        }
    } else {
        // Numeric arg: positive → enable, zero/negative → disable.
        // Floats are truncated to integer first (GNU define-minor-mode behavior).
        match args[0].kind() {
            ValueKind::Fixnum(n) => {
                if n > 0 {
                    Value::T
                } else {
                    Value::NIL
                }
            }
            ValueKind::Float => {
                let truncated = args[0].xfloat() as i64;
                if truncated > 0 { Value::T } else { Value::NIL }
            }
            _ => Value::T,
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
