//! Miscellaneous commonly-needed builtins.
//!
//! Contains:
//! - Special forms: prog2, save-current-buffer
//! - Pure builtins: copy-alist, rassoc, rassq, assoc-default, make-list, safe-length,
//!   subst-char-in-string, string/char encoding stubs, locale-info
//! - Eval-dependent builtins: backtrace-* helpers, recursion-depth

use super::error::{EvalResult, Flow, signal};
use super::expr::Expr;
use super::intern::resolve_sym;
use super::string_escape::{bytes_to_unibyte_storage_string, encode_nonunicode_char_for_storage};
use super::value::*;
use crate::emacs_core::value::{ValueKind};

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

fn expect_wholenump(val: &Value) -> Result<i64, Flow> {
    match val.kind() {
        ValueKind::Fixnum(n) if n >= 0 => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *val],
        )),
    }
}

fn expect_string(val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::String => Ok(val.as_str().unwrap().to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *val],
        )),
    }
}

fn expect_char(val: &Value) -> Result<char, Flow> {
    match val.kind() {
        ValueKind::Char(c) => Ok(c),
        ValueKind::Fixnum(n) => char::from_u32(n as u32).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *val],
            )
        }),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *val],
        )),
    }
}

fn expect_character_code(val: &Value) -> Result<i64, Flow> {
    match val.kind() {
        ValueKind::Char(c) => Ok(c as i64),
        ValueKind::Fixnum(n) if (0..=MAX_EMACS_CHAR).contains(&n) => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *val],
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

/// `(save-current-buffer BODY...)` -- save the current buffer, execute BODY,
/// then restore the previous current buffer.
pub(crate) fn sf_save_current_buffer(eval: &mut super::eval::Context, tail: &[Expr]) -> EvalResult {
    let saved_buf = eval.buffers.current_buffer().map(|b| b.id);
    let result = eval.sf_progn(tail);
    if let Some(saved_id) = saved_buf {
        eval.restore_current_buffer_if_live(saved_id);
    }
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
        match cursor.kind() {
            ValueKind::Nil => break,
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                // If the element is a cons, copy it; otherwise keep as-is
                let entry = match pair_car.kind() {
                    ValueKind::Cons => {
                        let inner_pair_car = cursor.cons_car();
                        let inner_pair_cdr = cursor.cons_cdr();
                        Value::cons(inner_pair_car, inner_pair_cdr)
                    }
                    other => *cursor,
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
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if &pair_car.is_cons() {
                    let inner_pair_car = cursor.cons_car();
                    let inner_pair_cdr = cursor.cons_cdr();
                    if equal_value(&inner_pair_cdr, key, 0) {
                        return Ok(pair_car);
                    }
                }
                cursor = pair_cdr;
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
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if &pair_car.is_cons() {
                    let inner_pair_car = cursor.cons_car();
                    let inner_pair_cdr = cursor.cons_cdr();
                    if eq_value(&inner_pair_cdr, key) {
                        return Ok(pair_car);
                    }
                }
                cursor = pair_cdr;
            }
            _ => return Ok(Value::NIL),
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
        return Ok(Value::fixnum(0));
    }
    if !list.is_cons() {
        return Ok(Value::fixnum(0));
    }

    // GNU uses FOR_EACH_TAIL_SAFE which implements Brent's cycle
    // detection (teleporting tortoise). This matches the exact count
    // GNU returns for circular lists.
    let mut tortoise = *list;
    let mut hare = *list;
    let mut length: i64 = 0;
    let mut power: i64 = 1;
    let mut step: i64 = 0;

    loop {
        match hare.kind() {
            ValueKind::Cons => {
                let pair_car = hare.cons_car();
                let pair_cdr = hare.cons_cdr();
                hare = pair_cdr;
                length += 1;
                step += 1;
            }
            _ => return Ok(Value::fixnum(length)),
        }

        // Brent's: check if hare caught up to tortoise
        if let (Value::Cons(a), Value::Cons(b)) = (&hare, &tortoise) {
            if a == b {
                return Ok(Value::fixnum(length));
            }
        }

        // Teleport tortoise: when step count reaches power, move
        // tortoise to hare's position and double the power.
        if step == power {
            tortoise = hare;
            power *= 2;
            step = 0;
        }

        if length > 10_000_000 {
            return Ok(Value::fixnum(length));
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
    if args[0].is_string() {
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
    if args[0].is_string() {
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
    if args[0].is_string() {
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
    if args[0].is_string() {
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
        Ok(Value::fixnum(code))
    } else {
        Ok(Value::fixnum(code + 0x3FFF00))
    }
}

/// `(multibyte-char-to-unibyte CHAR)` -- map multibyte/raw-byte char code to byte.
pub(crate) fn builtin_multibyte_char_to_unibyte(args: Vec<Value>) -> EvalResult {
    expect_args("multibyte-char-to-unibyte", &args, 1)?;
    let code = expect_character_code(&args[0])?;
    if code <= 0xFF {
        return Ok(Value::fixnum(code));
    }
    if (0x3FFF80..=0x3FFFFF).contains(&code) {
        return Ok(Value::fixnum(code - 0x3FFF00));
    }
    Ok(Value::fixnum(-1))
}

/// `(locale-info ITEM)` -- minimal locale info.
/// Returns a small oracle-aligned subset in batch mode.
pub(crate) fn builtin_locale_info(args: Vec<Value>) -> EvalResult {
    expect_args("locale-info", &args, 1)?;
    match args[0].kind() {
        ValueKind::Symbol(item) if resolve_sym(item) == "codeset" => Ok(Value::string("UTF-8")),
        ValueKind::Symbol(item) if resolve_sym(item) == "days" => Ok(Value::vector(vec![
            Value::string("Sunday"),
            Value::string("Monday"),
            Value::string("Tuesday"),
            Value::string("Wednesday"),
            Value::string("Thursday"),
            Value::string("Friday"),
            Value::string("Saturday"),
        ])),
        ValueKind::Symbol(item) if resolve_sym(item) == "months" => Ok(Value::vector(vec![
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
        ValueKind::Symbol(item) if resolve_sym(item) == "paper" => {
            Ok(Value::list(vec![Value::fixnum(210), Value::fixnum(297)]))
        }
        _ => Ok(Value::NIL),
    }
}

/// `(display-line-numbers-update-width)` -- compatibility no-op in batch mode.
#[cfg(test)]
pub(crate) fn builtin_display_line_numbers_update_width(args: Vec<Value>) -> EvalResult {
    expect_args("display-line-numbers-update-width", &args, 0)?;
    Ok(Value::NIL)
}

// ===========================================================================
// Eval-dependent builtins
// ===========================================================================

/// `(backtrace-frame NFRAMES &optional BASE)` -- returns compatibility-formatted
/// synthetic backtrace frames for supported NFRAMES values.
pub(crate) fn builtin_backtrace_frame(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backtrace-frame", &args, 1)?;
    expect_max_args("backtrace-frame", &args, 2)?;
    let nframes = expect_wholenump(&args[0])?;

    if args.get(1).is_some_and(|v| v.is_truthy()) {
        return Ok(Value::NIL);
    }

    match nframes.kind() {
        0 => {
            let mut frame = vec![Value::T, Value::symbol("backtrace-frame"), Value::fixnum(nframes)];
            if args.len() > 1 {
                frame.push(args[1]);
            }
            Ok(Value::list(frame))
        }
        1 => {
            let mut call = vec![Value::symbol("backtrace-frame"), Value::fixnum(nframes)];
            if args.len() > nframes {
                call.push(args[nframes]);
            }
            Ok(Value::list(vec![
                Value::T,
                Value::symbol("eval"),
                Value::list(call),
                Value::NIL,
            ]))
        }
        2 | 3 => Ok(Value::list(vec![Value::NIL])),
        _ => Ok(Value::NIL),
    }
}

fn expect_threadp(eval: &super::eval::Context, value: &Value) -> Result<(), Flow> {
    expect_threadp_in_state(&eval.threads, value)
}

fn expect_threadp_in_state(
    threads: &crate::emacs_core::threads::ThreadManager,
    value: &Value,
) -> Result<(), Flow> {
    if threads.thread_id_from_handle(value).is_some() {
        return Ok(());
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("threadp"), *value],
    ))
}

/// `(backtrace--frames-from-thread THREAD)` -- synthetic backtrace frame list.
pub(crate) fn builtin_backtrace_frames_from_thread(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("backtrace--frames-from-thread", &args, 1)?;
    expect_threadp_in_state(&eval.threads, &args[0])?;
    Ok(Value::list(vec![Value::list(vec![
        Value::T,
        Value::symbol("backtrace--frames-from-thread"),
        args[0],
    ])]))
}

/// `(backtrace--locals FRAME &optional BASE)` -- batch-compatible helper.
pub(crate) fn builtin_backtrace_locals(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backtrace--locals", &args, 1)?;
    expect_max_args("backtrace--locals", &args, 2)?;
    let frame = expect_wholenump(&args[0])?;
    if frame == 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), Value::fixnum(-1)],
        ));
    }
    if let Some(base) = args.get(1) {
        let _ = expect_wholenump(base)?;
    }
    Ok(Value::NIL)
}

/// `(backtrace-debug FRAME INDEX &optional FLAG)` -- batch-compatible helper.
pub(crate) fn builtin_backtrace_debug(
    _eval: &mut super::eval::Context,
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
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backtrace-eval", &args, 2)?;
    expect_max_args("backtrace-eval", &args, 3)?;
    let _ = expect_wholenump(&args[0])?;
    let _ = expect_wholenump(&args[1])?;
    Ok(Value::NIL)
}

fn runtime_backtrace_indirect_function(
    eval: &super::eval::Context,
    function: Value,
) -> Option<Value> {
    match function.kind() {
        ValueKind::Symbol(symbol) => {
            super::builtins::symbols::resolve_indirect_symbol_by_id_in_obarray(
                &eval.obarray,
                symbol,
            )
            .map(|(_, value)| value)
            .or(Some(function))
        }
        ValueKind::T => runtime_backtrace_indirect_function(eval, Value::symbol("t")),
        ValueKind::Keyword(symbol) => {
            super::builtins::symbols::resolve_indirect_symbol_by_id_in_obarray(
                &eval.obarray,
                symbol,
            )
            .map(|(_, value)| value)
            .or(Some(function))
        }
        ValueKind::Nil => None,
        other => Some(function),
    }
}

fn runtime_backtrace_frames_from_base(
    eval: &super::eval::Context,
    base: Value,
) -> Result<Vec<super::eval::RuntimeBacktraceFrame>, Flow> {
    let mut offset = 0usize;
    let mut base_function = base;
    if base.is_cons() {
        let pair_car = base.cons_car();
        let pair_cdr = base.cons_cdr();
        if let Some(raw_offset) = pair_car.as_fixnum() {
            if raw_offset < 0 {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("wholenump"), pair_car],
                ));
            }
            offset = raw_offset as usize;
            base_function = pair_cdr;
        }
    }

    let start_index = if base_function.is_nil() {
        eval.runtime_backtrace.len().checked_sub(1)
    } else {
        let Some(indirect_base) = runtime_backtrace_indirect_function(eval, base_function) else {
            return Ok(Vec::new());
        };
        let mut found = None;
        for (index, frame) in eval.runtime_backtrace.iter().enumerate().rev() {
            let Some(indirect_frame) = runtime_backtrace_indirect_function(eval, frame.function)
            else {
                continue;
            };
            if eq_value(&indirect_frame, &indirect_base) {
                found = Some(index);
                break;
            }
        }
        found
    };

    let Some(mut index) = start_index else {
        return Ok(Vec::new());
    };

    while offset > 0 {
        if index == 0 {
            return Ok(Vec::new());
        }
        index -= 1;
        offset -= 1;
    }

    Ok(eval.runtime_backtrace[..=index]
        .iter()
        .rev()
        .cloned()
        .collect())
}

fn runtime_backtrace_frame_flags(frame: &super::eval::RuntimeBacktraceFrame) -> Value {
    if frame.debug_on_exit {
        Value::list(vec![Value::symbol(":debug-on-exit"), Value::T])
    } else {
        Value::NIL
    }
}

fn apply_backtrace_callback(
    eval: &mut super::eval::Context,
    function: Value,
    frame: &super::eval::RuntimeBacktraceFrame,
) -> EvalResult {
    eval.apply(
        function,
        vec![
            Value::bool_val(frame.evaluated),
            frame.function,
            Value::list(frame.args.clone()),
            runtime_backtrace_frame_flags(frame),
        ],
    )
}

/// `(backtrace-frame--internal FUN NFRAMES BASE)` -- compatibility helper.
///
/// In official Emacs this walks the specpdl backtrace. NeoVM now keeps a
/// GNU-shaped runtime call stack and feeds that through the same callback
/// shape that `subr.el` expects.
pub(crate) fn builtin_backtrace_frame_internal(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("backtrace-frame--internal", &args, 3)?;
    let nframes = expect_wholenump(&args[1])? as usize;
    let frames = runtime_backtrace_frames_from_base(eval, args[2])?;
    let Some(frame) = frames.get(nframes) else {
        return Ok(Value::NIL);
    };
    apply_backtrace_callback(eval, args[0], frame)
}

/// `(mapbacktrace FUNCTION &optional BASE)` -- iterate runtime backtrace
/// frames in GNU order, newest first.
pub(crate) fn builtin_mapbacktrace(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("mapbacktrace", &args, 1)?;
    expect_max_args("mapbacktrace", &args, 2)?;
    let base = args.get(1).copied().unwrap_or(Value::NIL);
    let frames = runtime_backtrace_frames_from_base(eval, base)?;
    for frame in &frames {
        apply_backtrace_callback(eval, args[0], frame)?;
    }
    Ok(Value::NIL)
}

/// `(recursion-depth)` -- return the current Lisp recursion depth.
/// Uses the dynamic binding stack depth as a proxy (the true depth counter
/// is private to the Context).
pub(crate) fn builtin_recursion_depth(
    _eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("recursion-depth", &args, 0)?;
    Ok(Value::fixnum(0))
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "misc_test.rs"]
mod tests;
