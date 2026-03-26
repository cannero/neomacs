//! Indentation builtins for the Elisp interpreter.
//!
//! Implements stub versions of Emacs indentation primitives:
//! - `current-indentation`, `indent-to`, `current-column`, `move-to-column`
//! - `indent-line-to`, `indent-rigidly`, `newline-and-indent`,
//!   `tab-to-tab-stop`, `delete-indentation`
//!
//! Variables: `tab-width`, `indent-tabs-mode`, `standard-indent`, `tab-stop-list`

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::symbol::Obarray;
use super::value::*;
use crate::buffer::{Buffer, BufferManager};

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

fn expect_fixnump(val: &Value) -> Result<i64, Flow> {
    match val {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *other],
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

fn dynamic_buffer_or_global_symbol_value(
    obarray: &Obarray,
    _dynamic: &[OrderedRuntimeBindingMap],
    buf: Option<&Buffer>,
    name: &str,
) -> Option<Value> {
    if let Some(buf) = buf
        && let Some(value) = buf.get_buffer_local(name)
    {
        return Some(*value);
    }
    obarray.symbol_value(name).copied()
}

fn tab_width_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buf: Option<&Buffer>,
) -> usize {
    match dynamic_buffer_or_global_symbol_value(obarray, dynamic, buf, "tab-width") {
        Some(Value::Int(n)) if n > 0 => n as usize,
        Some(Value::Char(c)) if (c as u32) > 0 => c as usize,
        _ => 8,
    }
}

fn indent_tabs_mode_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buf: Option<&Buffer>,
) -> bool {
    dynamic_buffer_or_global_symbol_value(obarray, dynamic, buf, "indent-tabs-mode")
        .is_none_or(|value| value.is_truthy())
}

fn buffer_read_only_active_in_state(
    obarray: &Obarray,
    dynamic: &[OrderedRuntimeBindingMap],
    buf: &Buffer,
) -> bool {
    if buf.read_only {
        return true;
    }
    dynamic_buffer_or_global_symbol_value(obarray, dynamic, Some(buf), "buffer-read-only")
        .is_some_and(|value| value.is_truthy())
}

fn buffer_read_only_active(eval: &super::eval::Context, buf: &Buffer) -> bool {
    buffer_read_only_active_in_state(&eval.obarray, &[], buf)
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

#[inline]
fn is_horizontal_space(ch: char) -> bool {
    ch == ' ' || ch == '\t'
}

fn delete_horizontal_space_at_point(
    eval: &mut super::eval::Context,
    backward_only: bool,
) -> Result<(), Flow> {
    let buf = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let pmin = buf.point_min();
    let pmax = buf.point_max();
    let pt = buf.point();

    let mut left = pt;
    while left > pmin {
        let Some(ch) = buf.char_before(left) else {
            break;
        };
        if !is_horizontal_space(ch) {
            break;
        }
        left -= ch.len_utf8();
    }

    let mut right = pt;
    if !backward_only {
        while right < pmax {
            let Some(ch) = buf.char_after(right) else {
                break;
            };
            if !is_horizontal_space(ch) {
                break;
            }
            right += ch.len_utf8();
        }
    }

    if left == right {
        return Ok(());
    }

    if buffer_read_only_active(eval, buf) {
        return Err(signal(
            "buffer-read-only",
            vec![Value::string(buf.name.clone())],
        ));
    }

    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval.buffers.delete_buffer_region(current_id, left, right);
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared-runtime indentation builtins
// ---------------------------------------------------------------------------

/// (current-indentation) -> integer
///
/// Return indentation columns for the current line.
pub(crate) fn builtin_current_indentation_eval(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-indentation", &args, 0)?;
    let Some(buf) = &ctx.buffers.current_buffer() else {
        return Ok(Value::Int(0));
    };

    let tabw = tab_width_in_state(&ctx.obarray, &[], Some(buf));
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
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-column", &args, 0)?;
    let Some(buf) = &ctx.buffers.current_buffer() else {
        return Ok(Value::Int(0));
    };

    let tabw = tab_width_in_state(&ctx.obarray, &[], Some(buf));
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
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("move-to-column", &args, 1)?;
    expect_max_args("move-to-column", &args, 2)?;
    let target = expect_wholenump(&args[0])?;
    let force = args.get(1).is_some_and(|v| v.is_truthy());
    let Some(current_id) = ctx.buffers.current_buffer_id() else {
        return Ok(Value::Int(0));
    };
    let Some(buf) = ctx.buffers.get(current_id) else {
        return Ok(Value::Int(0));
    };
    let tabw = tab_width_in_state(&ctx.obarray, &[], Some(buf));
    let read_only = buffer_read_only_active_in_state(&ctx.obarray, &[], buf);
    let text = buf.text.to_string();
    let pt = buf.pt.clamp(buf.begv, buf.zv);
    let (bol, eol) = line_bounds(&text, buf.begv, buf.zv, pt);
    let line = &text[bol..eol];
    let buffer_name = buf.name.clone();

    if target == 0 {
        let _ = ctx.buffers.goto_buffer_byte(current_id, bol);
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
        let _ = ctx.buffers.goto_buffer_byte(current_id, tab_byte);
        let pad = padding_to_column(col_before_tab, target, tabw);
        let _ = ctx.buffers.insert_into_buffer(current_id, &pad);
        return Ok(Value::Int(target as i64));
    }

    let _ = ctx.buffers.goto_buffer_byte(current_id, dest_byte);

    if force && reached < target {
        if read_only {
            return Err(signal("buffer-read-only", vec![Value::string(buffer_name)]));
        }
        let pad = padding_to_column(reached, target, tabw);
        let _ = ctx.buffers.insert_into_buffer(current_id, &pad);
        reached = target;
    }

    Ok(Value::Int(reached as i64))
}

/// (indent-to COLUMN &optional MINIMUM) -> COLUMN
///
/// GNU Emacs `Findent_to` primitive from `src/indent.c`.
pub(crate) fn builtin_indent_to_eval(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("indent-to", &args, 1)?;
    expect_max_args("indent-to", &args, 2)?;
    let column = expect_fixnump(&args[0])?.max(0) as usize;
    let minimum = if args.len() > 1 && !args[1].is_nil() {
        expect_fixnump(&args[1])?.max(0) as usize
    } else {
        0
    };

    let current_id = ctx.buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = ctx.buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let pt = buf.point();
    let pmin = buf.point_min();
    let text_before = buf.buffer_substring(pmin, pt);
    let line_start = text_before.rfind('\n').map(|pos| pos + 1).unwrap_or(0);
    let line_prefix = &text_before[line_start..];
    let tab_width = tab_width_in_state(&ctx.obarray, &[], Some(buf));

    let mut fromcol = 0usize;
    for ch in line_prefix.chars() {
        fromcol = next_column(fromcol, ch, tab_width);
    }

    let mincol = column.max(fromcol + minimum);
    if fromcol >= mincol {
        return Ok(Value::Int(mincol as i64));
    }

    if buffer_read_only_active_in_state(&ctx.obarray, &[], buf) {
        return Err(signal(
            "buffer-read-only",
            vec![Value::string(buf.name.clone())],
        ));
    }

    let use_tabs = indent_tabs_mode_in_state(&ctx.obarray, &[], Some(buf));

    let mut indent = String::new();
    let mut col = fromcol;

    if use_tabs {
        let tab = tab_width.max(1);
        while col < mincol {
            let next_tab = col + (tab - (col % tab));
            if next_tab <= mincol {
                indent.push('\t');
                col = next_tab;
            } else {
                break;
            }
        }
    }

    while col < mincol {
        indent.push(' ');
        col += 1;
    }

    let _ = ctx.buffers.insert_into_buffer(current_id, &indent);

    Ok(Value::Int(mincol as i64))
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
    obarray.set_symbol_value("tab-width", Value::Int(8));
    obarray.make_special("tab-width");

    // indent-tabs-mode: default t
    obarray.set_symbol_value("indent-tabs-mode", Value::True);
    obarray.make_special("indent-tabs-mode");

    // standard-indent: default 4
    obarray.set_symbol_value("standard-indent", Value::Int(4));
    obarray.make_special("standard-indent");

    // tab-stop-list: default nil
    obarray.set_symbol_value("tab-stop-list", Value::Nil);
    obarray.make_special("tab-stop-list");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "indent_test.rs"]
mod tests;
