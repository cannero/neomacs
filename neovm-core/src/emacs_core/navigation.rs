//! Buffer navigation, line operations, and mark/region management builtins.
//!
//! All functions here take `(eval: &mut Evaluator, args: Vec<Value>) -> EvalResult`
//! and are dispatched from `builtins.rs` via `dispatch_builtin`.

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::value::{Value, lexenv_lookup, read_cons, with_heap};

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
    let char_pos = if pos > 0 { pos as usize - 1 } else { 0 };
    buf.text.char_to_byte(char_pos.min(buf.text.char_count()))
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
    if n == 0 {
        return (line_beginning_byte(text, byte_pos), 0);
    }
    let mut pos = byte_pos.min(text.len());
    let mut moved: i64 = 0;
    if n > 0 {
        for _ in 0..n {
            match text[pos..].find('\n') {
                Some(offset) => {
                    pos = pos + offset + 1;
                    moved += 1;
                }
                None => break,
            }
        }
    } else {
        // Move to start of current line first, if not already there.
        let bol = line_beginning_byte(text, pos);
        if bol < pos {
            pos = bol;
            // This counts as moving backward one line for the purpose of
            // backward navigation — but Emacs counts it differently.
            // Actually, in Emacs `forward-line` with negative N first goes to
            // BOL, and that counts as one of the N lines moved.
            moved -= 1;
        }
        let remaining = n - moved; // remaining is still negative
        for _ in 0..(-remaining) {
            if pos == 0 {
                break;
            }
            // Move before the newline at pos-1
            pos -= 1;
            pos = line_beginning_byte(text, pos);
            moved -= 1;
        }
    }
    (pos, moved)
}

// ===========================================================================
// Position predicates
// ===========================================================================

/// (bobp) -- at beginning of buffer?
pub(crate) fn builtin_bobp(eval: &mut super::eval::Evaluator, _args: Vec<Value>) -> EvalResult {
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    Ok(Value::bool(buf.pt == buf.begv))
}

/// (eobp) -- at end of buffer?
pub(crate) fn builtin_eobp(eval: &mut super::eval::Evaluator, _args: Vec<Value>) -> EvalResult {
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    Ok(Value::bool(buf.pt == buf.zv))
}

/// (bolp) -- at beginning of line?
pub(crate) fn builtin_bolp(eval: &mut super::eval::Evaluator, _args: Vec<Value>) -> EvalResult {
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    if buf.pt == buf.begv {
        return Ok(Value::True);
    }
    let text = buffer_text(buf);
    let at_bol = buf.pt > 0 && buf.pt <= text.len() && text.as_bytes()[buf.pt - 1] == b'\n';
    Ok(Value::bool(buf.pt == 0 || at_bol))
}

/// (eolp) -- at end of line?
pub(crate) fn builtin_eolp(eval: &mut super::eval::Evaluator, _args: Vec<Value>) -> EvalResult {
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    if buf.pt == buf.zv {
        return Ok(Value::True);
    }
    match buf.char_after(buf.pt) {
        Some('\n') => Ok(Value::True),
        _ => Ok(Value::Nil),
    }
}

// ===========================================================================
// Line operations
// ===========================================================================

/// (line-beginning-position &optional N)
pub(crate) fn builtin_line_beginning_position(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    let text = buffer_text(buf);
    let mut pos = buf.pt;
    if n != 1 {
        let delta = n - 1;
        let (new_pos, _) = move_by_lines(&text, pos, delta);
        pos = new_pos;
    }
    let bol = line_beginning_byte(&text, pos);
    Ok(Value::Int(byte_to_char_pos(buf, bol)))
}

/// (line-end-position &optional N)
pub(crate) fn builtin_line_end_position(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    let text = buffer_text(buf);
    let point = buf.pt.min(text.len());
    let current_line = count_newlines(&text, 0, point) as i64 + 1;
    let total_lines = count_newlines(&text, 0, text.len()) as i64 + 1;
    let target_line = current_line + (n - 1);

    if target_line < 1 {
        return Ok(Value::Int(1));
    }
    if target_line > total_lines {
        return Ok(Value::Int(byte_to_char_pos(buf, text.len())));
    }

    let mut line_start = 0usize;
    for _ in 1..(target_line as usize) {
        match text[line_start..].find('\n') {
            Some(offset) => {
                line_start += offset + 1;
            }
            None => {
                line_start = text.len();
                break;
            }
        }
    }
    let eol = line_end_byte(&text, line_start);
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
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let text = buffer_text(buf);
    let (new_pos, moved) = move_by_lines(&text, buf.pt, n);
    buf.goto_char(new_pos);
    Ok(Value::Int(n - moved))
}

/// (next-line &optional N)
pub(crate) fn builtin_next_line(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let remainder = match builtin_forward_line(eval, vec![Value::Int(n)])? {
        Value::Int(v) => v,
        _ => 0,
    };
    if remainder > 0 {
        return Err(signal("end-of-buffer", vec![]));
    }
    if remainder < 0 {
        return Err(signal("beginning-of-buffer", vec![]));
    }
    Ok(Value::Nil)
}

/// (previous-line &optional N)
pub(crate) fn builtin_previous_line(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let before_line = {
        let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
        let text = buffer_text(buf);
        count_newlines(&text, 0, line_beginning_byte(&text, buf.pt)) as i64
    };

    let remainder = match builtin_forward_line(eval, vec![Value::Int(-n)])? {
        Value::Int(v) => v,
        _ => 0,
    };

    let after_line = {
        let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
        let text = buffer_text(buf);
        count_newlines(&text, 0, line_beginning_byte(&text, buf.pt)) as i64
    };

    if n > 0 {
        let moved_up = before_line.saturating_sub(after_line);
        if moved_up < n {
            return Err(signal("beginning-of-buffer", vec![]));
        }
    } else if n < 0 {
        let moved_down = after_line.saturating_sub(before_line);
        let wanted = -n;
        if moved_down < wanted {
            return Err(signal("end-of-buffer", vec![]));
        }
    }

    if remainder > 0 {
        return Err(signal("end-of-buffer", vec![]));
    }
    if remainder < 0 {
        return Err(signal("beginning-of-buffer", vec![]));
    }
    Ok(Value::Nil)
}

/// (beginning-of-line &optional N)
pub(crate) fn builtin_beginning_of_line(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let text = buffer_text(buf);
    let mut pos = buf.pt;
    if n != 1 {
        let delta = n - 1;
        let (new_pos, _) = move_by_lines(&text, pos, delta);
        pos = new_pos;
    }
    let bol = line_beginning_byte(&text, pos);
    buf.goto_char(bol);
    Ok(Value::Nil)
}

/// (end-of-line &optional N)
pub(crate) fn builtin_end_of_line(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let text = buffer_text(buf);
    let mut pos = buf.pt;
    if n != 1 {
        let delta = n - 1;
        let (new_pos, _) = move_by_lines(&text, pos, delta);
        pos = new_pos;
    }
    let eol = line_end_byte(&text, pos);
    buf.goto_char(eol);
    Ok(Value::Nil)
}

/// (beginning-of-buffer &optional ARG)
///
/// GNU Emacs batch behavior keeps this as a command primitive:
/// - nil/missing ARG => point-min
/// - non-nil ARG => point-max
pub(crate) fn builtin_beginning_of_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("beginning-of-buffer", &args, 1)?;
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    if args.first().is_some_and(|arg| !arg.is_nil()) {
        buf.goto_char(buf.zv);
    } else {
        buf.goto_char(buf.begv);
    }
    Ok(Value::Nil)
}

/// (end-of-buffer &optional ARG)
///
/// ARG is accepted for arity compatibility; point moves to point-max.
pub(crate) fn builtin_end_of_buffer(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("end-of-buffer", &args, 1)?;
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    buf.goto_char(buf.zv);
    Ok(Value::Nil)
}

/// (goto-line LINE)
pub(crate) fn builtin_goto_line(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("goto-line", &args, 1)?;
    let line = expect_int(&args[0])?;
    if line < 1 {
        return Err(signal(
            "args-out-of-range",
            vec![Value::string("Line number must be >= 1"), Value::Int(line)],
        ));
    }
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let text = buffer_text(buf);
    // Go to line 1 (beginning of buffer), then move forward (line-1) lines.
    let (new_pos, _) = move_by_lines(&text, 0, line - 1);
    buf.goto_char(new_pos);
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
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let cur_char = buf.point_char();
    let total_chars = buf.text.char_count();
    let new_char = if n >= 0 {
        let nc = cur_char.saturating_add(n as usize);
        nc.min(total_chars)
    } else {
        let abs_n = (-n) as usize;
        cur_char.saturating_sub(abs_n)
    };
    let new_byte = buf.text.char_to_byte(new_char);
    buf.goto_char(new_byte);
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
    let n = if args.is_empty() || args[0].is_nil() {
        1
    } else {
        expect_int(&args[0])?
    };
    // backward-char N == forward-char (- N)
    builtin_forward_char(eval, vec![Value::Int(-n)])
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
    expect_min_args("skip-chars-forward", &args, 1)?;
    let set_str = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).clone()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let lim_byte = if args.len() > 1 && !args[1].is_nil() {
        char_pos_to_byte(buf, expect_int(&args[1])?)
    } else {
        buf.zv
    };

    let (negate, char_set) = parse_skip_chars_set(&set_str);
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
    buf.goto_char(pos);
    let moved_chars = buf.text.byte_to_char(pos) as i64 - buf.text.byte_to_char(start_pos) as i64;
    Ok(Value::Int(moved_chars))
}

/// (skip-chars-backward STRING &optional LIM)
pub(crate) fn builtin_skip_chars_backward(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("skip-chars-backward", &args, 1)?;
    let set_str = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).clone()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let lim_byte = if args.len() > 1 && !args[1].is_nil() {
        char_pos_to_byte(buf, expect_int(&args[1])?)
    } else {
        buf.begv
    };

    let (negate, char_set) = parse_skip_chars_set(&set_str);
    let _text = buffer_text(buf);
    let start_pos = buf.pt;
    let mut pos = buf.pt;
    let limit = lim_byte;

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
    buf.goto_char(pos);
    let moved_chars = buf.text.byte_to_char(pos) as i64 - buf.text.byte_to_char(start_pos) as i64;
    Ok(Value::Int(moved_chars))
}

// ===========================================================================
// Mark and region
// ===========================================================================

/// (push-mark &optional LOCATION NOMSG ACTIVATE)
pub(crate) fn builtin_push_mark(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let byte_pos = if args.is_empty() || args[0].is_nil() {
        buf.pt
    } else {
        char_pos_to_byte(buf, expect_int(&args[0])?)
    };
    // Store current mark on the mark ring (buffer-local property "mark-ring")
    if let Some(old_mark) = buf.mark {
        let old_char = buf.text.byte_to_char(old_mark) as i64 + 1;
        let ring = buf
            .properties
            .entry("mark-ring".to_string())
            .or_insert(Value::Nil);
        // Prepend old mark to the ring list
        *ring = Value::cons(Value::Int(old_char), *ring);
    }
    buf.set_mark(byte_pos);
    let _nomsg = args.get(1).is_some_and(|v| v.is_truthy());
    let activate = args.get(2).is_some_and(|v| v.is_truthy());
    if activate {
        buf.properties
            .insert("mark-active".to_string(), Value::True);
    }
    Ok(Value::Nil)
}

/// (pop-mark)
pub(crate) fn builtin_pop_mark(eval: &mut super::eval::Evaluator, _args: Vec<Value>) -> EvalResult {
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let ring = buf
        .properties
        .get("mark-ring")
        .cloned()
        .unwrap_or(Value::Nil);
    match &ring {
        Value::Cons(cell) => {
            let pair = read_cons(*cell);
            let pos_val = pair.car;
            let rest = pair.cdr;
            drop(pair);
            buf.properties.insert("mark-ring".to_string(), rest);
            if let Some(pos) = pos_val.as_int() {
                let byte_pos = char_pos_to_byte(buf, pos);
                buf.set_mark(byte_pos);
            }
            Ok(Value::Nil)
        }
        _ => {
            // Empty mark ring — just clear the mark
            buf.mark = None;
            Ok(Value::Nil)
        }
    }
}

/// (set-mark POS) — set mark at pos and activate
/// Note: this overrides the existing builtin_set_mark in builtins.rs with
/// additional mark-active behavior.  We keep it here but the dispatch will
/// point to the one in builtins.rs unless re-routed.
pub(crate) fn builtin_set_mark_nav(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-mark", &args, 1)?;
    let pos = expect_int(&args[0])? as usize;
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let char_pos = if pos > 0 { pos - 1 } else { 0 };
    let byte_pos = buf.text.char_to_byte(char_pos.min(buf.text.char_count()));
    buf.set_mark(byte_pos);
    buf.properties
        .insert("mark-active".to_string(), Value::True);
    Ok(args[0])
}

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
    Ok(Value::Int(byte_to_char_pos(buf, start)))
}

/// (region-end) -> integer
pub(crate) fn builtin_region_end(
    eval: &mut super::eval::Evaluator,
    _args: Vec<Value>,
) -> EvalResult {
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
    Ok(Value::Int(byte_to_char_pos(buf, end)))
}

/// (use-region-p) -> t or nil
pub(crate) fn builtin_use_region_p(
    eval: &mut super::eval::Evaluator,
    _args: Vec<Value>,
) -> EvalResult {
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    let mark_active = match dynamic_or_global_symbol_value(eval, "mark-active") {
        Some(v) => v.is_truthy(),
        None => buf
            .properties
            .get("mark-active")
            .is_some_and(|v| v.is_truthy()),
    };
    let transient_mark_mode =
        dynamic_or_global_symbol_value(eval, "transient-mark-mode").is_some_and(|v| v.is_truthy());
    let non_empty_region = buf.mark().is_some_and(|m| m != buf.point());
    Ok(Value::bool(
        mark_active && transient_mark_mode && non_empty_region,
    ))
}

/// (region-active-p) -> t or nil
pub(crate) fn builtin_region_active_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("region-active-p", &args, 0)?;
    let buf = eval.buffers.current_buffer().ok_or_else(no_buffer)?;
    let mark_active = match dynamic_or_global_symbol_value(eval, "mark-active") {
        Some(v) => v.is_truthy(),
        None => buf
            .properties
            .get("mark-active")
            .is_some_and(|v| v.is_truthy()),
    };
    let transient_mark_mode =
        dynamic_or_global_symbol_value(eval, "transient-mark-mode").is_some_and(|v| v.is_truthy());
    Ok(Value::bool(
        mark_active && transient_mark_mode && buf.mark().is_some(),
    ))
}

/// (deactivate-mark &optional FORCE)
pub(crate) fn builtin_deactivate_mark(
    eval: &mut super::eval::Evaluator,
    _args: Vec<Value>,
) -> EvalResult {
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    buf.properties.insert("mark-active".to_string(), Value::Nil);
    Ok(Value::Nil)
}

/// (activate-mark &optional FORCE)
pub(crate) fn builtin_activate_mark(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.len() > 1 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("activate-mark"),
                Value::Int(args.len() as i64),
            ],
        ));
    }
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    if buf.mark().is_some() {
        buf.properties
            .insert("mark-active".to_string(), Value::True);
    }
    Ok(Value::Nil)
}

/// (exchange-point-and-mark &optional ARG)
pub(crate) fn builtin_exchange_point_and_mark(
    eval: &mut super::eval::Evaluator,
    _args: Vec<Value>,
) -> EvalResult {
    let buf = eval.buffers.current_buffer_mut().ok_or_else(no_buffer)?;
    let mark = buf.mark().ok_or_else(|| {
        signal(
            "user-error",
            vec![Value::string("No mark set in this buffer")],
        )
    })?;
    let old_pt = buf.pt;
    buf.pt = mark;
    buf.mark = Some(old_pt);
    // Activate the mark
    buf.properties
        .insert("mark-active".to_string(), Value::True);
    Ok(Value::Nil)
}

/// (transient-mark-mode &optional ARG) — toggle/query mode flag.
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

    let arg = args.first().unwrap_or(&Value::Nil);
    let numeric = match arg {
        Value::Nil => 1,
        Value::Int(n) => *n,
        Value::Float(f, _) => *f as i64,
        Value::Char(c) => *c as i64,
        _ => 1,
    };

    let val = if numeric > 0 { Value::True } else { Value::Nil };
    eval.obarray.set_symbol_value("transient-mark-mode", val);
    Ok(val)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "navigation_test.rs"]
mod tests;
