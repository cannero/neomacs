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

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(value.as_str().unwrap().to_string()),
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

fn rectangle_strings_to_value(rectangle: &[String]) -> Value {
    Value::list(rectangle.iter().map(|s| Value::string(s.clone())).collect())
}

fn rectangle_strings_from_value(value: &Value) -> Result<Vec<String>, Flow> {
    let items = list_to_vec(value)
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), *value]))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item.kind() {
            ValueKind::String => out.push(item.as_str().unwrap().to_string()),
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
    pub killed: Vec<String>,
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

fn line_col_for_char_index(text: &str, target: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut col = 0usize;
    for (idx, ch) in text.chars().enumerate() {
        if idx == target {
            return (line, col);
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn extract_line_columns(line: &str, start_col: usize, end_col: usize) -> String {
    if start_col >= end_col {
        return String::new();
    }
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();

    if start_col >= len {
        return " ".repeat(end_col - start_col);
    }

    let mut out: String = chars[start_col..len.min(end_col)].iter().collect();
    if end_col > len {
        out.push_str(&" ".repeat(end_col - len));
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

fn char_index_to_byte(s: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    s.char_indices()
        .nth(char_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

fn line_col_to_char_index(text: &str, line: usize, col: usize) -> usize {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut pos = 0usize;
    for idx in 0..line {
        pos += lines.get(idx).copied().unwrap_or("").chars().count();
        pos += 1; // newline separator
    }
    let line_len = lines.get(line).copied().unwrap_or("").chars().count();
    pos + col.min(line_len)
}

fn extract_rectangle_from_text(
    text: &str,
    start_line: usize,
    end_line: usize,
    left_col: usize,
    right_col: usize,
) -> Vec<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    for line_index in rectangle_lines_for_extract(start_line, end_line) {
        let line = lines.get(line_index).copied().unwrap_or("");
        out.push(extract_line_columns(line, left_col, right_col));
    }
    out
}

fn delete_extract_rectangle_from_text(
    text: &str,
    start_line: usize,
    end_line: usize,
    left_col: usize,
    right_col: usize,
) -> (Vec<String>, String) {
    let mut lines: Vec<String> = text.split('\n').map(ToString::to_string).collect();
    let mut extracted = Vec::new();
    let width = right_col.saturating_sub(left_col);

    for line_index in rectangle_lines_for_extract(start_line, end_line) {
        let Some(line) = lines.get_mut(line_index) else {
            extracted.push(" ".repeat(width));
            continue;
        };

        let line_len = line.chars().count();
        if line_len < left_col {
            extracted.push(" ".repeat(width));
            continue;
        }

        extracted.push(extract_line_columns(line, left_col, right_col));
        let del_end_char = line_len.min(right_col);
        let del_start_byte = char_index_to_byte(line, left_col);
        let del_end_byte = char_index_to_byte(line, del_end_char);
        if del_start_byte < del_end_byte {
            line.replace_range(del_start_byte..del_end_byte, "");
        }
    }

    (extracted, lines.join("\n"))
}

fn clamped_rect_inputs(
    eval: &super::eval::Context,
    start: i64,
    end: i64,
) -> Option<(
    String,
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
    let text = {
        let string = buf.buffer_substring_lisp_string(pmin, pmax);
        super::builtins::runtime_string_from_lisp_string(&string)
    };

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
        let chars: Vec<char> = line.chars().collect();
        lo = lo.min(chars.len());
        hi = hi.min(chars.len());
        if lo >= hi {
            return Ok(Value::string(""));
        }
        let slice: String = chars[lo..hi].iter().collect();
        return Ok(Value::string(slice));
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
            let _ = eval.buffers.insert_into_buffer(current_id, &rewritten);
        }
        let new_end = pmin + rewritten.len();
        super::editfns::signal_after_change(eval, pmin, new_end, old_len)?;
    }

    Ok(Value::list(
        extracted.into_iter().map(Value::string).collect(),
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "rect_test.rs"]
mod tests;
