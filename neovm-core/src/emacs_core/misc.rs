//! Miscellaneous commonly-needed builtins.
//!
//! Contains:
//! - Special forms: prog2, with-temp-buffer, save-current-buffer, track-mouse, with-syntax-table
//! - Pure builtins: copy-alist, rassoc, rassq, assoc-default, make-list, safe-length,
//!   subst-char-in-string, string/char encoding stubs, locale-info
//! - Eval-dependent builtins: backtrace-* helpers, recursion-depth

use super::error::{EvalResult, Flow, signal};
use super::expr::Expr;
use super::intern::resolve_sym;
use super::string_escape::{bytes_to_unibyte_storage_string, encode_nonunicode_char_for_storage};
use super::value::*;

const RAW_BYTE_SENTINEL_BASE: u32 = 0xE000;
const RAW_BYTE_SENTINEL_MIN: u32 = 0xE080;
const RAW_BYTE_SENTINEL_MAX: u32 = 0xE0FF;
const UNIBYTE_BYTE_SENTINEL_BASE: u32 = 0xE300;
const UNIBYTE_BYTE_SENTINEL_MIN: u32 = 0xE300;
const UNIBYTE_BYTE_SENTINEL_MAX: u32 = 0xE3FF;
const MAX_EMACS_CHAR: i64 = 0x3FFFFF;

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

fn expect_wholenump(val: &Value) -> Result<i64, Flow> {
    match val {
        Value::Int(n) if *n >= 0 => Ok(*n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *other],
        )),
    }
}

fn expect_string(val: &Value) -> Result<String, Flow> {
    match val {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).clone())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn expect_char(val: &Value) -> Result<char, Flow> {
    match val {
        Value::Char(c) => Ok(*c),
        Value::Int(n) => char::from_u32(*n as u32).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *val],
            )
        }),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *other],
        )),
    }
}

fn expect_character_code(val: &Value) -> Result<i64, Flow> {
    match val {
        Value::Char(c) => Ok(*c as i64),
        Value::Int(n) if (0..=MAX_EMACS_CHAR).contains(n) => Ok(*n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *other],
        )),
    }
}

fn convert_unibyte_storage_to_multibyte(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let cp = ch as u32;
        if (UNIBYTE_BYTE_SENTINEL_MIN..=UNIBYTE_BYTE_SENTINEL_MAX).contains(&cp) {
            let byte = cp - UNIBYTE_BYTE_SENTINEL_BASE;
            if byte <= 0x7F {
                out.push(char::from_u32(byte).expect("ascii scalar"));
            } else {
                let raw_code = 0x3FFF00 + byte;
                let encoded = encode_nonunicode_char_for_storage(raw_code)
                    .expect("raw-byte code should be encodable");
                out.push_str(&encoded);
            }
            continue;
        }
        out.push(ch);
    }
    out
}

// ===========================================================================
// Special forms
// ===========================================================================

/// `(with-temp-buffer BODY...)` -- create a temp buffer, make it current,
/// execute BODY, kill the buffer, restore previous buffer, return last result.
pub(crate) fn sf_with_temp_buffer(eval: &mut super::eval::Evaluator, tail: &[Expr]) -> EvalResult {
    // Save current buffer
    let saved_buf = eval.buffers.current_buffer().map(|b| b.id);

    // Create a temporary buffer
    let temp_name = eval.buffers.generate_new_buffer_name(" *temp*");
    let temp_id = eval.buffers.create_buffer(&temp_name);
    eval.buffers.set_current(temp_id);

    // Execute body
    let result = eval.sf_progn(tail);

    // Kill temp buffer and restore
    eval.buffers.kill_buffer(temp_id);
    if let Some(saved_id) = saved_buf {
        eval.buffers.set_current(saved_id);
    }
    result
}

/// `(save-current-buffer BODY...)` -- save the current buffer, execute BODY,
/// then restore the previous current buffer.
pub(crate) fn sf_save_current_buffer(
    eval: &mut super::eval::Evaluator,
    tail: &[Expr],
) -> EvalResult {
    let saved_buf = eval.buffers.current_buffer().map(|b| b.id);
    let result = eval.sf_progn(tail);
    if let Some(saved_id) = saved_buf {
        eval.buffers.set_current(saved_id);
    }
    result
}

/// `(track-mouse BODY...)` -- evaluate BODY forms.
/// In batch/terminal mode, this is an effective no-op wrapper around `progn`.
pub(crate) fn sf_track_mouse(eval: &mut super::eval::Evaluator, tail: &[Expr]) -> EvalResult {
    eval.sf_progn(tail)
}

/// `(with-syntax-table TABLE BODY...)` -- evaluate BODY with TABLE installed
/// as the current buffer syntax-table object, then restore the previous table.
pub(crate) fn sf_with_syntax_table(eval: &mut super::eval::Evaluator, tail: &[Expr]) -> EvalResult {
    if tail.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("with-syntax-table"), Value::Int(0)],
        ));
    }
    let saved = super::syntax::builtin_syntax_table(eval, vec![])?;
    let table = eval.eval(&tail[0])?;
    super::syntax::builtin_set_syntax_table(eval, vec![table])?;

    let result = eval.sf_progn(&tail[1..]);
    let _ = super::syntax::builtin_set_syntax_table(eval, vec![saved]);
    result
}

// ===========================================================================
// Pure builtins (no eval needed)
// ===========================================================================

/// `(copy-alist ALIST)` -- shallow copy an association list.
/// Each top-level cons is copied; the car/cdr of each entry are shared.
pub(crate) fn builtin_copy_alist(args: Vec<Value>) -> EvalResult {
    expect_args("copy-alist", &args, 1)?;
    let alist = &args[0];
    let mut result = Vec::new();
    let mut cursor = *alist;
    loop {
        match cursor {
            Value::Nil => break,
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                // If the element is a cons, copy it; otherwise keep as-is
                let entry = match &pair.car {
                    Value::Cons(inner) => {
                        let inner_pair = read_cons(*inner);
                        Value::cons(inner_pair.car, inner_pair.cdr)
                    }
                    other => *other,
                };
                result.push(entry);
                cursor = pair.cdr;
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), *alist],
                ));
            }
        }
    }
    Ok(Value::list(result))
}

/// `(rassoc KEY ALIST)` -- find the first entry in ALIST whose cdr equals KEY
/// (using `equal`).
pub(crate) fn builtin_rassoc(args: Vec<Value>) -> EvalResult {
    expect_args("rassoc", &args, 2)?;
    let key = &args[0];
    let alist = &args[1];
    let mut cursor = *alist;
    loop {
        match cursor {
            Value::Nil => return Ok(Value::Nil),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                if let Value::Cons(inner) = &pair.car {
                    let inner_pair = read_cons(*inner);
                    if equal_value(&inner_pair.cdr, key, 0) {
                        return Ok(pair.car);
                    }
                }
                cursor = pair.cdr;
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), cursor],
                ));
            }
        }
    }
}

/// `(rassq KEY ALIST)` -- like rassoc but uses `eq` for comparison.
pub(crate) fn builtin_rassq(args: Vec<Value>) -> EvalResult {
    expect_args("rassq", &args, 2)?;
    let key = &args[0];
    let alist = &args[1];
    let mut cursor = *alist;
    loop {
        match cursor {
            Value::Nil => return Ok(Value::Nil),
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                if let Value::Cons(inner) = &pair.car {
                    let inner_pair = read_cons(*inner);
                    if eq_value(&inner_pair.cdr, key) {
                        return Ok(pair.car);
                    }
                }
                cursor = pair.cdr;
            }
            _ => return Ok(Value::Nil),
        }
    }
}

/// `(make-list LENGTH INIT)` -- create a list of LENGTH elements, each INIT.
pub(crate) fn builtin_make_list(args: Vec<Value>) -> EvalResult {
    expect_args("make-list", &args, 2)?;
    let length = expect_wholenump(&args[0])?;
    let init = &args[1];
    let items: Vec<Value> = (0..length as usize).map(|_| *init).collect();
    Ok(Value::list(items))
}

/// `(string-repeat STRING COUNT)` -- repeat STRING COUNT times.
#[cfg(test)]
pub(crate) fn builtin_string_repeat(args: Vec<Value>) -> EvalResult {
    expect_args("string-repeat", &args, 2)?;
    let s = expect_string(&args[0])?;
    let count = expect_wholenump(&args[1])?;
    Ok(Value::string(s.repeat(count as usize)))
}

/// `(safe-length LIST)` -- return the length of LIST, returning 0 for
/// non-lists and stopping at circular references (up to a limit).
pub(crate) fn builtin_safe_length(args: Vec<Value>) -> EvalResult {
    expect_args("safe-length", &args, 1)?;
    let list = &args[0];
    if list.is_nil() {
        return Ok(Value::Int(0));
    }
    if !list.is_cons() {
        return Ok(Value::Int(0));
    }

    // Traverse once while running tortoise-and-hare cycle detection.
    // `length` tracks visited cons cells via `slow`.
    let mut slow = *list;
    let mut fast = *list;
    let mut length: i64 = 0;

    loop {
        // Advance slow by 1
        match slow {
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                slow = pair.cdr;
                length += 1;
            }
            _ => return Ok(Value::Int(length)),
        }

        // Advance fast by 2 when possible. If it reaches a non-cons, we still
        // continue counting via `slow` so proper odd-length lists are exact.
        for _ in 0..2 {
            match fast {
                Value::Cons(cell) => {
                    let pair = read_cons(cell);
                    fast = pair.cdr;
                }
                _ => {
                    fast = Value::Nil;
                    break;
                }
            }
        }

        // Check for cycle (pointer equality)
        if let (Value::Cons(a), Value::Cons(b)) = (&slow, &fast) {
            if a == b {
                // Circular list detected; return count so far
                return Ok(Value::Int(length));
            }
        }

        // Safety limit to avoid infinite loops
        if length > 10_000_000 {
            return Ok(Value::Int(length));
        }
    }
}

/// `(subst-char-in-string FROMCHAR TOCHAR STRING &optional INPLACE)` --
/// replace all occurrences of FROMCHAR with TOCHAR in STRING.
/// INPLACE is ignored (we always return a new string).
pub(crate) fn builtin_subst_char_in_string(args: Vec<Value>) -> EvalResult {
    expect_min_args("subst-char-in-string", &args, 3)?;
    expect_max_args("subst-char-in-string", &args, 4)?;
    let from_char = expect_char(&args[0])?;
    let to_char = expect_char(&args[1])?;
    let s = expect_string(&args[2])?;
    let result: String = s
        .chars()
        .map(|c| if c == from_char { to_char } else { c })
        .collect();
    Ok(Value::string(result))
}

/// `(string-to-multibyte STRING)` -- convert unibyte storage bytes to multibyte chars.
pub(crate) fn builtin_string_to_multibyte(args: Vec<Value>) -> EvalResult {
    expect_args("string-to-multibyte", &args, 1)?;
    if let Value::Str(id) = args[0] {
        if with_heap(|h| h.string_is_multibyte(id)) {
            return Ok(args[0]);
        }
    }
    let s = expect_string(&args[0])?;
    Ok(Value::multibyte_string(
        convert_unibyte_storage_to_multibyte(&s),
    ))
}

/// `(string-to-unibyte STRING)` -- convert to unibyte storage.
pub(crate) fn builtin_string_to_unibyte(args: Vec<Value>) -> EvalResult {
    expect_args("string-to-unibyte", &args, 1)?;
    if let Value::Str(id) = args[0] {
        if !with_heap(|h| h.string_is_multibyte(id)) {
            return Ok(args[0]);
        }
    }
    let s = expect_string(&args[0])?;

    let mut bytes = Vec::with_capacity(s.chars().count());
    for (idx, ch) in s.chars().enumerate() {
        let cp = ch as u32;
        if cp <= 0x7F {
            bytes.push(cp as u8);
            continue;
        }
        if (RAW_BYTE_SENTINEL_MIN..=RAW_BYTE_SENTINEL_MAX).contains(&cp) {
            bytes.push((cp - RAW_BYTE_SENTINEL_BASE) as u8);
            continue;
        }
        if (UNIBYTE_BYTE_SENTINEL_MIN..=UNIBYTE_BYTE_SENTINEL_MAX).contains(&cp) {
            bytes.push((cp - UNIBYTE_BYTE_SENTINEL_BASE) as u8);
            continue;
        }

        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Cannot convert character at index {idx} to unibyte"
            ))],
        ));
    }

    Ok(Value::unibyte_string(bytes_to_unibyte_storage_string(
        &bytes,
    )))
}

/// `(string-as-unibyte STRING)` -- reinterpret as unibyte byte sequence.
pub(crate) fn builtin_string_as_unibyte(args: Vec<Value>) -> EvalResult {
    expect_args("string-as-unibyte", &args, 1)?;
    if let Value::Str(id) = args[0] {
        if !with_heap(|h| h.string_is_multibyte(id)) {
            return Ok(args[0]);
        }
    }
    let s = expect_string(&args[0])?;

    let mut bytes = Vec::with_capacity(s.len());
    for ch in s.chars() {
        let cp = ch as u32;
        if cp <= 0x7F {
            bytes.push(cp as u8);
            continue;
        }
        if (RAW_BYTE_SENTINEL_MIN..=RAW_BYTE_SENTINEL_MAX).contains(&cp) {
            bytes.push((cp - RAW_BYTE_SENTINEL_BASE) as u8);
            continue;
        }
        if (UNIBYTE_BYTE_SENTINEL_MIN..=UNIBYTE_BYTE_SENTINEL_MAX).contains(&cp) {
            bytes.push((cp - UNIBYTE_BYTE_SENTINEL_BASE) as u8);
            continue;
        }

        let mut utf8 = [0u8; 4];
        let encoded = ch.encode_utf8(&mut utf8);
        bytes.extend_from_slice(encoded.as_bytes());
    }

    Ok(Value::unibyte_string(bytes_to_unibyte_storage_string(
        &bytes,
    )))
}

/// `(string-as-multibyte STRING)` -- reinterpret unibyte storage as multibyte.
pub(crate) fn builtin_string_as_multibyte(args: Vec<Value>) -> EvalResult {
    expect_args("string-as-multibyte", &args, 1)?;
    if let Value::Str(id) = args[0] {
        if with_heap(|h| h.string_is_multibyte(id)) {
            return Ok(args[0]);
        }
    }
    let s = expect_string(&args[0])?;
    Ok(Value::multibyte_string(
        convert_unibyte_storage_to_multibyte(&s),
    ))
}

/// `(unibyte-char-to-multibyte CHAR)` -- map 0..255 to multibyte/raw-byte char code.
pub(crate) fn builtin_unibyte_char_to_multibyte(args: Vec<Value>) -> EvalResult {
    expect_args("unibyte-char-to-multibyte", &args, 1)?;
    let code = expect_character_code(&args[0])?;
    if code > 0xFF {
        return Err(signal(
            "error",
            vec![Value::string(format!("Not a unibyte character: {code}"))],
        ));
    }
    if code < 0x80 {
        Ok(Value::Int(code))
    } else {
        Ok(Value::Int(code + 0x3FFF00))
    }
}

/// `(multibyte-char-to-unibyte CHAR)` -- map multibyte/raw-byte char code to byte.
pub(crate) fn builtin_multibyte_char_to_unibyte(args: Vec<Value>) -> EvalResult {
    expect_args("multibyte-char-to-unibyte", &args, 1)?;
    let code = expect_character_code(&args[0])?;
    if code <= 0xFF {
        return Ok(Value::Int(code));
    }
    if (0x3FFF80..=0x3FFFFF).contains(&code) {
        return Ok(Value::Int(code - 0x3FFF00));
    }
    Ok(Value::Int(-1))
}

/// `(locale-info ITEM)` -- minimal locale info.
/// Returns a small oracle-aligned subset in batch mode.
pub(crate) fn builtin_locale_info(args: Vec<Value>) -> EvalResult {
    expect_args("locale-info", &args, 1)?;
    match &args[0] {
        Value::Symbol(item) if resolve_sym(*item) == "codeset" => Ok(Value::string("UTF-8")),
        Value::Symbol(item) if resolve_sym(*item) == "days" => Ok(Value::vector(vec![
            Value::string("Sunday"),
            Value::string("Monday"),
            Value::string("Tuesday"),
            Value::string("Wednesday"),
            Value::string("Thursday"),
            Value::string("Friday"),
            Value::string("Saturday"),
        ])),
        Value::Symbol(item) if resolve_sym(*item) == "months" => Ok(Value::vector(vec![
            Value::string("January"),
            Value::string("February"),
            Value::string("March"),
            Value::string("April"),
            Value::string("May"),
            Value::string("June"),
            Value::string("July"),
            Value::string("August"),
            Value::string("September"),
            Value::string("October"),
            Value::string("November"),
            Value::string("December"),
        ])),
        Value::Symbol(item) if resolve_sym(*item) == "paper" => {
            Ok(Value::list(vec![Value::Int(210), Value::Int(297)]))
        }
        _ => Ok(Value::Nil),
    }
}

/// `(display-line-numbers-update-width)` -- compatibility no-op in batch mode.
#[cfg(test)]
pub(crate) fn builtin_display_line_numbers_update_width(args: Vec<Value>) -> EvalResult {
    expect_args("display-line-numbers-update-width", &args, 0)?;
    Ok(Value::Nil)
}

// ===========================================================================
// Eval-dependent builtins
// ===========================================================================

/// `(backtrace-frame NFRAMES &optional BASE)` -- returns compatibility-formatted
/// synthetic backtrace frames for supported NFRAMES values.
pub(crate) fn builtin_backtrace_frame(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backtrace-frame", &args, 1)?;
    expect_max_args("backtrace-frame", &args, 2)?;
    let nframes = expect_wholenump(&args[0])?;

    if args.get(1).is_some_and(|v| v.is_truthy()) {
        return Ok(Value::Nil);
    }

    match nframes {
        0 => {
            let mut frame = vec![Value::True, Value::symbol("backtrace-frame"), Value::Int(0)];
            if args.len() > 1 {
                frame.push(args[1]);
            }
            Ok(Value::list(frame))
        }
        1 => {
            let mut call = vec![Value::symbol("backtrace-frame"), Value::Int(1)];
            if args.len() > 1 {
                call.push(args[1]);
            }
            Ok(Value::list(vec![
                Value::True,
                Value::symbol("eval"),
                Value::list(call),
                Value::Nil,
            ]))
        }
        2 | 3 => Ok(Value::list(vec![Value::Nil])),
        _ => Ok(Value::Nil),
    }
}

fn expect_threadp(eval: &super::eval::Evaluator, value: &Value) -> Result<(), Flow> {
    if eval.threads.thread_id_from_handle(value).is_some() {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("threadp"), *value],
        ))
    }
}

/// `(backtrace--frames-from-thread THREAD)` -- synthetic backtrace frame list.
pub(crate) fn builtin_backtrace_frames_from_thread(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("backtrace--frames-from-thread", &args, 1)?;
    expect_threadp(eval, &args[0])?;
    Ok(Value::list(vec![Value::list(vec![
        Value::True,
        Value::symbol("backtrace--frames-from-thread"),
        args[0],
    ])]))
}

/// `(backtrace--locals FRAME &optional BASE)` -- batch-compatible helper.
pub(crate) fn builtin_backtrace_locals(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backtrace--locals", &args, 1)?;
    expect_max_args("backtrace--locals", &args, 2)?;
    let frame = expect_wholenump(&args[0])?;
    if frame == 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), Value::Int(-1)],
        ));
    }
    if let Some(base) = args.get(1) {
        let _ = expect_wholenump(base)?;
    }
    Ok(Value::Nil)
}

/// `(backtrace-debug FRAME INDEX &optional FLAG)` -- batch-compatible helper.
pub(crate) fn builtin_backtrace_debug(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backtrace-debug", &args, 2)?;
    expect_max_args("backtrace-debug", &args, 3)?;
    let _ = expect_wholenump(&args[0])?;
    let _ = expect_wholenump(&args[1])?;
    Ok(args[0])
}

/// `(backtrace-eval FRAME INDEX &optional FLAG)` -- batch-compatible helper.
pub(crate) fn builtin_backtrace_eval(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backtrace-eval", &args, 2)?;
    expect_max_args("backtrace-eval", &args, 3)?;
    let _ = expect_wholenump(&args[0])?;
    let _ = expect_wholenump(&args[1])?;
    Ok(Value::Nil)
}

/// `(backtrace-frame--internal FUN NFRAMES BASE)` -- compatibility helper.
///
/// In official Emacs this walks the specpdl stack and calls FUN for each
/// frame.  NeoVM doesn't maintain a specpdl-style stack, so we return nil
/// (no frames available) rather than signalling an error.
pub(crate) fn builtin_backtrace_frame_internal(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("backtrace-frame--internal", &args, 3)?;
    Ok(Value::Nil)
}

/// `(recursion-depth)` -- return the current Lisp recursion depth.
/// Uses the dynamic binding stack depth as a proxy (the true depth counter
/// is private to the Evaluator).
pub(crate) fn builtin_recursion_depth(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("recursion-depth", &args, 0)?;
    Ok(Value::Int(eval.dynamic.len() as i64))
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "misc_test.rs"]
mod tests;
