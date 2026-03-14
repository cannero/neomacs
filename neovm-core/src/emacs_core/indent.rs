//! Indentation builtins for the Elisp interpreter.
//!
//! Implements stub versions of Emacs indentation primitives:
//! - `current-indentation`, `indent-to`, `current-column`, `move-to-column`
//! - `indent-region`, `indent-line-to`, `indent-rigidly`, `newline-and-indent`,
//!   `reindent-then-newline-and-indent`, `indent-for-tab-command`,
//!   `indent-according-to-mode`, `tab-to-tab-stop`, `back-to-indentation`,
//!   `delete-indentation`
//!
//! Variables: `tab-width`, `indent-tabs-mode`, `standard-indent`, `tab-stop-list`

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::value::*;
use crate::buffer::Buffer;

// ---------------------------------------------------------------------------
// Argument helpers (local to this module)
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

fn expect_int(val: &Value) -> Result<i64, Flow> {
    match val {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

fn expect_wholenump(val: &Value) -> Result<usize, Flow> {
    match val {
        Value::Int(n) if *n >= 0 => Ok(*n as usize),
        Value::Char(c) => Ok(*c as usize),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// (current-indentation) -> integer
///
/// Return the indentation of the current line (number of whitespace columns
/// at the beginning of the line).  Stub: always returns 0.
pub(crate) fn builtin_current_indentation(args: Vec<Value>) -> EvalResult {
    expect_args("current-indentation", &args, 0)?;
    Ok(Value::Int(0))
}

/// (indent-to COLUMN &optional MINIMUM) -> COLUMN
///
/// Indent from point with tabs and spaces until COLUMN is reached.
/// Optional second argument MINIMUM says always do at least MINIMUM spaces
/// even if that moves past COLUMN; default is zero.
/// Stub: returns COLUMN unchanged.
pub(crate) fn builtin_indent_to(args: Vec<Value>) -> EvalResult {
    expect_min_args("indent-to", &args, 1)?;
    expect_max_args("indent-to", &args, 2)?;
    let column = expect_int(&args[0])?;
    // Optional MINIMUM argument is accepted but ignored in this stub.
    Ok(Value::Int(column))
}

/// (current-column) -> integer
///
/// Return the horizontal position of point.  Beginning of line is column 0.
/// Stub: always returns 0.
pub(crate) fn builtin_current_column(args: Vec<Value>) -> EvalResult {
    expect_args("current-column", &args, 0)?;
    Ok(Value::Int(0))
}

/// (move-to-column COLUMN &optional FORCE) -> COLUMN
///
/// Move point to column COLUMN in the current line.
/// Stub: returns COLUMN unchanged.
pub(crate) fn builtin_move_to_column(args: Vec<Value>) -> EvalResult {
    expect_min_args("move-to-column", &args, 1)?;
    expect_max_args("move-to-column", &args, 2)?;
    let column = expect_int(&args[0])?;
    // Optional FORCE argument is accepted but ignored in this stub.
    Ok(Value::Int(column))
}

fn tab_width(eval: &super::eval::Evaluator) -> usize {
    match eval.obarray.symbol_value("tab-width") {
        Some(Value::Int(n)) if *n > 0 => *n as usize,
        Some(Value::Char(c)) if (*c as u32) > 0 => *c as usize,
        _ => 8,
    }
}

fn buffer_read_only_active(eval: &super::eval::Evaluator, buf: &Buffer) -> bool {
    if buf.read_only {
        return true;
    }

    let name_id = intern("buffer-read-only");
    for frame in eval.dynamic.iter().rev() {
        if let Some(value) = frame.get(&name_id) {
            return value.is_truthy();
        }
    }

    if let Some(value) = buf.get_buffer_local("buffer-read-only") {
        return value.is_truthy();
    }

    eval.obarray
        .symbol_value("buffer-read-only")
        .is_some_and(|value| value.is_truthy())
}

fn line_bounds(text: &str, begv: usize, zv: usize, point: usize) -> (usize, usize) {
    let bytes = text.as_bytes();
    let pt = point.clamp(begv, zv);

    let mut bol = pt;
    while bol > begv && bytes[bol - 1] != b'\n' {
        bol -= 1;
    }

    let mut eol = pt;
    while eol < zv && bytes[eol] != b'\n' {
        eol += 1;
    }

    (bol, eol)
}

fn next_column(column: usize, ch: char, tab_width: usize) -> usize {
    if ch == '\t' {
        let tab = tab_width.max(1);
        column + (tab - (column % tab))
    } else {
        column + crate::encoding::char_width(ch)
    }
}

fn column_for_prefix(prefix: &str, tab_width: usize) -> usize {
    let mut column = 0usize;
    for ch in prefix.chars() {
        column = next_column(column, ch, tab_width);
    }
    column
}

fn padding_to_column(mut column: usize, target: usize, tab_width: usize) -> String {
    let mut out = String::new();
    let tab = tab_width.max(1);
    while column < target {
        let next_tab = column + (tab - (column % tab));
        if next_tab <= target && next_tab > column + 1 {
            out.push('\t');
            column = next_tab;
        } else {
            out.push(' ');
            column += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Eval-dependent builtins
// ---------------------------------------------------------------------------

/// (current-indentation) -> integer
///
/// Return indentation columns for the current line.
pub(crate) fn builtin_current_indentation_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-indentation", &args, 0)?;
    let Some(buf) = eval.buffers.current_buffer() else {
        return Ok(Value::Int(0));
    };

    let tabw = tab_width(eval);
    let text = buf.text.to_string();
    let (bol, eol) = line_bounds(&text, buf.begv, buf.zv, buf.pt);
    let line = &text[bol..eol];

    let mut column = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' | '\t' => column = next_column(column, ch, tabw),
            _ => break,
        }
    }

    Ok(Value::Int(column as i64))
}

/// (current-column) -> integer
///
/// Return the display column at point on the current line.
pub(crate) fn builtin_current_column_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-column", &args, 0)?;
    let Some(buf) = eval.buffers.current_buffer() else {
        return Ok(Value::Int(0));
    };

    let tabw = tab_width(eval);
    let text = buf.text.to_string();
    let pt = buf.pt.clamp(buf.begv, buf.zv);
    let (bol, _) = line_bounds(&text, buf.begv, buf.zv, pt);
    let prefix = &text[bol..pt];

    Ok(Value::Int(column_for_prefix(prefix, tabw) as i64))
}

/// (move-to-column COLUMN &optional FORCE) -> COLUMN-REACHED
///
/// Move point on the current line according to display columns.
pub(crate) fn builtin_move_to_column_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("move-to-column", &args, 1)?;
    expect_max_args("move-to-column", &args, 2)?;
    let target = expect_wholenump(&args[0])?;
    let force = args.get(1).is_some_and(|v| v.is_truthy());
    let tabw = tab_width(eval);
    let read_only = eval
        .buffers
        .current_buffer()
        .is_some_and(|buf| buffer_read_only_active(eval, buf));
    let Some(current_id) = eval.buffers.current_buffer_id() else {
        return Ok(Value::Int(0));
    };
    let Some(buf) = eval.buffers.get(current_id) else {
        return Ok(Value::Int(0));
    };
    let text = buf.text.to_string();
    let pt = buf.pt.clamp(buf.begv, buf.zv);
    let (bol, eol) = line_bounds(&text, buf.begv, buf.zv, pt);
    let line = &text[bol..eol];
    let buffer_name = buf.name.clone();

    if target == 0 {
        let _ = eval.buffers.goto_buffer_byte(current_id, bol);
        return Ok(Value::Int(0));
    }

    let mut column = 0usize;
    let mut dest_byte = bol;
    let mut reached = 0usize;
    let mut found = false;
    let mut tab_split: Option<(usize, usize)> = None;

    for (rel, ch) in line.char_indices() {
        let char_start = bol + rel;
        let char_end = char_start + ch.len_utf8();
        let next = next_column(column, ch, tabw);
        if next >= target {
            if force && ch == '\t' && next > target {
                tab_split = Some((char_start, column));
            } else {
                dest_byte = char_end;
                reached = next;
            }
            found = true;
            break;
        }
        dest_byte = char_end;
        reached = next;
        column = next;
    }

    if !found {
        dest_byte = eol;
        reached = column_for_prefix(line, tabw);
    }

    if let Some((tab_byte, col_before_tab)) = tab_split {
        if read_only {
            return Err(signal(
                "buffer-read-only",
                vec![Value::string(buffer_name.clone())],
            ));
        }
        let _ = eval.buffers.goto_buffer_byte(current_id, tab_byte);
        let pad = padding_to_column(col_before_tab, target, tabw);
        let _ = eval.buffers.insert_into_buffer(current_id, &pad);
        return Ok(Value::Int(target as i64));
    }

    let _ = eval.buffers.goto_buffer_byte(current_id, dest_byte);

    if force && reached < target {
        if read_only {
            return Err(signal("buffer-read-only", vec![Value::string(buffer_name)]));
        }
        let pad = padding_to_column(reached, target, tabw);
        let _ = eval.buffers.insert_into_buffer(current_id, &pad);
        reached = target;
    }

    Ok(Value::Int(reached as i64))
}

/// (indent-region START END &optional COLUMN) -> nil
///
/// Indent each nonblank line in the region.
/// Stub: does nothing, returns nil.
pub(crate) fn builtin_indent_region(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("indent-region", &args, 2)?;
    expect_max_args("indent-region", &args, 3)?;
    let start = expect_int(&args[0])?;
    let end = expect_int(&args[1])?;
    let target_column = match args.get(2) {
        Some(Value::Int(n)) => Some((*n).max(0) as usize),
        Some(Value::Char(c)) => Some((*c as i64).max(0) as usize),
        _ => None,
    };

    let Some(buf) = eval.buffers.current_buffer() else {
        return Ok(Value::True);
    };
    if buffer_read_only_active(eval, buf) {
        return Err(signal(
            "buffer-read-only",
            vec![Value::string(buf.name.clone())],
        ));
    }

    let point_min = buf.text.byte_to_char(buf.point_min()) as i64 + 1;
    let point_max = buf.text.byte_to_char(buf.point_max()) as i64 + 1;
    let a = start.clamp(point_min, point_max);
    let b = end.clamp(point_min, point_max);
    if a >= b {
        return Ok(Value::True);
    }
    let region_start = buf.text.char_to_byte((a - 1).max(0) as usize);
    let region_end = buf.text.char_to_byte((b - 1).max(0) as usize);

    let text = buf.text.to_string();
    let region = &text[region_start..region_end];

    let mut rewritten = String::with_capacity(
        region
            .len()
            .saturating_add(target_column.unwrap_or(0).saturating_mul(4)),
    );
    for segment in region.split_inclusive('\n') {
        let (line, has_newline) = match segment.strip_suffix('\n') {
            Some(prefix) => (prefix, true),
            None => (segment, false),
        };

        if line.chars().all(|ch| ch == ' ' || ch == '\t') {
            rewritten.push_str(line);
        } else {
            let trimmed = line.trim_start_matches([' ', '\t']);
            if let Some(column) = target_column {
                rewritten.push_str(&" ".repeat(column));
            }
            rewritten.push_str(trimmed);
        }

        if has_newline {
            rewritten.push('\n');
        }
    }

    let Some(current_id) = eval.buffers.current_buffer_id() else {
        return Ok(Value::Nil);
    };
    let _ = eval
        .buffers
        .delete_buffer_region(current_id, region_start, region_end);
    let _ = eval.buffers.goto_buffer_byte(current_id, region_start);
    let _ = eval.buffers.insert_into_buffer(current_id, &rewritten);

    Ok(Value::True)
}

/// (reindent-then-newline-and-indent) -> nil
///
/// Reindent current line, insert newline, then indent the new line.
/// Stub: does nothing, returns nil.
pub(crate) fn builtin_reindent_then_newline_and_indent(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("reindent-then-newline-and-indent", &args, 0)?;
    builtin_indent_according_to_mode(eval, vec![])?;
    super::kill_ring::builtin_newline_and_indent(eval, vec![])?;
    Ok(Value::Nil)
}

/// (indent-for-tab-command &optional ARG) -> nil
///
/// Indent the current line or region, or insert a tab, as appropriate.
/// Current behavior: insert a tab character at point.
pub(crate) fn builtin_indent_for_tab_command(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("indent-for-tab-command", &args, 1)?;
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

    // When point is in horizontal whitespace, Emacs collapses it before tab insert.
    super::kill_ring::builtin_delete_horizontal_space(eval, vec![])?;

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.insert_into_buffer(current_id, "\t");
    Ok(Value::Nil)
}

/// (indent-according-to-mode) -> nil
///
/// Indent line in proper way for current major mode.
/// Stub: does nothing, returns nil.
pub(crate) fn builtin_indent_according_to_mode(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("indent-according-to-mode", &args, 1)?;

    let Some(buf) = eval.buffers.current_buffer() else {
        return Ok(Value::Nil);
    };
    let text = buf.text.to_string();
    let pt = buf.pt.clamp(buf.begv, buf.zv);
    let (bol, eol) = line_bounds(&text, buf.begv, buf.zv, pt);
    let line = &text[bol..eol];
    let indent_len = line
        .chars()
        .take_while(|ch| *ch == ' ' || *ch == '\t')
        .map(char::len_utf8)
        .sum::<usize>();
    if indent_len == 0 {
        return Ok(Value::Nil);
    }

    if buffer_read_only_active(eval, buf) {
        return Err(signal(
            "buffer-read-only",
            vec![Value::string(buf.name.clone())],
        ));
    }

    let indent_end = bol + indent_len;
    let new_pt = if pt <= bol {
        pt
    } else if pt <= indent_end {
        bol
    } else {
        pt - indent_len
    };

    let Some(current_id) = eval.buffers.current_buffer_id() else {
        return Ok(Value::Nil);
    };
    let _ = eval
        .buffers
        .delete_buffer_region(current_id, bol, indent_end);
    let _ = eval.buffers.goto_buffer_byte(current_id, new_pt);

    Ok(Value::Nil)
}

/// (back-to-indentation) -> nil
///
/// Move point to first non-space/tab on current line.
pub(crate) fn builtin_back_to_indentation(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("back-to-indentation", &args, 0)?;
    let Some(current_id) = eval.buffers.current_buffer_id() else {
        return Ok(Value::Nil);
    };
    let Some(buf) = eval.buffers.get(current_id) else {
        return Ok(Value::Nil);
    };
    let text = buf.text.to_string();
    let pt = buf.pt.clamp(buf.begv, buf.zv);
    let (bol, eol) = line_bounds(&text, buf.begv, buf.zv, pt);
    let line = &text[bol..eol];

    let mut dest = eol;
    for (rel, ch) in line.char_indices() {
        if ch != ' ' && ch != '\t' {
            dest = bol + rel;
            break;
        }
    }

    let _ = eval.buffers.goto_buffer_byte(current_id, dest);
    Ok(Value::Nil)
}

// ---------------------------------------------------------------------------
// Variable initialisation
// ---------------------------------------------------------------------------

/// Pre-populate the obarray with standard indentation variables.
///
/// Must be called during evaluator initialisation (after the obarray is created
/// but before any user code runs).
pub fn init_indent_vars(obarray: &mut super::symbol::Obarray) {
    // tab-width: default 8 (buffer-local in real Emacs, global default here)
    let sym = obarray.get_or_intern("tab-width");
    sym.value = Some(Value::Int(8));
    sym.special = true;

    // indent-tabs-mode: default t
    let sym = obarray.get_or_intern("indent-tabs-mode");
    sym.value = Some(Value::True);
    sym.special = true;

    // standard-indent: default 4
    let sym = obarray.get_or_intern("standard-indent");
    sym.value = Some(Value::Int(4));
    sym.special = true;

    // tab-stop-list: default nil
    let sym = obarray.get_or_intern("tab-stop-list");
    sym.value = Some(Value::Nil);
    sym.special = true;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "indent_test.rs"]
mod tests;
