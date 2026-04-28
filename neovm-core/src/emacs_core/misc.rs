//! Miscellaneous commonly-needed builtins.
//!
//! Contains:
//! - Special forms: prog2, save-current-buffer
//! - Pure builtins: copy-alist, rassoc, rassq, assoc-default, make-list, safe-length,
//!   subst-char-in-string, string/char encoding stubs, locale-info
//! - Eval-dependent builtins: backtrace-* helpers, recursion-depth

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::*;

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
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *val],
        )),
    }
}

fn expect_string(val: &Value) -> Result<String, Flow> {
    val.as_runtime_string_owned()
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("stringp"), *val]))
}

fn expect_character_code(val: &Value) -> Result<i64, Flow> {
    match val.kind() {
        ValueKind::Fixnum(c) => Ok(c as i64),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *val],
        )),
    }
}

/// Convert unibyte LispString bytes to multibyte Emacs encoding.
fn convert_unibyte_to_multibyte_bytes(src: &[u8]) -> Vec<u8> {
    use crate::emacs_core::emacs_char;
    let mut out = Vec::with_capacity(src.len() * 2);
    for &b in src {
        let c = emacs_char::byte8_to_char(b);
        let mut buf = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
        let len = emacs_char::char_string(c, &mut buf);
        out.extend_from_slice(&buf[..len]);
    }
    out
}

/// Reinterpret unibyte bytes as an Emacs multibyte sequence.
///
/// Valid multibyte sequences are preserved as-is; lone high bytes become
/// raw-byte characters.
fn reinterpret_unibyte_as_multibyte_bytes(src: &[u8]) -> Vec<u8> {
    use crate::emacs_core::emacs_char;

    let mut out = Vec::with_capacity(src.len() * 2);
    let mut pos = 0usize;
    while pos < src.len() {
        let (cp, len) = emacs_char::string_char(&src[pos..]);
        if len > 1 {
            let mut buf = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
            let enc_len = emacs_char::char_string(cp, &mut buf);
            if enc_len == len && src.get(pos..pos + len) == Some(&buf[..len]) {
                out.extend_from_slice(&src[pos..pos + len]);
                pos += len;
                continue;
            }
        } else if src[pos] < 0x80 {
            out.push(src[pos]);
            pos += 1;
            continue;
        }

        let c = emacs_char::byte8_to_char(src[pos]);
        let mut buf = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
        let enc_len = emacs_char::char_string(c, &mut buf);
        out.extend_from_slice(&buf[..enc_len]);
        pos += 1;
    }
    out
}

// ===========================================================================
// Special forms
// ===========================================================================

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
                        let inner_pair_car = pair_car.cons_car();
                        let inner_pair_cdr = pair_car.cons_cdr();
                        Value::cons(inner_pair_car, inner_pair_cdr)
                    }
                    _ => pair_car,
                };
                result.push(entry);
                cursor = pair_cdr;
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
    builtin_rassoc_with_symbols(args, false)
}

pub(crate) fn builtin_rassoc_with_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_rassoc_with_symbols(args, eval.symbols_with_pos_enabled)
}

fn builtin_rassoc_with_symbols(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
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
                if pair_car.is_cons() {
                    let inner_pair_cdr = pair_car.cons_cdr();
                    if equal_value_swp(&inner_pair_cdr, key, 0, symbols_with_pos_enabled) {
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
    builtin_rassq_with_symbols(args, false)
}

pub(crate) fn builtin_rassq_with_ctx(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_rassq_with_symbols(args, eval.symbols_with_pos_enabled)
}

fn builtin_rassq_with_symbols(args: Vec<Value>, symbols_with_pos_enabled: bool) -> EvalResult {
    expect_args("rassq", &args, 2)?;
    builtin_rassq_values(args[0], args[1], symbols_with_pos_enabled)
}

pub(crate) fn builtin_rassq_2(
    eval: &mut super::eval::Context,
    key: Value,
    alist: Value,
) -> EvalResult {
    builtin_rassq_values(key, alist, eval.symbols_with_pos_enabled)
}

fn builtin_rassq_values(key: Value, alist: Value, symbols_with_pos_enabled: bool) -> EvalResult {
    let mut cursor = alist;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if pair_car.is_cons() {
                    let inner_pair_cdr = pair_car.cons_cdr();
                    if eq_value_swp(&inner_pair_cdr, &key, symbols_with_pos_enabled) {
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
        if hare.is_cons() && tortoise.is_cons() && eq_value(&hare, &tortoise) {
            return Ok(Value::fixnum(length));
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
    let from_code = expect_character_code(&args[0])? as u32;
    let to_code = expect_character_code(&args[1])? as u32;
    let src_ls = args[2].as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[2]],
        )
    })?;

    use crate::emacs_core::emacs_char;
    let src_bytes = src_ls.as_bytes();

    // Unibyte path: each byte is one character. If either FROM or TO
    // doesn't fit in a single byte the substitution can't apply, so
    // return the original string unchanged (mirroring GNU which only
    // allows unibyte chars in unibyte contexts).
    if !src_ls.is_multibyte() {
        if from_code > 0xFF || to_code > 0xFF {
            return Ok(args[2]);
        }
        let from_byte = from_code as u8;
        if !src_bytes.contains(&from_byte) {
            return Ok(args[2]);
        }
        let to_byte = to_code as u8;
        let replaced: Vec<u8> = src_bytes
            .iter()
            .map(|&b| if b == from_byte { to_byte } else { b })
            .collect();
        return Ok(Value::heap_string(
            crate::heap_types::LispString::from_unibyte(replaced),
        ));
    }

    // Multibyte path: walk FROM via emacs_char::string_char, emitting
    // TO's Emacs encoding whenever the decoded char matches FROM.
    // Unlike the old same-length-only helper, this handles FROM/TO pairs
    // that encode to different byte counts (matching GNU fns.c:3196).
    let mut to_buf = [0u8; emacs_char::MAX_MULTIBYTE_LENGTH];
    let to_len = emacs_char::char_string(to_code, &mut to_buf);
    let to_bytes = &to_buf[..to_len];

    let mut out = Vec::with_capacity(src_bytes.len());
    let mut changed = false;
    let mut pos = 0;
    while pos < src_bytes.len() {
        let (code, len) = emacs_char::string_char(&src_bytes[pos..]);
        let clen = len.max(1);
        if code == from_code {
            out.extend_from_slice(to_bytes);
            changed = true;
        } else {
            out.extend_from_slice(&src_bytes[pos..pos + clen]);
        }
        pos += clen;
    }
    if !changed {
        return Ok(args[2]);
    }
    Ok(Value::heap_string(
        crate::heap_types::LispString::from_emacs_bytes(out),
    ))
}

/// `(string-to-multibyte STRING)` -- convert unibyte storage bytes to multibyte chars.
pub(crate) fn builtin_string_to_multibyte(args: Vec<Value>) -> EvalResult {
    expect_args("string-to-multibyte", &args, 1)?;
    let ls = args[0].as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )
    })?;
    if ls.is_multibyte() {
        return Ok(args[0]);
    }
    let out = convert_unibyte_to_multibyte_bytes(ls.as_bytes());
    Ok(Value::heap_string(
        crate::heap_types::LispString::from_emacs_bytes(out),
    ))
}

/// `(string-to-unibyte STRING)` -- convert to unibyte storage.
pub(crate) fn builtin_string_to_unibyte(args: Vec<Value>) -> EvalResult {
    use crate::emacs_core::emacs_char;
    expect_args("string-to-unibyte", &args, 1)?;
    let ls = args[0].as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )
    })?;
    if !ls.is_multibyte() {
        return Ok(args[0]);
    }
    let src = ls.as_bytes();
    let mut bytes = Vec::with_capacity(ls.schars());
    let mut pos = 0;
    let mut idx = 0usize;
    while pos < src.len() {
        let (cp, len) = emacs_char::string_char(&src[pos..]);
        pos += len;
        if cp <= 0x7F {
            bytes.push(cp as u8);
        } else if emacs_char::char_byte8_p(cp) {
            bytes.push(emacs_char::char_to_byte8(cp));
        } else {
            return Err(signal(
                "error",
                vec![Value::string(format!(
                    "Cannot convert character at index {idx} to unibyte"
                ))],
            ));
        }
        idx += 1;
    }
    Ok(Value::heap_string(
        crate::heap_types::LispString::from_unibyte(bytes),
    ))
}

/// `(string-as-unibyte STRING)` -- reinterpret as unibyte byte sequence.
pub(crate) fn builtin_string_as_unibyte(args: Vec<Value>) -> EvalResult {
    use crate::emacs_core::emacs_char;
    expect_args("string-as-unibyte", &args, 1)?;
    let ls = args[0].as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )
    })?;
    if !ls.is_multibyte() {
        return Ok(args[0]);
    }
    // Reinterpret: raw-byte chars become their byte value, other chars keep
    // their UTF-8 encoding as raw bytes.
    let src = ls.as_bytes();
    let mut bytes = Vec::with_capacity(src.len());
    let mut pos = 0;
    while pos < src.len() {
        let (cp, len) = emacs_char::string_char(&src[pos..]);
        if emacs_char::char_byte8_p(cp) {
            bytes.push(emacs_char::char_to_byte8(cp));
        } else {
            // Keep the raw encoding bytes
            bytes.extend_from_slice(&src[pos..pos + len]);
        }
        pos += len;
    }
    Ok(Value::heap_string(
        crate::heap_types::LispString::from_unibyte(bytes),
    ))
}

/// `(string-as-multibyte STRING)` -- reinterpret unibyte storage as multibyte.
pub(crate) fn builtin_string_as_multibyte(args: Vec<Value>) -> EvalResult {
    expect_args("string-as-multibyte", &args, 1)?;
    let ls = args[0].as_lisp_string().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )
    })?;
    if ls.is_multibyte() {
        return Ok(args[0]);
    }
    let out = reinterpret_unibyte_as_multibyte_bytes(ls.as_bytes());
    Ok(Value::heap_string(
        crate::heap_types::LispString::from_emacs_bytes(out),
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

// `backtrace-frame` is implemented in elisp in `lisp/subr.el:6703-6718`,
// delegating to `backtrace-frame--internal` (below). No Rust-level
// `backtrace-frame` primitive exists; a previous stub returning
// synthetic canned frames was removed because it never made it to the
// defsubr registry (subr.el's defun wins at runtime) and its fixed
// output did not match GNU semantics.

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

/// `(backtrace--locals NFRAMES &optional BASE)` -- batch-compatible helper.
pub(crate) fn builtin_backtrace_locals(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backtrace--locals", &args, 1)?;
    expect_max_args("backtrace--locals", &args, 2)?;
    let nframes = expect_wholenump(&args[0])? as usize;
    if nframes == 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), Value::fixnum(-1)],
        ));
    }
    let base = args.get(1).copied().unwrap_or(Value::NIL);
    let frames = runtime_backtrace_frames_from_base(eval, base)?;
    if frames.get(nframes).is_none() || frames.get(nframes - 1).is_none() {
        return Err(signal(
            "error",
            vec![Value::string("Activation frame not found!")],
        ));
    }
    Ok(Value::NIL)
}

/// `(backtrace-debug LEVEL FLAG &optional BASE)` -- batch-compatible helper.
pub(crate) fn builtin_backtrace_debug(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backtrace-debug", &args, 2)?;
    expect_max_args("backtrace-debug", &args, 3)?;
    let _level = expect_wholenump(&args[0])?;
    if let Some(base) = args.get(2) {
        let _ = runtime_backtrace_frames_from_base(eval, *base)?;
    }
    Ok(args[1])
}

/// `(backtrace-eval EXP NFRAMES &optional BASE)` -- batch-compatible helper.
pub(crate) fn builtin_backtrace_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("backtrace-eval", &args, 2)?;
    expect_max_args("backtrace-eval", &args, 3)?;
    let nframes = expect_wholenump(&args[1])? as usize;
    let base = args.get(2).copied().unwrap_or(Value::NIL);
    let frames = runtime_backtrace_frames_from_base(eval, base)?;
    if frames.get(nframes).is_none() {
        return Err(signal(
            "error",
            vec![Value::string("Activation frame not found!")],
        ));
    }
    eval.eval_value(&args[0])
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
        ValueKind::Nil => None,
        _ => Some(function),
    }
}

/// Snapshot of a single backtrace frame extracted from the specpdl.
struct BacktraceFrameSnapshot {
    function: Value,
    args: Vec<Value>,
    debug_on_exit: bool,
    /// `true` mirrors GNU `nargs == UNEVALLED` for special forms; `args`
    /// then holds a single element that is the cons list of un-evaluated
    /// argument forms.
    unevalled: bool,
}

/// Collect backtrace frames from the specpdl, ordered oldest-first (index 0 = deepest).
fn collect_backtrace_frames(eval: &super::eval::Context) -> Vec<BacktraceFrameSnapshot> {
    eval.specpdl
        .iter()
        .filter_map(|entry| match entry {
            super::eval::SpecBinding::Backtrace {
                function,
                args,
                debug_on_exit,
                unevalled,
            } => Some(BacktraceFrameSnapshot {
                function: *function,
                args: args.iter().copied().collect(),
                debug_on_exit: *debug_on_exit,
                unevalled: *unevalled,
            }),
            _ => None,
        })
        .collect()
}

fn runtime_backtrace_frames_from_base(
    eval: &super::eval::Context,
    base: Value,
) -> Result<Vec<BacktraceFrameSnapshot>, Flow> {
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

    let frames = collect_backtrace_frames(eval);

    let start_index = if base_function.is_nil() {
        frames.len().checked_sub(1)
    } else {
        let Some(indirect_base) = runtime_backtrace_indirect_function(eval, base_function) else {
            return Ok(Vec::new());
        };
        let mut found = None;
        for (index, frame) in frames.iter().enumerate().rev() {
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

    Ok(frames.into_iter().take(index + 1).rev().collect())
}

fn runtime_backtrace_frame_flags(frame: &BacktraceFrameSnapshot) -> Value {
    if frame.debug_on_exit {
        Value::list(vec![Value::symbol(":debug-on-exit"), Value::T])
    } else {
        Value::NIL
    }
}

fn apply_backtrace_callback(
    eval: &mut super::eval::Context,
    function: Value,
    frame: &BacktraceFrameSnapshot,
) -> EvalResult {
    // Matches GNU `backtrace_frame_apply` (eval.c:3993-3998).
    // UNEVALLED frames pass `evald=nil` and the single argument
    // slot (the cons list of un-evaluated forms) directly; otherwise
    // pass `evald=t` and a fresh list of the evaluated argument values.
    let (evald, args) = if frame.unevalled {
        let forms = frame.args.first().copied().unwrap_or(Value::NIL);
        (Value::NIL, forms)
    } else {
        (Value::T, Value::list(frame.args.clone()))
    };
    eval.apply(
        function,
        vec![
            evald,
            frame.function,
            args,
            runtime_backtrace_frame_flags(frame),
        ],
    )
}

/// `(backtrace-frame--internal FUN NFRAMES BASE)` -- compatibility helper.
///
/// Walks the specpdl backtrace entries and feeds frames through the same
/// callback shape that `subr.el` expects.
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
