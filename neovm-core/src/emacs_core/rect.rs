//! Rectangle operation builtins for the Elisp interpreter.
//!
//! Implements rectangle manipulation commands:
//! - `extract-rectangle-line`
//! - `extract-rectangle`, `delete-rectangle`, `kill-rectangle`
//! - `yank-rectangle`, `insert-rectangle`, `open-rectangle`
//! - `clear-rectangle`, `string-rectangle`, `replace-rectangle`
//! - `delete-extract-rectangle`
//!
//! These implement compatibility-focused rectangle behavior used by
//! vm-compat batches. Remaining edge drift is tracked and locked by
//! oracle corpora.

use super::error::{EvalResult, Flow, signal};
use super::intern::intern;
use super::value::*;
use crate::emacs_core::value::ValueKind;
use crate::heap_types::LispString;

// ---------------------------------------------------------------------------
// Argument helpers (local copies — same pattern as other modules)
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
            vec![Value::symbol("integerp"), *value],
        )),
    }
}
#[cfg(test)]
fn expect_string(value: &Value) -> Result<LispString, Flow> {
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

/// Route symbol-value reads through the full GNU lookup path so
/// LOCALIZED BLV / FORWARDED slot / specpdl let-binding state is
/// observed. See the extended comment on the identical helper in
/// `builtins/misc_eval.rs` (audit finding #3 in
/// `drafts/regex-search-audit.md`).
fn dynamic_or_global_symbol_value(eval: &super::eval::Context, name: &str) -> Option<Value> {
    let id = crate::emacs_core::intern::intern(name);
    eval.eval_symbol_by_id(id).ok()
}

fn rectangle_strings_to_value(rectangle: &[LispString]) -> Value {
    Value::list(rectangle.iter().cloned().map(Value::heap_string).collect())
}

fn rectangle_strings_from_value(value: &Value) -> Result<Vec<LispString>, Flow> {
    let items = list_to_vec(value)
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), *value]))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item.kind() {
            ValueKind::String => out.push(
                item.as_lisp_string()
                    .expect("ValueKind::String must carry LispString payload")
                    .clone(),
            ),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("buffer-or-string-p"), item],
                ));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// RectangleState — stores the last killed rectangle
// ---------------------------------------------------------------------------

/// Persistent state for rectangle operations across the session.
#[derive(Clone, Debug)]
pub(crate) struct RectangleState {
    /// The last killed rectangle: one string per line.
    pub killed: Vec<LispString>,
}

impl RectangleState {
    pub fn new() -> Self {
        Self { killed: Vec::new() }
    }
}

impl Default for RectangleState {
    fn default() -> Self {
        Self::new()
    }
}

fn empty_lisp_string(multibyte: bool) -> LispString {
    super::builtins::lisp_string_from_buffer_bytes(Vec::new(), multibyte)
}

fn space_lisp_string(width: usize, multibyte: bool) -> LispString {
    super::builtins::lisp_string_from_buffer_bytes(vec![b' '; width], multibyte)
}

fn slice_lisp_string_chars(string: &LispString, start: usize, end: usize) -> LispString {
    let start = start.min(string.schars());
    let end = end.min(string.schars());
    if start >= end {
        return empty_lisp_string(string.is_multibyte());
    }

    if !string.is_multibyte() {
        return LispString::from_unibyte(string.as_bytes()[start..end].to_vec());
    }

    let start_byte = crate::emacs_core::emacs_char::char_to_byte_pos(string.as_bytes(), start);
    let end_byte = crate::emacs_core::emacs_char::char_to_byte_pos(string.as_bytes(), end);
    LispString::from_emacs_bytes(string.as_bytes()[start_byte..end_byte].to_vec())
}

fn split_lisp_string_lines(text: &LispString) -> Vec<LispString> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (idx, &byte) in text.as_bytes().iter().enumerate() {
        if byte == b'\n' {
            lines.push(
                text.slice(start, idx)
                    .expect("newline split must stay within string bounds"),
            );
            start = idx + 1;
        }
    }
    lines.push(
        text.slice(start, text.sbytes())
            .expect("line tail split must stay within string bounds"),
    );
    lines
}

fn join_lisp_string_lines(lines: &[LispString], multibyte: bool) -> LispString {
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx > 0 {
            out.push(b'\n');
        }
        out.extend_from_slice(line.as_bytes());
    }
    super::builtins::lisp_string_from_buffer_bytes(out, multibyte)
}

fn line_col_for_char_index(text: &LispString, target: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut col = 0usize;
    for (idx, ch) in super::builtins::lisp_string_char_codes(text)
        .into_iter()
        .enumerate()
    {
        if idx == target {
            return (line, col);
        }
        if ch == b'\n' as u32 {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn extract_line_columns(line: &LispString, start_col: usize, end_col: usize) -> LispString {
    if start_col >= end_col {
        return empty_lisp_string(line.is_multibyte());
    }
    let len = line.schars();

    if start_col >= len {
        return space_lisp_string(end_col - start_col, line.is_multibyte());
    }

    let mut out = slice_lisp_string_chars(line, start_col, len.min(end_col));
    if end_col > len {
        out.data_mut()
            .extend(std::iter::repeat_n(b' ', end_col - len));
        out.recompute_size();
    }
    out
}

fn rectangle_lines_for_extract(start_line: usize, end_line: usize) -> Vec<usize> {
    if start_line <= end_line {
        (start_line..=end_line).collect()
    } else {
        vec![start_line]
    }
}

fn line_col_to_char_index(text: &LispString, line: usize, col: usize) -> usize {
    let lines = split_lisp_string_lines(text);
    let mut pos = 0usize;
    for idx in 0..line {
        pos += lines.get(idx).map(LispString::schars).unwrap_or(0);
        pos += 1; // newline separator
    }
    let line_len = lines.get(line).map(LispString::schars).unwrap_or(0);
    pos + col.min(line_len)
}

fn extract_rectangle_from_text(
    text: &LispString,
    start_line: usize,
    end_line: usize,
    left_col: usize,
    right_col: usize,
) -> Vec<LispString> {
    let lines = split_lisp_string_lines(text);
    let mut out = Vec::new();
    for line_index in rectangle_lines_for_extract(start_line, end_line) {
        if let Some(line) = lines.get(line_index) {
            out.push(extract_line_columns(line, left_col, right_col));
        } else {
            out.push(space_lisp_string(
                right_col.saturating_sub(left_col),
                text.is_multibyte(),
            ));
        }
    }
    out
}

fn delete_extract_rectangle_from_text(
    text: &LispString,
    start_line: usize,
    end_line: usize,
    left_col: usize,
    right_col: usize,
) -> (Vec<LispString>, LispString) {
    let mut lines = split_lisp_string_lines(text);
    let mut extracted = Vec::new();
    let width = right_col.saturating_sub(left_col);

    for line_index in rectangle_lines_for_extract(start_line, end_line) {
        let Some(line) = lines.get_mut(line_index) else {
            extracted.push(space_lisp_string(width, text.is_multibyte()));
            continue;
        };

        let line_len = line.schars();
        if line_len < left_col {
            extracted.push(space_lisp_string(width, line.is_multibyte()));
            continue;
        }

        extracted.push(extract_line_columns(line, left_col, right_col));
        let del_end_char = line_len.min(right_col);
        let del_start_byte = if line.is_multibyte() {
            crate::emacs_core::emacs_char::char_to_byte_pos(line.as_bytes(), left_col)
        } else {
            left_col
        };
        let del_end_byte = if line.is_multibyte() {
            crate::emacs_core::emacs_char::char_to_byte_pos(line.as_bytes(), del_end_char)
        } else {
            del_end_char
        };
        if del_start_byte < del_end_byte {
            line.data_mut().drain(del_start_byte..del_end_byte);
            line.recompute_size();
        }
    }

    (
        extracted,
        join_lisp_string_lines(&lines, text.is_multibyte()),
    )
}

fn clamped_rect_inputs(
    eval: &super::eval::Context,
    start: i64,
    end: i64,
) -> Option<(
    LispString,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
)> {
    let buf = eval.buffers.current_buffer()?;
    let point_min_char = buf.point_min_char() as i64 + 1;
    let point_max_char = buf.point_max_char() as i64 + 1;
    let clamped_start = start.clamp(point_min_char, point_max_char);
    let clamped_end = end.clamp(point_min_char, point_max_char);
    let pmin = buf.point_min();
    let pmax = buf.point_max();
    let text = buf.buffer_substring_lisp_string(pmin, pmax);

    let rel_start = (clamped_start - point_min_char).max(0) as usize;
    let rel_end = (clamped_end - point_min_char).max(0) as usize;
    let (start_line, start_col) = line_col_for_char_index(&text, rel_start);
    let (end_line, end_col) = line_col_for_char_index(&text, rel_end);
    let (left_col, right_col) = if start_col <= end_col {
        (start_col, end_col)
    } else {
        (end_col, start_col)
    };
    Some((
        text, pmin, pmax, start_line, start_col, end_line, end_col, left_col, right_col,
    ))
}

// ---------------------------------------------------------------------------
// Eval-dependent builtins
// ---------------------------------------------------------------------------

/// `(extract-rectangle-line STARTCOL ENDCOL &optional LINE)` -- extract one
/// line of a rectangle as a string.
///
/// Compatibility behavior:
/// - with optional LINE, returns substring between STARTCOL and ENDCOL
/// - without LINE, returns an empty string (legacy stub path)
#[cfg(test)]
pub(crate) fn builtin_extract_rectangle_line(args: Vec<Value>) -> EvalResult {
    expect_min_args("extract-rectangle-line", &args, 2)?;
    expect_max_args("extract-rectangle-line", &args, 3)?;
    let start_col = expect_int(&args[0])?;
    let end_col = expect_int(&args[1])?;
    if start_col < 0 || end_col < 0 {
        return Err(signal(
            "args-out-of-range",
            vec![Value::fixnum(start_col), Value::fixnum(end_col)],
        ));
    }
    if args.len() == 3 {
        let line = expect_string(&args[2])?;
        let mut lo = start_col as usize;
        let mut hi = end_col as usize;
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
        }
        lo = lo.min(line.schars());
        hi = hi.min(line.schars());
        if lo >= hi {
            return Ok(Value::heap_string(empty_lisp_string(line.is_multibyte())));
        }
        return Ok(Value::heap_string(slice_lisp_string_chars(&line, lo, hi)));
    }
    Ok(Value::string(""))
}

/// `(clear-rectangle START END &optional FILL)` -- replace the rectangle
/// contents with spaces (or FILL character if given).
///
/// Compatibility behavior:
/// - fills rectangle width with spaces, then trims trailing spaces in affected
///   lines
/// `(delete-extract-rectangle START END)` -- delete the rectangle and
/// return its contents as a list of strings.
///
/// Compatibility behavior:
/// - extracts rectangle text using the same START/END column/line model as
///   `extract-rectangle`
/// - deletes extracted text from each affected line
/// - when rectangle starts past EOL, returns width spaces and leaves line
///   unchanged
fn delete_extract_rectangle_eval(
    eval: &mut super::eval::Context,
    start: i64,
    end: i64,
) -> EvalResult {
    let Some((text, pmin, pmax, start_line, _start_col, end_line, _end_col, left_col, right_col)) =
        clamped_rect_inputs(eval, start, end)
    else {
        return Ok(Value::list(Vec::new()));
    };

    let (extracted, rewritten) =
        delete_extract_rectangle_from_text(&text, start_line, end_line, left_col, right_col);

    if let Some(_current_id) = eval.buffers.current_buffer_id() {
        super::editfns::signal_before_change(eval, pmin, pmax)?;
        let old_len = super::editfns::current_buffer_byte_span_char_len(eval, pmin, pmax);
        if let Some(current_id) = eval.buffers.current_buffer_id() {
            let _ = eval.buffers.delete_buffer_region(current_id, pmin, pmax);
            let _ = eval.buffers.goto_buffer_byte(current_id, pmin);
            let _ = eval
                .buffers
                .insert_lisp_string_into_buffer(current_id, &rewritten);
        }
        let new_end = pmin + rewritten.sbytes();
        super::editfns::signal_after_change(eval, pmin, new_end, old_len)?;
    }

    Ok(rectangle_strings_to_value(&extracted))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "rect_test.rs"]
mod tests;
