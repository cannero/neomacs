//! Built-in primitive functions.
//!
//! All functions here take pre-evaluated `Vec<Value>` arguments and return `EvalResult`.
//! The evaluator dispatches here after evaluating the argument expressions.

use std::sync::atomic::{AtomicBool, Ordering};

/// Debug flag: when true, log every dispatch_builtin call name.
/// Activated after window-setup-hook completes during startup.
static TRACE_ALL_BUILTINS: AtomicBool = AtomicBool::new(false);

/// Check if post-startup tracing is active.
pub(crate) fn is_post_startup_tracing() -> bool {
    TRACE_ALL_BUILTINS.load(Ordering::Relaxed)
}

pub(super) use super::error::{EvalResult, Flow, signal};
pub(super) use super::intern::{SymId, intern, intern_uninterned, resolve_sym};
pub(super) use super::keyboard::pure::{
    KEY_CHAR_ALT, KEY_CHAR_CODE_MASK, KEY_CHAR_CTRL, KEY_CHAR_HYPER, KEY_CHAR_META, KEY_CHAR_SHIFT,
    KEY_CHAR_SUPER, basic_char_code, describe_single_key_value, event_modifier_bit,
    event_modifier_prefix, key_sequence_values, resolve_control_code, symbol_has_modifier_prefix,
};
pub(super) use super::string_escape::{
    bytes_to_storage_string, bytes_to_unibyte_storage_string, decode_storage_char_codes,
    encode_char_code_for_string_storage, encode_nonunicode_char_for_storage, storage_char_len,
    storage_string_display_width, storage_substring,
};
pub(super) use super::value::*;
pub(super) use crate::gc::ObjId;
pub(super) use ::regex::Regex;
pub(super) use std::cell::RefCell;
pub(super) use std::collections::{HashMap, HashSet};
use strum::EnumString;

/// Reset all thread-local state in builtins (called from Context::new).
pub(crate) fn reset_builtins_thread_locals() {
    collections::reset_collections_thread_locals();
    stubs::reset_stubs_thread_locals();
    hooks::reset_hooks_thread_locals();
    symbols::reset_symbols_thread_locals();
}

/// Expect exactly N arguments.
pub(super) fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

/// Expect at least N arguments.
pub(super) fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

/// Expect at most N arguments.
pub(super) fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

pub(super) fn expect_range_args(
    name: &str,
    args: &[Value],
    min: usize,
    max: usize,
) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

/// Extract an integer, signaling wrong-type-argument if not.
pub(super) fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

pub(super) fn expect_fixnum(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *other],
        )),
    }
}

pub(super) fn expect_char_table_index(value: &Value) -> Result<i64, Flow> {
    let idx = expect_fixnum(value)?;
    if !(0..=0x3F_FFFF).contains(&idx) {
        maybe_trace_characterp_nil(value, "expect_char_table_index");
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *value],
        ));
    }
    Ok(idx)
}

pub(super) fn expect_char_equal_code(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) if (0..=KEY_CHAR_CODE_MASK).contains(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => {
            maybe_trace_characterp_nil(other, "expect_char_equal_code");
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *other],
            ))
        }
    }
}

pub(super) fn expect_character_code(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Char(c) => Ok(*c as i64),
        Value::Int(n) if (0..=0x3FFFFF).contains(n) => Ok(*n),
        other => {
            maybe_trace_characterp_nil(other, "expect_character_code");
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *other],
            ))
        }
    }
}

fn maybe_trace_characterp_nil(value: &Value, source: &str) {
    if !matches!(value, Value::Nil) {
        return;
    }
    if std::env::var("NEOVM_TRACE_CHARACTERP_NIL").unwrap_or_default() != "1" {
        return;
    }
    eprintln!(
        "NEOVM_TRACE_CHARACTERP_NIL source={source}\n{}",
        std::backtrace::Backtrace::force_capture()
    );
}

pub(super) fn char_equal_folded(code: i64) -> Option<String> {
    char::from_u32(code as u32).map(|ch| ch.to_lowercase().collect())
}

/// Extract an integer/marker-ish position value.
///
/// GNU Emacs accepts marker designators anywhere `integer-or-marker-p`
/// is allowed, using the marker's current position.
pub(super) fn expect_integer_or_marker(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other if super::marker::is_marker(other) => super::marker::marker_position_as_int(other),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

pub(super) fn expect_integer_or_marker_eval(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other if super::marker::is_marker(other) => {
            super::marker::marker_position_as_int_eval(eval, other)
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

/// Extract a non-negative integer, signaling `wholenump` on failure.
pub(super) fn expect_wholenump(value: &Value) -> Result<i64, Flow> {
    let n = match value {
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("wholenump"), *value],
            ));
        }
    };
    if n < 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *value],
        ));
    }
    Ok(n)
}

pub(super) enum NumberOrMarker {
    Int(i64),
    Float(f64),
}

pub(super) fn expect_number_or_marker(value: &Value) -> Result<NumberOrMarker, Flow> {
    match value {
        Value::Int(n) => Ok(NumberOrMarker::Int(*n)),
        Value::Char(c) => Ok(NumberOrMarker::Int(*c as i64)),
        Value::Float(f, _) => Ok(NumberOrMarker::Float(*f)),
        other if super::marker::is_marker(other) => Ok(NumberOrMarker::Int(
            super::marker::marker_position_as_int(other)?,
        )),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *other],
        )),
    }
}

pub(super) fn expect_number_or_marker_eval(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<NumberOrMarker, Flow> {
    match value {
        Value::Int(n) => Ok(NumberOrMarker::Int(*n)),
        Value::Char(c) => Ok(NumberOrMarker::Int(*c as i64)),
        Value::Float(f, _) => Ok(NumberOrMarker::Float(*f)),
        other if super::marker::is_marker(other) => Ok(NumberOrMarker::Int(
            super::marker::marker_position_as_int_eval(eval, other)?,
        )),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *other],
        )),
    }
}

/// Extract a number as f64.
pub(super) fn expect_number(value: &Value) -> Result<f64, Flow> {
    match value {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f, _) => Ok(*f),
        Value::Char(c) => Ok(*c as u32 as f64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("numberp"), *other],
        )),
    }
}

pub(super) fn expect_number_or_marker_f64(value: &Value) -> Result<f64, Flow> {
    match expect_number_or_marker(value)? {
        NumberOrMarker::Int(n) => Ok(n as f64),
        NumberOrMarker::Float(f) => Ok(f),
    }
}

pub(super) fn expect_number_or_marker_f64_eval(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<f64, Flow> {
    match expect_number_or_marker_eval(eval, value)? {
        NumberOrMarker::Int(n) => Ok(n as f64),
        NumberOrMarker::Float(f) => Ok(f),
    }
}

pub(super) fn expect_integer_or_marker_after_number_check(value: &Value) -> Result<i64, Flow> {
    match expect_number_or_marker(value)? {
        NumberOrMarker::Int(n) => Ok(n),
        NumberOrMarker::Float(_) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

pub(super) fn expect_integer_or_marker_after_number_check_eval(
    eval: &super::eval::Context,
    value: &Value,
) -> Result<i64, Flow> {
    match expect_number_or_marker_eval(eval, value)? {
        NumberOrMarker::Int(n) => Ok(n),
        NumberOrMarker::Float(_) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

/// True if any arg is a float (triggers float arithmetic).
pub(super) fn has_float(args: &[Value]) -> bool {
    args.iter().any(|v| matches!(v, Value::Float(_, _)))
}

pub(super) fn normalize_string_start_arg(
    string: &str,
    start: Option<&Value>,
) -> Result<usize, Flow> {
    let Some(start_val) = start else {
        return Ok(0);
    };
    if start_val.is_nil() {
        return Ok(0);
    }

    let raw_start = expect_int(start_val)?;
    let len = string.chars().count() as i64;
    let normalized = if raw_start < 0 {
        len.checked_add(raw_start)
    } else {
        Some(raw_start)
    };

    let Some(start_idx) = normalized else {
        return Err(signal(
            "args-out-of-range",
            vec![Value::string(string), Value::Int(raw_start)],
        ));
    };

    if !(0..=len).contains(&start_idx) {
        return Err(signal(
            "args-out-of-range",
            vec![Value::string(string), Value::Int(raw_start)],
        ));
    }

    let start_char_idx = start_idx as usize;
    if start_char_idx == len as usize {
        return Ok(string.len());
    }

    Ok(string
        .char_indices()
        .nth(start_char_idx)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(string.len()))
}

pub(super) fn string_byte_to_char_index(s: &str, byte_idx: usize) -> Option<usize> {
    s.get(..byte_idx).map(|prefix| prefix.chars().count())
}

// Re-export sibling modules so submodules can use `super::eval`, `super::marker`, etc.
pub(super) use super::autoload;
pub(super) use super::builtins_extra;
pub(super) use super::ccl;
pub(super) use super::charset;
pub(super) use super::chartable;
pub(super) use super::editfns;
pub(super) use super::error;
pub(super) use super::eval;
pub(super) use super::expr;
pub(super) use super::fileio;
pub(super) use super::kbd;
pub(super) use super::keymap;
pub(super) use super::load;
pub(super) use super::marker;
pub(super) use super::navigation;
pub(super) use super::print;
pub(super) use super::regex;
pub(super) use super::subr_info;
pub(super) use super::syntax;
pub(super) use super::terminal;
pub(super) use super::textprop;
pub(super) use super::value;
pub(super) use super::window_cmds;

// --- Submodules ---
mod arithmetic;
pub(crate) mod collections;
mod cons_list;
mod misc_pure;
mod strings;
mod types;

pub(crate) use arithmetic::*;
pub(crate) use collections::*;
pub(crate) use cons_list::*;
pub(crate) use misc_pure::*;
pub(crate) use strings::*;
pub(crate) use types::*;

mod buffers;
pub(crate) mod higher_order;
mod hooks;
pub(crate) mod keymaps;
mod misc_eval;
pub(crate) mod search;
mod stubs;
pub(crate) mod symbols;

pub(crate) use buffers::*;
pub(crate) use higher_order::*;
pub(crate) use hooks::*;
pub(crate) use keymaps::*;
pub(crate) use misc_eval::*;
pub(crate) use search::*;
pub(crate) use stubs::*;
pub(crate) use symbols::*;

// ===========================================================================
// Helpers
// ===========================================================================

pub(super) fn expect_string(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

pub(super) fn expect_string_comparison_operand(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        _ => value.as_symbol_name().map(str::to_owned).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *value],
            )
        }),
    }
}

pub(super) fn expect_strict_string(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

// Search / regex builtins are defined at the end of this file.

// ===========================================================================
// Dispatch table
// ===========================================================================

#[derive(EnumString)]
enum PureBuiltinId {
    #[strum(serialize = "+")]
    Add,
    #[strum(serialize = "-")]
    Sub,
    #[strum(serialize = "*")]
    Mul,
    #[strum(serialize = "/")]
    Div,
    #[strum(serialize = "%")]
    Percent,
    #[strum(serialize = "mod")]
    Mod,
    #[strum(serialize = "1+")]
    Add1,
    #[strum(serialize = "1-")]
    Sub1,
    #[strum(serialize = "=")]
    NumEq,
    #[strum(serialize = "<")]
    NumLt,
    #[strum(serialize = "<=")]
    NumLe,
    #[strum(serialize = ">")]
    NumGt,
    #[strum(serialize = ">=")]
    NumGe,
    #[strum(serialize = "/=")]
    NumNe,
    #[strum(serialize = "max")]
    Max,
    #[strum(serialize = "min")]
    Min,
    #[strum(serialize = "abs")]
    Abs,
    #[strum(serialize = "logand")]
    LogAnd,
    #[strum(serialize = "logior")]
    LogIor,
    #[strum(serialize = "logxor")]
    LogXor,
    #[strum(serialize = "lognot")]
    LogNot,
    #[strum(serialize = "ash")]
    Ash,
    #[strum(serialize = "null")]
    Null,
    #[strum(serialize = "not")]
    Not,
    #[strum(serialize = "ignore")]
    Ignore,
    #[strum(serialize = "atom")]
    Atom,
    #[strum(serialize = "consp")]
    Consp,
    #[strum(serialize = "listp")]
    Listp,
    #[strum(serialize = "list-of-strings-p")]
    ListOfStringsp,
    #[strum(serialize = "nlistp")]
    NListp,
    #[strum(serialize = "symbolp")]
    Symbolp,
    #[strum(serialize = "booleanp")]
    Booleanp,
    #[strum(serialize = "numberp")]
    Numberp,
    #[strum(serialize = "integerp")]
    Integerp,
    #[strum(serialize = "integer-or-null-p")]
    IntegerOrNullp,
    #[strum(serialize = "string-or-null-p")]
    StringOrNullp,
    #[strum(serialize = "floatp")]
    Floatp,
    #[strum(serialize = "stringp")]
    Stringp,
    #[strum(serialize = "vectorp")]
    Vectorp,
    #[strum(serialize = "characterp")]
    Characterp,
    #[strum(serialize = "char-uppercase-p")]
    CharUppercasep,
    #[strum(serialize = "functionp")]
    Functionp,
    #[strum(serialize = "keywordp")]
    Keywordp,
    #[strum(serialize = "hash-table-p")]
    HashTablep,
    #[strum(serialize = "bufferp")]
    Bufferp,
    #[strum(serialize = "type-of")]
    TypeOf,
    #[strum(serialize = "sequencep")]
    Sequencep,
    #[strum(serialize = "arrayp")]
    Arrayp,
    #[strum(serialize = "eq")]
    Eq,
    #[strum(serialize = "eql")]
    Eql,
    #[strum(serialize = "equal")]
    Equal,
    #[strum(serialize = "cons")]
    Cons,
    #[strum(serialize = "car")]
    Car,
    #[strum(serialize = "cdr")]
    Cdr,
    #[strum(serialize = "car-safe")]
    CarSafe,
    #[strum(serialize = "cdr-safe")]
    CdrSafe,
    #[strum(serialize = "setcar")]
    Setcar,
    #[strum(serialize = "setcdr")]
    Setcdr,
    #[strum(serialize = "list")]
    List,
    #[strum(serialize = "length")]
    Length,
    #[strum(serialize = "nth")]
    Nth,
    #[strum(serialize = "nthcdr")]
    Nthcdr,
    #[strum(serialize = "append")]
    Append,
    #[strum(serialize = "reverse")]
    Reverse,
    #[strum(serialize = "nreverse")]
    Nreverse,
    #[strum(serialize = "member")]
    Member,
    #[strum(serialize = "memq")]
    Memq,
    #[strum(serialize = "assoc")]
    Assoc,
    #[strum(serialize = "assq")]
    Assq,
    #[strum(serialize = "copy-sequence")]
    CopySequence,
    #[strum(serialize = "string-equal", serialize = "string=")]
    StringEqual,
    #[strum(serialize = "string-lessp", serialize = "string<")]
    StringLessp,
    #[strum(serialize = "string-greaterp", serialize = "string>")]
    StringGreaterp,
    #[strum(serialize = "substring")]
    Substring,
    #[strum(serialize = "concat")]
    Concat,
    #[strum(serialize = "string")]
    String,
    #[strum(serialize = "unibyte-string")]
    UnibyteString,
    #[strum(serialize = "string-to-number")]
    StringToNumber,
    #[strum(serialize = "number-to-string")]
    NumberToString,
    #[strum(serialize = "upcase")]
    Upcase,
    #[strum(serialize = "downcase")]
    Downcase,
    #[strum(serialize = "format")]
    Format,
    #[strum(serialize = "make-vector")]
    MakeVector,
    #[strum(serialize = "vector")]
    Vector,
    #[strum(serialize = "aref")]
    Aref,
    #[strum(serialize = "aset")]
    Aset,
    #[strum(serialize = "vconcat")]
    Vconcat,
    #[strum(serialize = "float")]
    Float,
    #[strum(serialize = "truncate")]
    Truncate,
    #[strum(serialize = "floor")]
    Floor,
    #[strum(serialize = "ceiling")]
    Ceiling,
    #[strum(serialize = "round")]
    Round,
    #[strum(serialize = "char-to-string")]
    CharToString,
    #[strum(serialize = "string-to-char")]
    StringToChar,
    #[strum(serialize = "make-hash-table")]
    MakeHashTable,
    #[strum(serialize = "gethash")]
    Gethash,
    #[strum(serialize = "puthash")]
    Puthash,
    #[strum(serialize = "remhash")]
    Remhash,
    #[strum(serialize = "clrhash")]
    Clrhash,
    #[strum(serialize = "hash-table-count")]
    HashTableCount,
    #[strum(serialize = "plist-get")]
    PlistGet,
    #[strum(serialize = "plist-put")]
    PlistPut,
    #[strum(serialize = "symbol-name")]
    SymbolName,
    #[strum(serialize = "make-symbol")]
    MakeSymbol,
    #[strum(serialize = "sqrt")]
    Sqrt,
    #[strum(serialize = "sin")]
    Sin,
    #[strum(serialize = "cos")]
    Cos,
    #[strum(serialize = "tan")]
    Tan,
    #[strum(serialize = "asin")]
    Asin,
    #[strum(serialize = "acos")]
    Acos,
    #[strum(serialize = "atan")]
    Atan,
    #[strum(serialize = "exp")]
    Exp,
    #[strum(serialize = "log")]
    Log,
    #[strum(serialize = "expt")]
    Expt,
    #[strum(serialize = "random")]
    Random,
    #[strum(serialize = "isnan")]
    Isnan,
    #[strum(serialize = "make-string")]
    MakeString,
    #[strum(serialize = "string-width")]
    StringWidth,
    #[strum(serialize = "delete")]
    Delete,
    #[strum(serialize = "delq")]
    Delq,
    #[strum(serialize = "elt")]
    Elt,
    #[strum(serialize = "nconc")]
    Nconc,
    #[strum(serialize = "bitmap-spec-p")]
    BitmapSpecP,
    #[strum(serialize = "byte-to-string")]
    ByteToString,
    #[strum(serialize = "clear-buffer-auto-save-failure")]
    ClearBufferAutoSaveFailure,
    #[strum(serialize = "clear-face-cache")]
    ClearFaceCache,
}

pub(super) fn is_pure_builtin_name(name: &str) -> bool {
    name.parse::<PureBuiltinId>().is_ok()
}

fn dispatch_builtin_id_pure(id: PureBuiltinId, args: Vec<Value>) -> EvalResult {
    match id {
        PureBuiltinId::Add => builtin_add(args),
        PureBuiltinId::Sub => builtin_sub(args),
        PureBuiltinId::Mul => builtin_mul(args),
        PureBuiltinId::Div => builtin_div(args),
        PureBuiltinId::Percent => builtin_percent(args),
        PureBuiltinId::Mod => builtin_mod(args),
        PureBuiltinId::Add1 => builtin_add1(args),
        PureBuiltinId::Sub1 => builtin_sub1(args),
        PureBuiltinId::NumEq => builtin_num_eq(args),
        PureBuiltinId::NumLt => builtin_num_lt(args),
        PureBuiltinId::NumLe => builtin_num_le(args),
        PureBuiltinId::NumGt => builtin_num_gt(args),
        PureBuiltinId::NumGe => builtin_num_ge(args),
        PureBuiltinId::NumNe => builtin_num_ne(args),
        PureBuiltinId::Max => builtin_max(args),
        PureBuiltinId::Min => builtin_min(args),
        PureBuiltinId::Abs => builtin_abs(args),
        PureBuiltinId::LogAnd => builtin_logand(args),
        PureBuiltinId::LogIor => builtin_logior(args),
        PureBuiltinId::LogXor => builtin_logxor(args),
        PureBuiltinId::LogNot => builtin_lognot(args),
        PureBuiltinId::Ash => builtin_ash(args),
        PureBuiltinId::Null => builtin_null(args),
        PureBuiltinId::Not => builtin_not(args),
        PureBuiltinId::Ignore => builtin_ignore(args),
        PureBuiltinId::Atom => builtin_atom(args),
        PureBuiltinId::Consp => builtin_consp(args),
        PureBuiltinId::Listp => builtin_listp(args),
        PureBuiltinId::ListOfStringsp => builtin_list_of_strings_p(args),
        PureBuiltinId::NListp => builtin_nlistp(args),
        PureBuiltinId::Symbolp => builtin_symbolp(args),
        PureBuiltinId::Booleanp => builtin_booleanp(args),
        PureBuiltinId::Numberp => builtin_numberp(args),
        PureBuiltinId::Integerp => builtin_integerp(args),
        PureBuiltinId::IntegerOrNullp => builtin_integer_or_null_p(args),
        PureBuiltinId::StringOrNullp => builtin_string_or_null_p(args),
        PureBuiltinId::Floatp => builtin_floatp(args),
        PureBuiltinId::Stringp => builtin_stringp(args),
        PureBuiltinId::Vectorp => builtin_vectorp(args),
        PureBuiltinId::Characterp => builtin_characterp(args),
        PureBuiltinId::CharUppercasep => builtin_char_uppercase_p(args),
        PureBuiltinId::Functionp => builtin_functionp(args),
        PureBuiltinId::Keywordp => builtin_keywordp(args),
        PureBuiltinId::HashTablep => builtin_hash_table_p(args),
        PureBuiltinId::Bufferp => builtin_bufferp(args),
        PureBuiltinId::TypeOf => builtin_type_of(args),
        PureBuiltinId::Sequencep => builtin_sequencep(args),
        PureBuiltinId::Arrayp => builtin_arrayp(args),
        PureBuiltinId::Eq => builtin_eq(args),
        PureBuiltinId::Eql => builtin_eql(args),
        PureBuiltinId::Equal => builtin_equal(args),
        PureBuiltinId::Cons => builtin_cons(args),
        PureBuiltinId::Car => builtin_car(args),
        PureBuiltinId::Cdr => builtin_cdr(args),
        PureBuiltinId::CarSafe => builtin_car_safe(args),
        PureBuiltinId::CdrSafe => builtin_cdr_safe(args),
        PureBuiltinId::Setcar => builtin_setcar(args),
        PureBuiltinId::Setcdr => builtin_setcdr(args),
        PureBuiltinId::List => builtin_list(args),
        PureBuiltinId::Length => builtin_length(args),
        PureBuiltinId::Nth => builtin_nth(args),
        PureBuiltinId::Nthcdr => builtin_nthcdr(args),
        PureBuiltinId::Append => builtin_append(args),
        PureBuiltinId::Reverse => builtin_reverse(args),
        PureBuiltinId::Nreverse => builtin_nreverse(args),
        PureBuiltinId::Member => builtin_member(args),
        PureBuiltinId::Memq => builtin_memq(args),
        PureBuiltinId::Assoc => builtin_assoc(args),
        PureBuiltinId::Assq => builtin_assq(args),
        PureBuiltinId::CopySequence => builtin_copy_sequence(args),
        PureBuiltinId::StringEqual => builtin_string_equal(args),
        PureBuiltinId::StringLessp => builtin_string_lessp(args),
        PureBuiltinId::StringGreaterp => builtin_string_greaterp(args),
        PureBuiltinId::Substring => builtin_substring(args),
        PureBuiltinId::Concat => builtin_concat(args),
        PureBuiltinId::String => builtin_string(args),
        PureBuiltinId::UnibyteString => builtin_unibyte_string(args),
        PureBuiltinId::StringToNumber => builtin_string_to_number(args),
        PureBuiltinId::NumberToString => builtin_number_to_string(args),
        PureBuiltinId::Upcase => builtin_upcase(args),
        PureBuiltinId::Downcase => builtin_downcase(args),
        PureBuiltinId::Format => builtin_format(args),
        PureBuiltinId::MakeVector => builtin_make_vector(args),
        PureBuiltinId::Vector => builtin_vector(args),
        PureBuiltinId::Aref => builtin_aref(args),
        PureBuiltinId::Aset => builtin_aset(args),
        PureBuiltinId::Vconcat => builtin_vconcat(args),
        PureBuiltinId::Float => builtin_float(args),
        PureBuiltinId::Truncate => builtin_truncate(args),
        PureBuiltinId::Floor => builtin_floor(args),
        PureBuiltinId::Ceiling => builtin_ceiling(args),
        PureBuiltinId::Round => builtin_round(args),
        PureBuiltinId::CharToString => builtin_char_to_string(args),
        PureBuiltinId::StringToChar => builtin_string_to_char(args),
        PureBuiltinId::MakeHashTable => builtin_make_hash_table(args),
        PureBuiltinId::Gethash => builtin_gethash(args),
        PureBuiltinId::Puthash => builtin_puthash(args),
        PureBuiltinId::Remhash => builtin_remhash(args),
        PureBuiltinId::Clrhash => builtin_clrhash(args),
        PureBuiltinId::HashTableCount => builtin_hash_table_count(args),
        PureBuiltinId::PlistGet => builtin_plist_get(args),
        PureBuiltinId::PlistPut => builtin_plist_put(args),
        PureBuiltinId::SymbolName => builtin_symbol_name(args),
        PureBuiltinId::MakeSymbol => builtin_make_symbol(args),
        PureBuiltinId::Sqrt => builtin_sqrt(args),
        PureBuiltinId::Sin => builtin_sin(args),
        PureBuiltinId::Cos => builtin_cos(args),
        PureBuiltinId::Tan => builtin_tan(args),
        PureBuiltinId::Asin => builtin_asin(args),
        PureBuiltinId::Acos => builtin_acos(args),
        PureBuiltinId::Atan => builtin_atan(args),
        PureBuiltinId::Exp => builtin_exp(args),
        PureBuiltinId::Log => builtin_log(args),
        PureBuiltinId::Expt => builtin_expt(args),
        PureBuiltinId::Random => builtin_random(args),
        PureBuiltinId::Isnan => builtin_isnan(args),
        PureBuiltinId::MakeString => builtin_make_string(args),
        PureBuiltinId::StringWidth => builtin_string_width(args),
        PureBuiltinId::Delete => builtin_delete(args),
        PureBuiltinId::Delq => builtin_delq(args),
        PureBuiltinId::Elt => builtin_elt(args),
        PureBuiltinId::Nconc => builtin_nconc(args),
        PureBuiltinId::BitmapSpecP => builtin_bitmap_spec_p(args),
        PureBuiltinId::ByteToString => builtin_byte_to_string(args),
        PureBuiltinId::ClearBufferAutoSaveFailure => builtin_clear_buffer_auto_save_failure(args),
        PureBuiltinId::ClearFaceCache => builtin_clear_face_cache(args),
    }
}

fn dispatch_builtin_id_eval(
    eval: &mut super::eval::Context,
    id: PureBuiltinId,
    args: Vec<Value>,
) -> EvalResult {
    match id {
        PureBuiltinId::Max => builtin_max_eval(eval, args),
        PureBuiltinId::Min => builtin_min_eval(eval, args),
        PureBuiltinId::NumEq => builtin_num_eq_eval(eval, args),
        PureBuiltinId::NumLt => builtin_num_lt_eval(eval, args),
        PureBuiltinId::NumLe => builtin_num_le_eval(eval, args),
        PureBuiltinId::NumGt => builtin_num_gt_eval(eval, args),
        PureBuiltinId::NumGe => builtin_num_ge_eval(eval, args),
        PureBuiltinId::NumNe => builtin_num_ne_eval(eval, args),
        // Arithmetic with eval-aware marker position lookup
        PureBuiltinId::Add => super::builtins::arithmetic::builtin_add_eval(eval, args),
        PureBuiltinId::Sub => super::builtins::arithmetic::builtin_sub_eval(eval, args),
        other => dispatch_builtin_id_pure(other, args),
    }
}

/// Try to dispatch a builtin function by name. Returns None if not a known builtin.
pub(crate) fn dispatch_builtin(
    eval: &mut super::eval::Context,
    name: &str,
    args: Vec<Value>,
) -> Option<EvalResult> {
    // Fast path: check the function pointer registry first (O(1) hash lookup).
    // Builtins registered via defsubr() are dispatched here without any
    // string-matching. The match block below is the legacy fallback for
    // builtins not yet migrated to defsubr.
    if let Some(result) = eval.dispatch_subr(name, args.clone()) {
        return Some(result);
    }

    if let Ok(id) = name.parse::<PureBuiltinId>() {
        return Some(dispatch_builtin_id_eval(eval, id, args));
    }

    // Pure builtins (no evaluator needed)
    Some(match name {
        // Arithmetic
        "+" => builtin_add(args),
        "-" => builtin_sub(args),
        "*" => builtin_mul(args),
        "/" => builtin_div(args),
        "%" => builtin_percent(args),
        "mod" => builtin_mod(args),
        "1+" => builtin_add1(args),
        "1-" => builtin_sub1(args),
        "max" => builtin_max_eval(eval, args),
        "min" => builtin_min_eval(eval, args),
        "abs" => builtin_abs(args),

        // Logical / bitwise
        "logand" => builtin_logand(args),
        "logior" => builtin_logior(args),
        "logxor" => builtin_logxor(args),
        "lognot" => builtin_lognot(args),
        "ash" => builtin_ash(args),

        // Numeric comparisons
        "=" => builtin_num_eq_eval(eval, args),
        "<" => builtin_num_lt_eval(eval, args),
        "<=" => builtin_num_le_eval(eval, args),
        ">" => builtin_num_gt_eval(eval, args),
        ">=" => builtin_num_ge_eval(eval, args),
        "/=" => builtin_num_ne_eval(eval, args),

        // Type predicates (typed subset is dispatched above)
        // Type predicates (typed subset is dispatched above)
        // Type predicates (typed subset is dispatched above)
        "integer-or-marker-p" => builtin_integer_or_marker_p(args),
        "number-or-marker-p" => builtin_number_or_marker_p(args),
        "vector-or-char-table-p" => builtin_vector_or_char_table_p(args),
        "module-function-p" => builtin_module_function_p(args),
        "user-ptrp" => builtin_user_ptrp(args),
        "symbol-with-pos-p" => builtin_symbol_with_pos_p(args),
        "symbol-with-pos-pos" => builtin_symbol_with_pos_pos(args),

        // Equality (typed subset is dispatched above)
        "function-equal" => builtin_function_equal(args),

        // Cons / List
        "cons" => builtin_cons(args),
        "car" => builtin_car(args),
        "cdr" => builtin_cdr(args),
        "car-safe" => builtin_car_safe(args),
        "cdr-safe" => builtin_cdr_safe(args),
        "setcar" => builtin_setcar(args),
        "setcdr" => builtin_setcdr(args),
        "list" => builtin_list(args),
        "length" => builtin_length(args),
        "length<" => builtin_length_lt(args),
        "length=" => builtin_length_eq(args),
        "length>" => builtin_length_gt(args),
        "nth" => builtin_nth(args),
        "nthcdr" => builtin_nthcdr(args),
        "append" => builtin_append(args),
        "reverse" => builtin_reverse(args),
        "nreverse" => builtin_nreverse(args),
        "member" => builtin_member(args),
        "memq" => builtin_memq(args),
        "memql" => builtin_memql(args),
        "assq" => builtin_assq(args),
        "copy-sequence" => builtin_copy_sequence(args),
        "substring-no-properties" => builtin_substring_no_properties(args),

        // String (typed subset is dispatched above)

        // Vector (typed subset is dispatched above)

        // Hash table (typed subset is dispatched above)

        // Conversion (typed subset is dispatched above)

        // Property lists
        "plist-get" => builtin_plist_get(args),
        "plist-put" => builtin_plist_put(args),

        // Symbol (pure)
        "cl-type-of" => builtin_cl_type_of(args),
        "symbol-name" => builtin_symbol_name(args),
        "make-symbol" => builtin_make_symbol(args),

        // Math (typed subset is dispatched above)

        // Extended string (typed subset is dispatched above)

        // Extended list (typed subset is dispatched above)

        // Output / misc
        "identity" => builtin_identity(args),
        "message" => builtin_message(args),
        "message-box" => builtin_message_box(args),
        "message-or-box" => builtin_message_or_box(args),
        "current-message" => builtin_current_message(args),
        "ngettext" => builtin_ngettext(args),
        "secure-hash-algorithms" => builtin_secure_hash_algorithms(args),
        "prefix-numeric-value" => builtin_prefix_numeric_value(args),
        "command-error-default-function" => builtin_command_error_default_function(args),
        "clear-string" => builtin_clear_string(args),
        "combine-after-change-execute" => builtin_combine_after_change_execute(args),
        "princ" => builtin_princ(args),
        "prin1" => builtin_prin1(args),
        "prin1-to-string" => builtin_prin1_to_string(args),
        "print" => builtin_print(args),
        "terpri" => builtin_terpri(args),
        "write-char" => builtin_write_char(args),
        "propertize" => builtin_propertize(args),
        "string-to-syntax" => builtin_string_to_syntax(args),
        "syntax-class-to-char" => super::syntax::builtin_syntax_class_to_char(args),
        // matching-paren is now dispatched in dispatch_builtin (eval-dependent)
        // "matching-paren" => handled in dispatch_builtin
        "copy-syntax-table" => super::syntax::builtin_copy_syntax_table(args),
        "syntax-table-p" => super::syntax::builtin_syntax_table_p(args),
        "standard-syntax-table" => super::syntax::builtin_standard_syntax_table(args),
        "current-time" => super::timefns::builtin_current_time(args),
        "current-cpu-time" => builtin_current_cpu_time(args),
        "current-idle-time" => builtin_current_idle_time(args),
        "get-internal-run-time" => builtin_get_internal_run_time(args),
        "float-time" => super::timefns::builtin_float_time(args),
        "daemonp" => builtin_daemonp(args),
        "daemon-initialized" => builtin_daemon_initialized(args),
        "flush-standard-output" => builtin_flush_standard_output(args),
        "force-mode-line-update" => builtin_force_mode_line_update(args),
        "force-window-update" => super::dispnew::pure::builtin_force_window_update(args),
        "invocation-directory" => builtin_invocation_directory(args),
        "invocation-name" => builtin_invocation_name(args),

        // File I/O (pure)
        "file-name-directory" => super::fileio::builtin_file_name_directory(args),
        "file-name-nondirectory" => super::fileio::builtin_file_name_nondirectory(args),
        "file-name-as-directory" => super::fileio::builtin_file_name_as_directory(args),
        "directory-file-name" => super::fileio::builtin_directory_file_name(args),
        "file-name-concat" => super::fileio::builtin_file_name_concat(args),
        "file-name-absolute-p" => super::fileio::builtin_file_name_absolute_p(args),
        "directory-name-p" => super::fileio::builtin_directory_name_p(args),
        "substitute-in-file-name" => super::fileio::builtin_substitute_in_file_name(args),
        "set-file-acl" => super::fileio::builtin_set_file_acl(args),
        "set-file-selinux-context" => super::fileio::builtin_set_file_selinux_context(args),
        "visited-file-modtime" => super::fileio::builtin_visited_file_modtime(args),
        "make-temp-name" => super::fileio::builtin_make_temp_name(args),
        "next-read-file-uses-dialog-p" => super::fileio::builtin_next_read_file_uses_dialog_p(args),
        "unhandled-file-name-directory" => {
            super::fileio::builtin_unhandled_file_name_directory(args)
        }
        "get-truename-buffer" => super::fileio::builtin_get_truename_buffer(args),

        // Keymap (pure — no evaluator needed)
        "single-key-description" => builtin_single_key_description(args),
        "key-description" => builtin_key_description(args),
        "event-convert-list" => builtin_event_convert_list(args),
        "text-char-description" => builtin_text_char_description(args),

        // Process (pure — no evaluator needed)
        "set-binary-mode" => super::process::builtin_set_binary_mode(args),

        // Timer (pure — no evaluator needed)
        // Undo system (pure — no evaluator needed)
        "undo-boundary" => super::undo::builtin_undo_boundary(args),
        // Keyboard macro (pure — no evaluator needed)

        // Case table (pure)
        "case-table-p" => super::casetab::builtin_case_table_p(args),
        "current-case-table" => super::casetab::builtin_current_case_table(args),
        "standard-case-table" => super::casetab::builtin_standard_case_table(args),
        "set-case-table" => super::casetab::builtin_set_case_table(args),

        // Category (pure)
        "define-category" => super::category::builtin_define_category(args),
        "category-docstring" => super::category::builtin_category_docstring(args),
        "copy-category-table" => super::category::builtin_copy_category_table(args),
        "category-table-p" => super::category::builtin_category_table_p(args),
        "category-table" => super::category::builtin_category_table(args),
        "make-category-table" => super::category::builtin_make_category_table(args),
        "set-category-table" => super::category::builtin_set_category_table(args),
        "make-category-set" => super::category::builtin_make_category_set(args),
        "category-set-mnemonics" => super::category::builtin_category_set_mnemonics(args),

        // Dispnew (pure)
        "redraw-display" => super::dispnew::pure::builtin_redraw_display(args),
        "open-termscript" => super::dispnew::pure::builtin_open_termscript(args),
        "ding" => super::dispnew::pure::builtin_ding(args),
        "frame--z-order-lessp" => super::dispnew::pure::builtin_frame_z_order_lessp(args),
        // Display/terminal (pure)
        "x-export-frames" => super::display::builtin_x_export_frames(args),
        "x-backspace-delete-keys-p" => super::display::builtin_x_backspace_delete_keys_p(args),
        "x-change-window-property" => super::display::builtin_x_change_window_property(args),
        "x-focus-frame" => super::display::builtin_x_focus_frame(args),
        "x-get-local-selection" => super::display::builtin_x_get_local_selection(args),
        "x-get-modifier-masks" => super::display::builtin_x_get_modifier_masks(args),
        "x-get-selection-internal" => super::display::builtin_x_get_selection_internal(args),
        "x-display-list" => super::display::builtin_x_display_list(args),
        "x-disown-selection-internal" => super::display::builtin_x_disown_selection_internal(args),
        "x-delete-window-property" => super::display::builtin_x_delete_window_property(args),
        "x-frame-edges" => super::display::builtin_x_frame_edges(args),
        "x-frame-geometry" => super::display::builtin_x_frame_geometry(args),
        "x-frame-list-z-order" => super::display::builtin_x_frame_list_z_order(args),
        "x-frame-restack" => super::display::builtin_x_frame_restack(args),
        "x-family-fonts" => super::display::builtin_x_family_fonts(args),
        "x-get-atom-name" => super::display::builtin_x_get_atom_name(args),
        "x-mouse-absolute-pixel-position" => {
            super::display::builtin_x_mouse_absolute_pixel_position(args)
        }
        "x-get-resource" => super::display::builtin_x_get_resource(args),
        "x-list-fonts" => super::display::builtin_x_list_fonts(args),
        "x-open-connection" => super::display::builtin_x_open_connection(args),
        "x-parse-geometry" => super::display::builtin_x_parse_geometry(args),
        "x-own-selection-internal" => super::display::builtin_x_own_selection_internal(args),
        "x-popup-dialog" => super::display::builtin_x_popup_dialog(args),
        "x-popup-menu" => super::display::builtin_x_popup_menu(args),
        "x-register-dnd-atom" => super::display::builtin_x_register_dnd_atom(args),
        "x-selection-exists-p" => super::display::builtin_x_selection_exists_p(args),
        "x-selection-owner-p" => super::display::builtin_x_selection_owner_p(args),
        "x-hide-tip" => super::display::builtin_x_hide_tip(args),
        "x-internal-focus-input-context" => {
            super::display::builtin_x_internal_focus_input_context(args)
        }
        "x-send-client-message" => super::display::builtin_x_send_client_message(args),
        "x-show-tip" => super::display::builtin_x_show_tip(args),
        "x-set-mouse-absolute-pixel-position" => {
            super::display::builtin_x_set_mouse_absolute_pixel_position(args)
        }
        "x-synchronize" => super::display::builtin_x_synchronize(args),
        "x-translate-coordinates" => super::display::builtin_x_translate_coordinates(args),
        "x-uses-old-gtk-dialog" => super::display::builtin_x_uses_old_gtk_dialog(args),
        "x-close-connection" => super::display::builtin_x_close_connection(args),
        "x-window-property" => super::display::builtin_x_window_property(args),
        "x-window-property-attributes" => {
            super::display::builtin_x_window_property_attributes(args)
        }
        "terminal-list" => super::terminal::pure::builtin_terminal_list(args),
        "x-server-version" => super::display::builtin_x_server_version(args),
        "x-server-input-extension-version" => {
            super::display::builtin_x_server_input_extension_version(args)
        }
        "x-server-vendor" => super::display::builtin_x_server_vendor(args),
        "display-color-cells" => super::display::builtin_display_color_cells(args),
        "x-display-mm-height" => super::display::builtin_x_display_mm_height(args),
        "x-display-mm-width" => super::display::builtin_x_display_mm_width(args),
        "x-display-planes" => super::display::builtin_x_display_planes(args),
        "x-display-screens" => super::display::builtin_x_display_screens(args),
        "x-wm-set-size-hint" => super::display::builtin_x_wm_set_size_hint(args),

        // Image (pure)
        "image-size" => super::image::builtin_image_size(args),
        "image-mask-p" => super::image::builtin_image_mask_p(args),
        "image-flush" => super::image::builtin_image_flush(args),
        "clear-image-cache" => super::image::builtin_clear_image_cache(args),
        "image-cache-size" => super::image::builtin_image_cache_size(args),
        "image-metadata" => super::image::builtin_image_metadata(args),
        "imagep" => super::image::builtin_imagep(args),
        "image-transforms-p" => super::image::builtin_image_transforms_p(args),

        // Character encoding
        "char-width" => crate::encoding::builtin_char_width(args),
        "string-bytes" => crate::encoding::builtin_string_bytes(args),
        "multibyte-string-p" => crate::encoding::builtin_multibyte_string_p(args),
        "encode-coding-string" => crate::encoding::builtin_encode_coding_string(args),
        "decode-coding-string" => crate::encoding::builtin_decode_coding_string(args),
        "char-or-string-p" => crate::encoding::builtin_char_or_string_p(args),
        "max-char" => crate::encoding::builtin_max_char(args),

        // Extra builtins
        "take" => super::builtins_extra::builtin_take(args),
        "assoc-string" => super::builtins_extra::builtin_assoc_string(args),
        "string-search" => super::builtins_extra::builtin_string_search(args),
        "bare-symbol" => super::builtins_extra::builtin_bare_symbol(args),
        "bare-symbol-p" => super::builtins_extra::builtin_bare_symbol_p(args),
        "byteorder" => super::builtins_extra::builtin_byteorder(args),
        "car-less-than-car" => super::builtins_extra::builtin_car_less_than_car(args),
        "proper-list-p" => super::builtins_extra::builtin_proper_list_p(args),
        "subrp" => super::builtins_extra::builtin_subrp(args),
        "byte-code-function-p" => super::builtins_extra::builtin_byte_code_function_p(args),
        "closurep" => super::builtins_extra::builtin_closurep(args),
        "natnump" => super::builtins_extra::builtin_natnump(args),
        "fixnump" => super::builtins_extra::builtin_fixnump(args),
        "bignump" => super::builtins_extra::builtin_bignump(args),
        "user-login-name" => super::builtins_extra::builtin_user_login_name(args),
        "user-real-login-name" => super::builtins_extra::builtin_user_real_login_name(args),
        "user-full-name" => super::builtins_extra::builtin_user_full_name(args),
        "system-name" => super::builtins_extra::builtin_system_name(args),
        "emacs-pid" => super::builtins_extra::builtin_emacs_pid(args),
        "memory-use-counts" => super::builtins_extra::builtin_memory_use_counts(args),
        // Note: overlayp is in the eval-dependent section above
        // Time/date (pure)
        "time-add" => super::timefns::builtin_time_add(args),
        "time-subtract" => super::timefns::builtin_time_subtract(args),
        "time-less-p" => super::timefns::builtin_time_less_p(args),
        "time-equal-p" => super::timefns::builtin_time_equal_p(args),
        "current-time-string" => super::timefns::builtin_current_time_string(args),
        "current-time-zone" => super::timefns::builtin_current_time_zone(args),
        "encode-time" => super::timefns::builtin_encode_time(args),
        "decode-time" => super::timefns::builtin_decode_time(args),
        "time-convert" => super::timefns::builtin_time_convert(args),
        "set-time-zone-rule" => super::timefns::builtin_set_time_zone_rule(args),

        // Float/math (pure)
        "copysign" => super::floatfns::builtin_copysign(args),
        "frexp" => super::floatfns::builtin_frexp(args),
        "ldexp" => super::floatfns::builtin_ldexp(args),
        "logb" => super::floatfns::builtin_logb(args),
        "fceiling" => super::floatfns::builtin_fceiling(args),
        "ffloor" => super::floatfns::builtin_ffloor(args),
        "fround" => super::floatfns::builtin_fround(args),
        "ftruncate" => super::floatfns::builtin_ftruncate(args),

        // Case/char (pure)
        "capitalize" => super::casefiddle::builtin_capitalize(args),
        "upcase-initials" => super::casefiddle::builtin_upcase_initials(args),
        "char-resolve-modifiers" => super::casefiddle::builtin_char_resolve_modifiers(args),

        // Font/face (pure)
        "fontp" => super::font::builtin_fontp(args),
        "font-spec" => super::font::builtin_font_spec(args),
        "font-get" => super::font::builtin_font_get(args),
        "font-put" => super::font::builtin_font_put(args),
        "list-fonts" => super::font::builtin_list_fonts(args),
        "find-font" => super::font::builtin_find_font(args),
        "clear-font-cache" => super::font::builtin_clear_font_cache(args),
        "font-family-list" => super::font::builtin_font_family_list(args),
        "font-xlfd-name" => super::font::builtin_font_xlfd_name(args),
        "close-font" => super::font::builtin_close_font(args),
        "font-at" => {
            return Some(super::font::builtin_font_at_eval(eval, args));
        }
        "xw-display-color-p" => {
            return Some(builtin_xw_display_color_p_eval(eval, args));
        }
        "internal-make-lisp-face" => {
            return Some(super::font::builtin_internal_make_lisp_face_eval(
                eval, args,
            ));
        }
        "internal-lisp-face-p" => super::font::builtin_internal_lisp_face_p(args),
        "internal-copy-lisp-face" => {
            return Some(super::font::builtin_internal_copy_lisp_face_eval(
                eval, args,
            ));
        }
        "internal-set-lisp-face-attribute" => {
            return Some(super::font::builtin_internal_set_lisp_face_attribute_eval(
                eval, args,
            ));
        }
        "internal-get-lisp-face-attribute" => {
            return Some(super::font::builtin_internal_get_lisp_face_attribute_eval(
                eval, args,
            ));
        }
        "internal-lisp-face-attribute-values" => {
            super::font::builtin_internal_lisp_face_attribute_values(args)
        }
        "internal-lisp-face-equal-p" => super::font::builtin_internal_lisp_face_equal_p(args),
        "internal-lisp-face-empty-p" => super::font::builtin_internal_lisp_face_empty_p(args),
        "internal-merge-in-global-face" => {
            return Some(super::font::builtin_internal_merge_in_global_face_eval(
                eval, args,
            ));
        }
        "face-attribute-relative-p" => super::font::builtin_face_attribute_relative_p(args),
        "merge-face-attribute" => super::font::builtin_merge_face_attribute(args),
        "color-gray-p" => super::font::builtin_color_gray_p(args),
        "color-supported-p" => super::font::builtin_color_supported_p(args),
        "color-distance" => super::font::builtin_color_distance(args),
        "color-values-from-color-spec" => super::font::builtin_color_values_from_color_spec(args),
        "internal-face-x-get-resource" => super::font::builtin_internal_face_x_get_resource(args),
        "internal-set-font-selection-order" => {
            super::font::builtin_internal_set_font_selection_order(args)
        }
        "internal-set-alternative-font-family-alist" => {
            super::font::builtin_internal_set_alternative_font_family_alist(args)
        }
        "internal-set-alternative-font-registry-alist" => {
            super::font::builtin_internal_set_alternative_font_registry_alist(args)
        }

        // Directory/file attributes (pure)
        "file-name-completion" => super::dired::builtin_file_name_completion(eval, args),
        "file-attributes-lessp" => super::dired::builtin_file_attributes_lessp(args),
        "system-users" => super::dired::builtin_system_users(args),
        "system-groups" => super::dired::builtin_system_groups(args),

        // Display engine (pure)
        "format-mode-line" => super::xdisp::builtin_format_mode_line(args),
        "invisible-p" => super::xdisp::builtin_invisible_p(args),
        "line-pixel-height" => super::xdisp::builtin_line_pixel_height(args),
        "move-point-visually" => super::xdisp::builtin_move_point_visually(args),
        "lookup-image-map" => super::xdisp::builtin_lookup_image_map(args),
        "current-bidi-paragraph-direction" => {
            super::xdisp::builtin_current_bidi_paragraph_direction(args)
        }
        "bidi-resolved-levels" => super::xdisp::builtin_bidi_resolved_levels(args),
        "bidi-find-overridden-directionality" => {
            super::xdisp::builtin_bidi_find_overridden_directionality(args)
        }
        "move-to-window-line" => super::xdisp::builtin_move_to_window_line(args),
        "tool-bar-height" => super::xdisp::builtin_tool_bar_height(args),
        "tab-bar-height" => super::xdisp::builtin_tab_bar_height(args),
        "line-number-display-width" => super::xdisp::builtin_line_number_display_width(args),
        "long-line-optimizations-p" => super::xdisp::builtin_long_line_optimizations_p(args),

        // Charset (pure)
        "charsetp" => super::charset::builtin_charsetp(args),
        "charset-priority-list" => super::charset::builtin_charset_priority_list(args),
        "set-charset-priority" => super::charset::builtin_set_charset_priority(args),
        "char-charset" => super::charset::builtin_char_charset(args),
        "charset-plist" => super::charset::builtin_charset_plist(args),
        "charset-id-internal" => super::charset::builtin_charset_id_internal(args),
        "define-charset-alias" => super::charset::builtin_define_charset_alias(args),
        "define-charset-internal" => super::charset::builtin_define_charset_internal(args),
        "declare-equiv-charset" => super::charset::builtin_declare_equiv_charset(args),
        "find-charset-region" => super::charset::builtin_find_charset_region(args),
        "find-charset-string" => super::charset::builtin_find_charset_string(args),
        "decode-big5-char" => super::charset::builtin_decode_big5_char(args),
        "decode-char" => super::charset::builtin_decode_char(args),
        "decode-sjis-char" => super::charset::builtin_decode_sjis_char(args),
        "encode-big5-char" => super::charset::builtin_encode_big5_char(args),
        "encode-char" => super::charset::builtin_encode_char(args),
        "encode-sjis-char" => super::charset::builtin_encode_sjis_char(args),
        "get-unused-iso-final-char" => super::charset::builtin_get_unused_iso_final_char(args),
        "clear-charset-maps" => super::charset::builtin_clear_charset_maps(args),
        "charset-after" => super::charset::builtin_charset_after(args),

        // CCL (pure)
        "ccl-program-p" => builtin_ccl_program_p_eval(eval, args),
        "ccl-execute" => builtin_ccl_execute_eval(eval, args),
        "ccl-execute-on-string" => builtin_ccl_execute_on_string_eval(eval, args),
        "register-ccl-program" => builtin_register_ccl_program_eval(eval, args),
        "register-code-conversion-map" => builtin_register_code_conversion_map_eval(eval, args),

        // XML/decompress (pure)
        "libxml-parse-html-region" => super::xml::builtin_libxml_parse_html_region(args),
        "libxml-parse-xml-region" => super::xml::builtin_libxml_parse_xml_region(args),
        "libxml-available-p" => super::xml::builtin_libxml_available_p(args),
        "zlib-available-p" => super::xml::builtin_zlib_available_p(args),
        "zlib-decompress-region" => super::xml::builtin_zlib_decompress_region(args),

        // Custom system (pure)
        // frame.c missing builtins (pure stubs)
        "frame-id" => builtin_frame_id(args),
        "frame-root-frame" => builtin_frame_root_frame(args),
        "set-frame-size-and-position-pixelwise" => {
            builtin_set_frame_size_and_position_pixelwise(args)
        }
        "mouse-position-in-root-frame" => builtin_mouse_position_in_root_frame(args),

        // xfaces.c missing builtin
        "x-load-color-file" => super::font::builtin_x_load_color_file(args),

        // Internal compatibility surface (pure)
        "define-fringe-bitmap" => builtin_define_fringe_bitmap(args),
        "destroy-fringe-bitmap" => builtin_destroy_fringe_bitmap(args),
        "display--line-is-continued-p" => builtin_display_line_is_continued_p(args),
        "display--update-for-mouse-movement" => builtin_display_update_for_mouse_movement(args),
        "do-auto-save" => builtin_do_auto_save(args),
        "external-debugging-output" => builtin_external_debugging_output(args),
        "describe-buffer-bindings" => builtin_describe_buffer_bindings(args),
        "describe-vector" => builtin_describe_vector(args),
        "delete-terminal" => super::terminal::pure::builtin_delete_terminal(args),
        "face-attributes-as-vector" => builtin_face_attributes_as_vector(args),
        "font-face-attributes" => builtin_font_face_attributes(args),
        "font-get-glyphs" => builtin_font_get_glyphs(args),
        "font-get-system-font" => builtin_font_get_system_font(args),
        "font-get-system-normal-font" => builtin_font_get_system_normal_font(args),
        "font-has-char-p" => builtin_font_has_char_p(args),
        "font-info" => builtin_font_info(args),
        "font-match-p" => builtin_font_match_p(args),
        "font-shape-gstring" => builtin_font_shape_gstring(args),
        "font-variation-glyphs" => builtin_font_variation_glyphs(args),
        "fontset-font" => builtin_fontset_font(args),
        "fontset-info" => builtin_fontset_info(args),
        "fontset-list" => builtin_fontset_list(args),
        "fontset-list-all" => builtin_fontset_list_all(args),
        "frame--set-was-invisible" => builtin_frame_set_was_invisible(args),
        "frame-after-make-frame" => builtin_frame_after_make_frame(args),
        "frame-ancestor-p" => builtin_frame_ancestor_p(args),
        "frame-bottom-divider-width" => builtin_frame_bottom_divider_width(args),
        "frame-child-frame-border-width" => builtin_frame_child_frame_border_width(args),
        "frame-focus" => builtin_frame_focus(args),
        "frame-font-cache" => builtin_frame_font_cache(args),
        "frame-fringe-width" => builtin_frame_fringe_width(args),
        "frame-internal-border-width" => builtin_frame_internal_border_width(args),
        "frame-or-buffer-changed-p" => builtin_frame_or_buffer_changed_p(args),
        "frame-parent" => builtin_frame_parent(args),
        "frame-pointer-visible-p" => builtin_frame_pointer_visible_p(args),
        "frame-right-divider-width" => builtin_frame_right_divider_width(args),
        "frame-scale-factor" => builtin_frame_scale_factor(args),
        "frame-scroll-bar-height" => builtin_frame_scroll_bar_height(args),
        "frame-scroll-bar-width" => builtin_frame_scroll_bar_width(args),
        "frame-window-state-change" => builtin_frame_window_state_change(args),
        "fringe-bitmaps-at-pos" => builtin_fringe_bitmaps_at_pos(args),
        "gap-position" => builtin_gap_position(args),
        "gap-size" => builtin_gap_size(args),
        "garbage-collect-heapsize" => builtin_garbage_collect_heapsize(args),
        "garbage-collect-maybe" => builtin_garbage_collect_maybe(args),
        "get-unicode-property-internal" => builtin_get_unicode_property_internal(args),
        "gnutls-available-p" => builtin_gnutls_available_p(args),
        "gnutls-asynchronous-parameters" => builtin_gnutls_asynchronous_parameters(args),
        "gnutls-boot" => return None, // dispatched through eval path in process.rs
        "gnutls-bye" => builtin_gnutls_bye(args),
        "gnutls-ciphers" => builtin_gnutls_ciphers(args),
        "gnutls-deinit" => builtin_gnutls_deinit(args),
        "gnutls-digests" => builtin_gnutls_digests(args),
        "gnutls-error-fatalp" => builtin_gnutls_error_fatalp(args),
        "gnutls-error-string" => builtin_gnutls_error_string(args),
        "gnutls-errorp" => builtin_gnutls_errorp(args),
        "gnutls-format-certificate" => builtin_gnutls_format_certificate(args),
        "gnutls-get-initstage" => builtin_gnutls_get_initstage(args),
        "gnutls-hash-digest" => builtin_gnutls_hash_digest(args),
        "gnutls-hash-mac" => builtin_gnutls_hash_mac(args),
        "gnutls-macs" => builtin_gnutls_macs(args),
        "gnutls-peer-status" => builtin_gnutls_peer_status(args),
        "gnutls-peer-status-warning-describe" => builtin_gnutls_peer_status_warning_describe(args),
        "gnutls-symmetric-decrypt" => builtin_gnutls_symmetric_decrypt(args),
        "gnutls-symmetric-encrypt" => builtin_gnutls_symmetric_encrypt(args),
        "gpm-mouse-start" => builtin_gpm_mouse_start(args),
        "gpm-mouse-stop" => builtin_gpm_mouse_stop(args),
        "handle-save-session" => builtin_handle_save_session(args),
        "handle-switch-frame" => builtin_handle_switch_frame(args),
        "help--describe-vector" => builtin_help_describe_vector(args),
        "init-image-library" => builtin_init_image_library(args),
        "internal--labeled-narrow-to-region" => builtin_internal_labeled_narrow_to_region(args),
        "internal--labeled-widen" => builtin_internal_labeled_widen(args),
        "internal--obarray-buckets" => builtin_internal_obarray_buckets(args),
        "internal--set-buffer-modified-tick" => builtin_internal_set_buffer_modified_tick(args),
        "internal--track-mouse" => builtin_internal_track_mouse(args),
        "internal-char-font" => builtin_internal_char_font(args),
        "internal-complete-buffer" => builtin_internal_complete_buffer(args),
        "internal-describe-syntax-value" => builtin_internal_describe_syntax_value(args),
        "internal-event-symbol-parse-modifiers" => {
            builtin_internal_event_symbol_parse_modifiers(args)
        }
        "internal-handle-focus-in" => builtin_internal_handle_focus_in(args),
        "internal-make-var-non-special" => return None,
        "internal-set-lisp-face-attribute-from-resource" => {
            builtin_internal_set_lisp_face_attribute_from_resource(args)
        }
        "internal-stack-stats" => builtin_internal_stack_stats(args),
        "internal-subr-documentation" => builtin_internal_subr_documentation(args),
        "byte-code" => builtin_byte_code(args),
        "decode-coding-region" => builtin_decode_coding_region(args),
        "defconst-1" => builtin_defconst_1_eval(eval, args),
        "defvar-1" => builtin_defvar_1_eval(eval, args),
        "dump-emacs-portable" => builtin_dump_emacs_portable(args),
        "dump-emacs-portable--sort-predicate" => builtin_dump_emacs_portable_sort_predicate(args),
        "dump-emacs-portable--sort-predicate-copied" => {
            builtin_dump_emacs_portable_sort_predicate_copied(args)
        }
        "emacs-repository-get-version" => builtin_emacs_repository_get_version(args),
        "emacs-repository-get-branch" => builtin_emacs_repository_get_branch(args),
        "emacs-repository-get-dirty" => builtin_emacs_repository_get_dirty(args),
        "encode-coding-region" => builtin_encode_coding_region(args),
        "find-operation-coding-system" => builtin_find_operation_coding_system(args),
        "handler-bind-1" => return None,
        "iso-charset" => builtin_iso_charset(args),
        "keymap--get-keyelt" => builtin_keymap_get_keyelt(args),
        "keymap-prompt" => builtin_keymap_prompt(args),
        "kill-emacs" => return None,
        "lower-frame" => builtin_lower_frame(args),
        "lread--substitute-object-in-subtree" => builtin_lread_substitute_object_in_subtree(args),
        "malloc-info" => builtin_malloc_info(args),
        "malloc-trim" => builtin_malloc_trim(args),
        "make-byte-code" => builtin_make_byte_code(args),
        "make-char" => builtin_make_char(args),
        "make-closure" => builtin_make_closure(args),
        "make-finalizer" => builtin_make_finalizer(args),
        "marker-last-position" => builtin_marker_last_position(args),
        "make-indirect-buffer" => return None,
        "make-interpreted-closure" => builtin_make_interpreted_closure(args),
        "make-record" => builtin_make_record(args),
        "make-temp-file-internal" => builtin_make_temp_file_internal(args),
        "map-charset-chars" => builtin_map_charset_chars(args),
        "map-keymap" | "map-keymap-internal" => return None, // eval-backed in keymaps.rs
        "mapbacktrace" => builtin_mapbacktrace(args),
        // match-data--translate dispatched in eval path (needs &mut eval)
        "memory-info" => builtin_memory_info(args),
        "make-frame-invisible" => builtin_make_frame_invisible(args),
        "make-terminal-frame" => super::terminal::pure::builtin_make_terminal_frame(args),
        "menu-bar-menu-at-x-y" => builtin_menu_bar_menu_at_x_y(args),
        "menu-or-popup-active-p" => builtin_menu_or_popup_active_p(args),
        "module-load" => builtin_module_load(args),
        "mouse-pixel-position" => builtin_mouse_pixel_position(args),
        "mouse-position" => builtin_mouse_position(args),
        "newline-cache-check" => builtin_newline_cache_check(args),
        "native-comp-available-p" => builtin_native_comp_available_p(args),
        "native-comp-unit-file" => builtin_native_comp_unit_file(args),
        "native-comp-unit-set-file" => builtin_native_comp_unit_set_file(args),
        "native-elisp-load" => builtin_native_elisp_load(args),
        "new-fontset" => return None,
        "next-frame" => builtin_next_frame(args),
        "obarray-clear" => builtin_obarray_clear(args),
        "obarray-make" => builtin_obarray_make(args),
        "object-intervals" => builtin_object_intervals(args),
        "old-selected-frame" => builtin_old_selected_frame(args),
        "open-dribble-file" => builtin_open_dribble_file(args),
        "open-font" => builtin_open_font(args),
        "optimize-char-table" => builtin_optimize_char_table(args),
        "overlay-lists" => builtin_overlay_lists(args),
        "overlay-recenter" => builtin_overlay_recenter(args),
        "pdumper-stats" => builtin_pdumper_stats(args),
        "play-sound-internal" => builtin_play_sound_internal(args),
        "position-symbol" => builtin_position_symbol(args),
        "posn-at-point" => builtin_posn_at_point(args),
        "posn-at-x-y" => builtin_posn_at_x_y(args),
        "previous-frame" => builtin_previous_frame(args),
        "profiler-cpu-log" => builtin_profiler_cpu_log(args),
        "profiler-cpu-running-p" => builtin_profiler_cpu_running_p(args),
        "profiler-cpu-start" => builtin_profiler_cpu_start(args),
        "profiler-cpu-stop" => builtin_profiler_cpu_stop(args),
        "profiler-memory-log" => builtin_profiler_memory_log(args),
        "profiler-memory-running-p" => builtin_profiler_memory_running_p(args),
        "profiler-memory-start" => builtin_profiler_memory_start(args),
        "profiler-memory-stop" => builtin_profiler_memory_stop(args),
        "put-unicode-property-internal" => builtin_put_unicode_property_internal(args),
        "query-font" => builtin_query_font(args),
        "query-fontset" => builtin_query_fontset(args),
        "raise-frame" => builtin_raise_frame(args),
        "read-positioning-symbols" => builtin_read_positioning_symbols(args),
        "re--describe-compiled" => builtin_re_describe_compiled(args),
        "recent-auto-save-p" => builtin_recent_auto_save_p(args),
        "redisplay" => builtin_redisplay_eval(eval, args),
        "record" => builtin_record(args),
        "recordp" => builtin_recordp(args),
        "reconsider-frame-fonts" => builtin_reconsider_frame_fonts(args),
        "redirect-debugging-output" => builtin_redirect_debugging_output(args),
        "redirect-frame-focus" => builtin_redirect_frame_focus(args),
        "remove-pos-from-symbol" => builtin_remove_pos_from_symbol(args),
        "resize-mini-window-internal" => builtin_resize_mini_window_internal(args),
        "restore-buffer-modified-p" => builtin_restore_buffer_modified_p(args),
        "set--this-command-keys" => builtin_set_this_command_keys(args),
        "set-buffer-auto-saved" => builtin_set_buffer_auto_saved(args),
        "set-buffer-major-mode" => builtin_set_buffer_major_mode(args),
        "set-buffer-redisplay" => builtin_set_buffer_redisplay(args),
        "set-charset-plist" => builtin_set_charset_plist(args),
        "set-fontset-font" => return None,
        "set-frame-window-state-change" => builtin_set_frame_window_state_change(args),
        "set-fringe-bitmap-face" => builtin_set_fringe_bitmap_face(args),
        "set-minibuffer-window" => builtin_set_minibuffer_window(args),
        "set-mouse-pixel-position" => builtin_set_mouse_pixel_position(args),
        "set-mouse-position" => builtin_set_mouse_position(args),
        "set-window-new-normal" => builtin_set_window_new_normal(args),
        "set-window-new-pixel" => builtin_set_window_new_pixel(args),
        "set-window-new-total" => builtin_set_window_new_total(args),
        "sort-charsets" => builtin_sort_charsets(args),
        "split-char" => builtin_split_char(args),
        "string-distance" => builtin_string_distance(args),
        "subr-native-comp-unit" => builtin_subr_native_comp_unit(args),
        "subr-native-lambda-list" => builtin_subr_native_lambda_list(args),
        "subr-type" => builtin_subr_type(args),
        "suspend-emacs" => builtin_suspend_emacs(args),
        "thread--blocker" => builtin_thread_blocker(args),
        "tool-bar-get-system-style" => builtin_tool_bar_get_system_style(args),
        "tool-bar-pixel-width" => builtin_tool_bar_pixel_width(args),
        "translate-region-internal" => builtin_translate_region_internal(args),
        "transpose-regions" => builtin_transpose_regions(args),
        "tty--output-buffer-size" => builtin_tty_output_buffer_size(args),
        "tty--set-output-buffer-size" => builtin_tty_set_output_buffer_size(args),
        "tty-display-pixel-height" => builtin_tty_display_pixel_height(args),
        "tty-display-pixel-width" => builtin_tty_display_pixel_width(args),
        "tty-frame-at" => builtin_tty_frame_at(args),
        "tty-frame-edges" => builtin_tty_frame_edges(args),
        "tty-frame-geometry" => builtin_tty_frame_geometry(args),
        "tty-frame-list-z-order" => builtin_tty_frame_list_z_order(args),
        "tty-frame-restack" => builtin_tty_frame_restack(args),
        "tty-suppress-bold-inverse-default-colors" => {
            builtin_tty_suppress_bold_inverse_default_colors(args)
        }
        "unencodable-char-position" => builtin_unencodable_char_position(args),
        "unicode-property-table-internal" => builtin_unicode_property_table_internal(args),
        "unify-charset" => builtin_unify_charset(args),
        "unix-sync" => builtin_unix_sync(args),
        "value<" => builtin_value_lt(args),
        "variable-binding-locus" => builtin_variable_binding_locus(args),
        "x-begin-drag" => builtin_x_begin_drag(args),
        "x-double-buffered-p" => builtin_x_double_buffered_p(args),
        "x-menu-bar-open-internal" => builtin_x_menu_bar_open_internal(args),
        "xw-color-defined-p" => builtin_xw_color_defined_p(args),
        "xw-color-values" => builtin_xw_color_values(args),
        "interactive-form" => builtin_interactive_form(args),
        "inotify-add-watch" => builtin_inotify_add_watch(args),
        "inotify-allocated-p" => builtin_inotify_allocated_p(args),
        "inotify-rm-watch" => builtin_inotify_rm_watch(args),
        "inotify-valid-p" => builtin_inotify_valid_p(args),
        "inotify-watch-list" => builtin_inotify_watch_list(args),
        "local-variable-if-set-p" => builtin_local_variable_if_set_p(args),
        "lock-buffer" => builtin_lock_buffer(args),
        "lock-file" => builtin_lock_file(args),
        "lossage-size" => builtin_lossage_size(args),
        "unlock-buffer" => builtin_unlock_buffer(args),
        "unlock-file" => builtin_unlock_file(args),
        "window-bottom-divider-width" => builtin_window_bottom_divider_width(args),
        "window-left-child" => builtin_window_left_child(args),
        "window-line-height" => builtin_window_line_height(args),
        "window-lines-pixel-dimensions" => builtin_window_lines_pixel_dimensions(args),
        "window-new-normal" => builtin_window_new_normal(args),
        "window-new-pixel" => builtin_window_new_pixel(args),
        "window-new-total" => builtin_window_new_total(args),
        "window-next-sibling" => builtin_window_next_sibling(args),
        "window-normal-size" => builtin_window_normal_size(args),
        "window-old-body-pixel-height" => builtin_window_old_body_pixel_height(args),
        "window-old-body-pixel-width" => builtin_window_old_body_pixel_width(args),
        "window-old-pixel-height" => builtin_window_old_pixel_height(args),
        "window-old-pixel-width" => builtin_window_old_pixel_width(args),
        "window-parent" => builtin_window_parent(args),
        "window-pixel-left" => builtin_window_pixel_left(args),
        "window-pixel-top" => builtin_window_pixel_top(args),
        "window-prev-sibling" => builtin_window_prev_sibling(args),
        "window-resize-apply" => builtin_window_resize_apply(args),
        "window-right-divider-width" => builtin_window_right_divider_width(args),
        "window-scroll-bar-height" => builtin_window_scroll_bar_height(args),
        "window-scroll-bar-width" => builtin_window_scroll_bar_width(args),
        "window-top-child" => builtin_window_top_child(args),
        "treesit-available-p" => builtin_treesit_available_p(args),
        "treesit-compiled-query-p" => builtin_treesit_compiled_query_p(args),
        "treesit-induce-sparse-tree" => builtin_treesit_induce_sparse_tree(args),
        "treesit-language-abi-version" => builtin_treesit_language_abi_version(args),
        "treesit-language-available-p" => builtin_treesit_language_available_p(args),
        "treesit-library-abi-version" => builtin_treesit_library_abi_version(args),
        "treesit-node-check" => builtin_treesit_node_check(args),
        "treesit-node-child" => builtin_treesit_node_child(args),
        "treesit-node-child-by-field-name" => builtin_treesit_node_child_by_field_name(args),
        "treesit-node-child-count" => builtin_treesit_node_child_count(args),
        "treesit-node-descendant-for-range" => builtin_treesit_node_descendant_for_range(args),
        "treesit-node-end" => builtin_treesit_node_end(args),
        "treesit-node-eq" => builtin_treesit_node_eq(args),
        "treesit-node-field-name-for-child" => builtin_treesit_node_field_name_for_child(args),
        "treesit-node-first-child-for-pos" => builtin_treesit_node_first_child_for_pos(args),
        "treesit-node-match-p" => builtin_treesit_node_match_p(args),
        "treesit-node-next-sibling" => builtin_treesit_node_next_sibling(args),
        "treesit-node-p" => builtin_treesit_node_p(args),
        "treesit-node-parent" => builtin_treesit_node_parent(args),
        "treesit-node-parser" => builtin_treesit_node_parser(args),
        "treesit-node-prev-sibling" => builtin_treesit_node_prev_sibling(args),
        "treesit-node-start" => builtin_treesit_node_start(args),
        "treesit-node-string" => builtin_treesit_node_string(args),
        "treesit-node-type" => builtin_treesit_node_type(args),
        "treesit-parser-add-notifier" => builtin_treesit_parser_add_notifier(args),
        "treesit-parser-buffer" => builtin_treesit_parser_buffer(args),
        "treesit-parser-create" => builtin_treesit_parser_create(args),
        "treesit-parser-delete" => builtin_treesit_parser_delete(args),
        "treesit-parser-included-ranges" => builtin_treesit_parser_included_ranges(args),
        "treesit-parser-language" => builtin_treesit_parser_language(args),
        "treesit-parser-list" => builtin_treesit_parser_list(args),
        "treesit-parser-notifiers" => builtin_treesit_parser_notifiers(args),
        "treesit-parser-p" => builtin_treesit_parser_p(args),
        "treesit-parser-remove-notifier" => builtin_treesit_parser_remove_notifier(args),
        "treesit-parser-root-node" => builtin_treesit_parser_root_node(args),
        "treesit-parser-set-included-ranges" => builtin_treesit_parser_set_included_ranges(args),
        "treesit-parser-tag" => builtin_treesit_parser_tag(args),
        "treesit-pattern-expand" => builtin_treesit_pattern_expand(args),
        "treesit-query-capture" => builtin_treesit_query_capture(args),
        "treesit-query-compile" => builtin_treesit_query_compile(args),
        "treesit-query-expand" => builtin_treesit_query_expand(args),
        "treesit-query-language" => builtin_treesit_query_language(args),
        "treesit-query-p" => builtin_treesit_query_p(args),
        "treesit-search-forward" => builtin_treesit_search_forward(args),
        "treesit-search-subtree" => builtin_treesit_search_subtree(args),
        "treesit-subtree-stat" => builtin_treesit_subtree_stat(args),
        "treesit-grammar-location" => builtin_treesit_grammar_location(args),
        "treesit-tracking-line-column-p" => builtin_treesit_tracking_line_column_p(args),
        "treesit-parser-tracking-line-column-p" => {
            builtin_treesit_parser_tracking_line_column_p(args)
        }
        "treesit-query-eagerly-compiled-p" => builtin_treesit_query_eagerly_compiled_p(args),
        "treesit-query-source" => builtin_treesit_query_source(args),
        "treesit-parser-embed-level" => builtin_treesit_parser_embed_level(args),
        "treesit-parser-set-embed-level" => builtin_treesit_parser_set_embed_level(args),
        "treesit-parse-string" => builtin_treesit_parse_string(args),
        "treesit-parser-changed-regions" => builtin_treesit_parser_changed_regions(args),
        "treesit--linecol-at" => builtin_treesit_linecol_at(args),
        "treesit--linecol-cache-set" => builtin_treesit_linecol_cache_set(args),
        "treesit--linecol-cache" => builtin_treesit_linecol_cache(args),
        "sqlite-available-p" => builtin_sqlite_available_p(args),
        "sqlite-close" => builtin_sqlite_close(args),
        "sqlite-columns" => builtin_sqlite_columns(args),
        "sqlite-commit" => builtin_sqlite_commit(args),
        "sqlite-execute" => builtin_sqlite_execute(args),
        "sqlite-execute-batch" => builtin_sqlite_execute_batch(args),
        "sqlite-finalize" => builtin_sqlite_finalize(args),
        "sqlite-load-extension" => builtin_sqlite_load_extension(args),
        "sqlite-more-p" => builtin_sqlite_more_p(args),
        "sqlite-next" => builtin_sqlite_next(args),
        "sqlite-open" => builtin_sqlite_open(args),
        "sqlite-pragma" => builtin_sqlite_pragma(args),
        "sqlite-rollback" => builtin_sqlite_rollback(args),
        "sqlite-select" => builtin_sqlite_select(args),
        "sqlite-transaction" => builtin_sqlite_transaction(args),
        "sqlite-version" => builtin_sqlite_version(args),
        "sqlitep" => builtin_sqlitep(args),
        "fillarray" => builtin_fillarray(args),
        "define-hash-table-test" => builtin_define_hash_table_test(args),
        // Native compilation compatibility (pure)
        "comp--compile-ctxt-to-file0" => super::comp::builtin_comp_compile_ctxt_to_file0(args),
        "comp--init-ctxt" => super::comp::builtin_comp_init_ctxt(args),
        "comp--install-trampoline" => super::comp::builtin_comp_install_trampoline(args),
        "comp--late-register-subr" => super::comp::builtin_comp_late_register_subr(args),
        "comp--register-lambda" => super::comp::builtin_comp_register_lambda(args),
        "comp--register-subr" => super::comp::builtin_comp_register_subr(args),
        "comp--release-ctxt" => super::comp::builtin_comp_release_ctxt(args),
        "comp--subr-signature" => super::comp::builtin_comp_subr_signature(args),
        "comp-el-to-eln-filename" => super::comp::builtin_comp_el_to_eln_filename(args),
        "comp-el-to-eln-rel-filename" => super::comp::builtin_comp_el_to_eln_rel_filename(args),
        "comp-libgccjit-version" => super::comp::builtin_comp_libgccjit_version(args),
        "comp-native-compiler-options-effective-p" => {
            super::comp::builtin_comp_native_compiler_options_effective_p(args)
        }
        "comp-native-driver-options-effective-p" => {
            super::comp::builtin_comp_native_driver_options_effective_p(args)
        }

        // DBus compatibility (pure)
        "dbus--init-bus" => super::dbus::builtin_dbus_init_bus(args),
        "dbus-close-inhibitor-lock" => builtin_dbus_close_inhibitor_lock(args),
        "dbus-get-unique-name" => super::dbus::builtin_dbus_get_unique_name(args),
        "dbus-make-inhibitor-lock" => builtin_dbus_make_inhibitor_lock(args),
        "dbus-message-internal" => super::dbus::builtin_dbus_message_internal(args),
        "dbus-registered-inhibitor-locks" => builtin_dbus_registered_inhibitor_locks(args),

        // Documentation/help (pure)
        "documentation-property" => super::doc::builtin_documentation_property(args),
        "Snarf-documentation" => super::doc::builtin_snarf_documentation(args),
        // JSON (pure)
        "json-serialize" => super::json::builtin_json_serialize(args),
        "json-parse-string" => super::json::builtin_json_parse_string(args),

        // Subr introspection (pure)
        "subr-name" => super::subr_info::builtin_subr_name(args),
        "subr-arity" => super::subr_info::builtin_subr_arity(args),
        "native-comp-function-p" => super::subr_info::builtin_native_comp_function_p(args),
        "interpreted-function-p" => super::subr_info::builtin_interpreted_function_p(args),
        "commandp" => super::subr_info::builtin_commandp(args),
        "command-modes" => super::interactive::builtin_command_modes(args),
        "func-arity" => builtin_func_arity_eval(eval, args),

        // Format/string utilities (pure)
        "format-time-string" => super::format::builtin_format_time_string(args),
        // Marker (pure)
        "markerp" => super::marker::builtin_markerp(args),
        "marker-buffer" => super::marker::builtin_marker_buffer(args),
        "marker-insertion-type" => super::marker::builtin_marker_insertion_type(args),
        "make-marker" => super::marker::builtin_make_marker(args),

        // Composite (pure)
        "compose-string-internal" => super::composite::builtin_compose_string_internal(args),
        "find-composition-internal" => super::composite::builtin_find_composition_internal(args),
        "composition-get-gstring" => super::composite::builtin_composition_get_gstring(args),
        "clear-composition-cache" => super::composite::builtin_clear_composition_cache(args),
        "composition-sort-rules" => super::composite::builtin_composition_sort_rules(args),
        // Error hierarchy (pure)
        "signal" => super::errors::builtin_signal(args),

        // Hash-table extended (pure)
        "hash-table-test" => super::hashtab::builtin_hash_table_test(args),
        "hash-table-size" => super::hashtab::builtin_hash_table_size(args),
        "hash-table-rehash-size" => super::hashtab::builtin_hash_table_rehash_size(args),
        "hash-table-rehash-threshold" => super::hashtab::builtin_hash_table_rehash_threshold(args),
        "hash-table-weakness" => super::hashtab::builtin_hash_table_weakness(args),
        "copy-hash-table" => super::hashtab::builtin_copy_hash_table(args),
        "sxhash-eq" => super::hashtab::builtin_sxhash_eq(args),
        "sxhash-eql" => super::hashtab::builtin_sxhash_eql(args),
        "sxhash-equal" => super::hashtab::builtin_sxhash_equal(args),
        "sxhash-equal-including-properties" => {
            super::hashtab::builtin_sxhash_equal_including_properties(args)
        }
        "internal--hash-table-buckets" => super::hashtab::builtin_internal_hash_table_buckets(args),
        "internal--hash-table-histogram" => {
            super::hashtab::builtin_internal_hash_table_histogram(args)
        }
        "internal--hash-table-index-size" => {
            super::hashtab::builtin_internal_hash_table_index_size(args)
        }

        // Threading (pure)
        // Misc (pure)
        "copy-alist" => super::misc::builtin_copy_alist(args),
        "rassoc" => super::misc::builtin_rassoc(args),
        "rassq" => super::misc::builtin_rassq(args),
        "make-list" => super::misc::builtin_make_list(args),
        "safe-length" => super::misc::builtin_safe_length(args),
        "string-to-multibyte" => super::misc::builtin_string_to_multibyte(args),
        "string-to-unibyte" => super::misc::builtin_string_to_unibyte(args),
        "string-as-unibyte" => super::misc::builtin_string_as_unibyte(args),
        "string-as-multibyte" => super::misc::builtin_string_as_multibyte(args),
        "unibyte-char-to-multibyte" => super::misc::builtin_unibyte_char_to_multibyte(args),
        "multibyte-char-to-unibyte" => super::misc::builtin_multibyte_char_to_unibyte(args),
        "define-coding-system-internal" => {
            super::coding::builtin_define_coding_system_internal(&mut eval.coding_systems, args)
        }
        "define-coding-system-alias" => {
            super::coding::builtin_define_coding_system_alias(&mut eval.coding_systems, args)
        }
        "coding-system-p" => super::coding::builtin_coding_system_p(&eval.coding_systems, args),
        "check-coding-system" => {
            super::coding::builtin_check_coding_system(&eval.coding_systems, args)
        }
        "check-coding-systems-region" => {
            super::coding::builtin_check_coding_systems_region(&eval.coding_systems, args)
        }
        "set-coding-system-priority" => {
            super::coding::builtin_set_coding_system_priority(&mut eval.coding_systems, args)
        }
        "set-keyboard-coding-system-internal" => {
            super::coding::builtin_set_keyboard_coding_system_internal(
                &mut eval.coding_systems,
                args,
            )
        }
        "set-safe-terminal-coding-system-internal" => {
            super::coding::builtin_set_safe_terminal_coding_system_internal(
                &mut eval.coding_systems,
                args,
            )
        }
        "set-terminal-coding-system-internal" => {
            super::coding::builtin_set_terminal_coding_system_internal(
                &mut eval.coding_systems,
                args,
            )
        }
        "set-text-conversion-style" => super::coding::builtin_set_text_conversion_style(args),
        "text-quoting-style" => super::coding::builtin_text_quoting_style(args),
        "locale-info" => super::misc::builtin_locale_info(args),
        // Reader/printer (pure)
        "yes-or-no-p" => super::reader::builtin_yes_or_no_p(eval, args),

        // Char-table / bool-vector (pure)
        "make-char-table" => super::chartable::builtin_make_char_table(eval, args),
        "char-table-p" => super::chartable::builtin_char_table_p(args),
        "set-char-table-range" => super::chartable::builtin_set_char_table_range(args),
        "char-table-range" => super::chartable::builtin_char_table_range(args),
        "char-table-parent" => super::chartable::builtin_char_table_parent(args),
        "set-char-table-parent" => super::chartable::builtin_set_char_table_parent(args),
        "char-table-extra-slot" => super::chartable::builtin_char_table_extra_slot(args),
        "set-char-table-extra-slot" => super::chartable::builtin_set_char_table_extra_slot(args),
        "char-table-subtype" => super::chartable::builtin_char_table_subtype(args),
        "bool-vector" => super::chartable::builtin_bool_vector(args),
        "make-bool-vector" => super::chartable::builtin_make_bool_vector(args),
        "bool-vector-p" => super::chartable::builtin_bool_vector_p(args),
        "bool-vector-count-population" => {
            super::chartable::builtin_bool_vector_count_population(args)
        }
        "bool-vector-count-consecutive" => {
            super::chartable::builtin_bool_vector_count_consecutive(args)
        }
        "bool-vector-intersection" => super::chartable::builtin_bool_vector_intersection(args),
        "bool-vector-not" => super::chartable::builtin_bool_vector_not(args),
        "bool-vector-set-difference" => super::chartable::builtin_bool_vector_set_difference(args),
        "bool-vector-union" => super::chartable::builtin_bool_vector_union(args),
        "bool-vector-exclusive-or" => super::chartable::builtin_bool_vector_exclusive_or(args),
        "bool-vector-subsetp" => super::chartable::builtin_bool_vector_subsetp(args),

        // Note: windowp and framep are in the eval-dependent section above
        // Search (pure)
        "string-match" => super::search::builtin_string_match(args),
        "regexp-quote" => super::search::builtin_regexp_quote(args),
        "match-beginning" => super::search::builtin_match_beginning(args),
        "match-end" => super::search::builtin_match_end(args),
        "match-data" => super::search::builtin_match_data(args),
        "set-match-data" => super::search::builtin_set_match_data(args),
        "looking-at" => super::search::builtin_looking_at(args),

        // Lread (pure)
        "get-load-suffixes" => super::lread::builtin_get_load_suffixes(args),
        "locate-file-internal" => super::lread::builtin_locate_file_internal(eval, args),
        "read-coding-system" => super::lread::builtin_read_coding_system(args),
        "read-non-nil-coding-system" => super::lread::builtin_read_non_nil_coding_system(args),

        // Editfns (pure)
        "user-uid" => super::editfns::builtin_user_uid(args),
        "user-real-uid" => super::editfns::builtin_user_real_uid(args),
        "group-name" => super::editfns::builtin_group_name(args),
        "group-gid" => super::editfns::builtin_group_gid(args),
        "group-real-gid" => super::editfns::builtin_group_real_gid(args),
        "load-average" => super::editfns::builtin_load_average(args),
        "logcount" => super::editfns::builtin_logcount(args),

        // Fns (pure)
        "base64-encode-string" => super::fns::builtin_base64_encode_string(args),
        "base64-decode-string" => super::fns::builtin_base64_decode_string(args),
        "base64url-encode-string" => super::fns::builtin_base64url_encode_string(args),
        "md5" => super::fns::builtin_md5(args),
        "secure-hash" => super::fns::builtin_secure_hash(args),
        "equal-including-properties" => super::fns::builtin_equal_including_properties(args),
        "string-make-multibyte" => super::fns::builtin_string_make_multibyte(args),
        "string-make-unibyte" => super::fns::builtin_string_make_unibyte(args),
        "compare-strings" => super::fns::builtin_compare_strings(args),
        "string-version-lessp" => super::fns::builtin_string_version_lessp(args),
        "string-collate-lessp" => super::fns::builtin_string_collate_lessp(args),
        "string-collate-equalp" => super::fns::builtin_string_collate_equalp(args),

        // atimer.c gap-fill
        "debug-timer-check" => builtin_debug_timer_check(args),

        // lcms.c stubs (no lcms in NeoVM)
        "lcms2-available-p" => builtin_lcms2_available_p(args),
        "lcms-cie-de2000" => builtin_lcms_cie_de2000(args),
        "lcms-xyz->jch" => builtin_lcms_xyz_to_jch(args),
        "lcms-jch->xyz" => builtin_lcms_jch_to_xyz(args),
        "lcms-jch->jab" => builtin_lcms_jch_to_jab(args),
        "lcms-jab->jch" => builtin_lcms_jab_to_jch(args),
        "lcms-cam02-ucs" => builtin_lcms_cam02_ucs(args),
        "lcms-temp->white-point" => builtin_lcms_temp_to_white_point(args),

        // neomacsfns.c gap-fill
        "neomacs-frame-geometry" => builtin_neomacs_frame_geometry(args),
        "neomacs-frame-edges" => builtin_neomacs_frame_edges(args),
        "neomacs-mouse-absolute-pixel-position" => {
            builtin_neomacs_mouse_absolute_pixel_position(args)
        }
        "neomacs-set-mouse-absolute-pixel-position" => {
            builtin_neomacs_set_mouse_absolute_pixel_position(args)
        }
        "neomacs-display-monitor-attributes-list" => {
            builtin_neomacs_display_monitor_attributes_list(args)
        }
        "x-scroll-bar-foreground" => builtin_x_scroll_bar_foreground(args),
        "x-scroll-bar-background" => builtin_x_scroll_bar_background(args),
        "neomacs-clipboard-set" => builtin_neomacs_clipboard_set(args),
        "neomacs-clipboard-get" => builtin_neomacs_clipboard_get(args),
        "neomacs-primary-selection-set" => builtin_neomacs_primary_selection_set(args),
        "neomacs-primary-selection-get" => builtin_neomacs_primary_selection_get(args),
        "neomacs-core-backend" => builtin_neomacs_core_backend(args),

        // eval.c gap-fill — eval-backed for buffer access
        "buffer-local-toplevel-value" => {
            // GNU eval.c:838 — return toplevel buffer-local value,
            // bypassing dynamic let bindings.
            if let Err(e) =
                super::builtins::expect_range_args("buffer-local-toplevel-value", &args, 1, 2)
            {
                return Some(Err(e));
            }
            let Some(sym_name) = args[0].as_symbol_name() else {
                return Some(Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), args[0]],
                )));
            };
            if let Some(buf) = eval.buffers.current_buffer() {
                if let Some(val) = buf.get_buffer_local(sym_name) {
                    return Some(Ok(*val));
                }
            }
            if let Some(val) = eval.obarray.symbol_value(sym_name) {
                return Some(Ok(*val));
            }
            return Some(Err(signal("void-variable", vec![args[0]])));
        }
        "set-buffer-local-toplevel-value" => {
            // GNU eval.c:857 — set toplevel buffer-local value.
            if let Err(e) =
                super::builtins::expect_range_args("set-buffer-local-toplevel-value", &args, 2, 3)
            {
                return Some(Err(e));
            }
            let Some(sym_name) = args[0].as_symbol_name() else {
                return Some(Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), args[0]],
                )));
            };
            if let Some(bid) = eval.buffers.current_buffer_id() {
                let _ = eval
                    .buffers
                    .set_buffer_local_property(bid, sym_name, args[1]);
            }
            return Some(Ok(Value::Nil));
        }
        "debugger-trap" => builtin_debugger_trap(args),
        "internal-delete-indirect-variable" => builtin_internal_delete_indirect_variable(args),

        // coding.c gap-fill
        "internal-decode-string-utf-8" => builtin_internal_decode_string_utf_8(args),
        "internal-encode-string-utf-8" => builtin_internal_encode_string_utf_8(args),

        // buffer.c gap-fill
        "overlay-tree" => builtin_overlay_tree(args),

        // process.c gap-fill
        // "process-connection" removed: not a GNU C builtin

        // thread.c gap-fill
        "thread-buffer-disposition" => builtin_thread_buffer_disposition(args),
        "thread-set-buffer-disposition" => builtin_thread_set_buffer_disposition(args),

        // window.c gap-fill
        "window-discard-buffer-from-window" => builtin_window_discard_buffer_from_window(args),
        "window-cursor-info" => builtin_window_cursor_info(args),
        "combine-windows" => builtin_combine_windows(args),
        "uncombine-window" => builtin_uncombine_window(args),

        // frame.c gap-fill
        "frame-windows-min-size" => builtin_frame_windows_min_size(args),

        // xdisp.c gap-fill
        "remember-mouse-glyph" => builtin_remember_mouse_glyph(args),

        // image.c gap-fill
        "lookup-image" => builtin_lookup_image(args),
        "imagemagick-types" => builtin_imagemagick_types(args),

        // font.c gap-fill
        "font-drive-otf" => builtin_font_drive_otf(args),
        "font-otf-alternates" => builtin_font_otf_alternates(args),

        _ => return None,
    })
}

#[cfg(test)]
mod tests;

// -----------------------------------------------------------------------
// Wrapper functions for builtins that need tracing or non-standard access
// -----------------------------------------------------------------------

fn defsubr_run_hooks(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let hook_names: Vec<String> = args
        .iter()
        .filter_map(|a| a.as_symbol_name().map(|s| s.to_string()))
        .collect();
    let dominated_by_noise = hook_names
        .iter()
        .all(|h| h == "custom-define-hook" || h == "change-major-mode-hook");
    if dominated_by_noise {
        tracing::debug!(hooks = ?hook_names, "run-hooks");
    } else {
        tracing::info!(hooks = ?hook_names, "run-hooks called");
    }
    let result = builtin_run_hooks(eval, args);
    if !dominated_by_noise {
        tracing::info!(hooks = ?hook_names, "run-hooks returned");
    }
    if hook_names.iter().any(|h| h == "window-setup-hook") {
        tracing::info!("Enabling post-startup builtin tracing");
        TRACE_ALL_BUILTINS.store(true, Ordering::Relaxed);
    }
    result
}

fn defsubr_load(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let file_name = args.first().map(|a| format!("{}", a)).unwrap_or_default();
    tracing::info!(file = %file_name, "load called");
    let result = builtin_load(eval, args);
    tracing::info!(file = %file_name, ok = result.is_ok(), "load returned");
    result
}

fn defsubr_message(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let msg_preview: String = args
        .first()
        .map(|a| {
            let s = format!("{}", a);
            if s.len() > 120 {
                format!("{}...", &s[..120])
            } else {
                s
            }
        })
        .unwrap_or_default();
    tracing::info!(msg = %msg_preview, "message");
    builtin_message_eval(eval, args)
}

fn defsubr_coding_system_aliases(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_aliases(&eval.coding_systems, args)
}
fn defsubr_coding_system_plist(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_plist(&eval.coding_systems, args)
}
fn defsubr_coding_system_put(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_put(&mut eval.coding_systems, args)
}
fn defsubr_coding_system_base(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_base(&eval.coding_systems, args)
}
fn defsubr_coding_system_eol_type(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_coding_system_eol_type(&eval.coding_systems, args)
}
fn defsubr_detect_coding_string(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_detect_coding_string(&eval.coding_systems, args)
}
fn defsubr_detect_coding_region(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_detect_coding_region(&eval.coding_systems, args)
}
fn defsubr_keyboard_coding_system(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_keyboard_coding_system(&eval.coding_systems, args)
}
fn defsubr_terminal_coding_system(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    super::coding::builtin_terminal_coding_system(&eval.coding_systems, args)
}
fn defsubr_coding_system_priority_list(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    super::coding::builtin_coding_system_priority_list(&eval.coding_systems, args)
}

/// Register all builtins via defsubr — function pointer dispatch.
///
/// This replaces the giant match-by-name block in dispatch_builtin.
/// Each registered builtin is called via a direct function pointer,
/// matching GNU Emacs's defsubr/funcall_subr architecture.
pub(crate) fn init_builtins(ctx: &mut super::eval::Context) {
    use super::error::*;
    use super::eval::Context;
    use super::value::*;
    ctx.defsubr("apply", builtin_apply, 0, None);
    ctx.defsubr("funcall", builtin_funcall, 0, None);
    ctx.defsubr(
        "funcall-interactively",
        builtin_funcall_interactively,
        0,
        None,
    );
    ctx.defsubr(
        "funcall-with-delayed-message",
        builtin_funcall_with_delayed_message,
        0,
        None,
    );
    ctx.defsubr("defalias", builtin_defalias, 0, None);
    ctx.defsubr("provide", builtin_provide, 0, None);
    ctx.defsubr("require", builtin_require, 0, None);
    ctx.defsubr("mapcan", builtin_mapcan, 0, None);
    ctx.defsubr("mapcar", builtin_mapcar, 0, None);
    ctx.defsubr("mapc", builtin_mapc, 0, None);
    ctx.defsubr("mapconcat", builtin_mapconcat, 0, None);
    ctx.defsubr("sort", builtin_sort, 0, None);
    ctx.defsubr("functionp", builtin_functionp_eval, 0, None);
    ctx.defsubr("defvaralias", builtin_defvaralias_eval, 0, None);
    ctx.defsubr("boundp", builtin_boundp, 0, None);
    ctx.defsubr("default-boundp", builtin_default_boundp, 0, None);
    ctx.defsubr(
        "default-toplevel-value",
        builtin_default_toplevel_value,
        0,
        None,
    );
    ctx.defsubr("fboundp", builtin_fboundp, 0, None);
    ctx.defsubr(
        "internal-make-var-non-special",
        builtin_internal_make_var_non_special_eval,
        0,
        None,
    );
    ctx.defsubr("indirect-variable", builtin_indirect_variable_eval, 0, None);
    ctx.defsubr("handler-bind-1", builtin_handler_bind_1_eval, 0, None);
    ctx.defsubr("symbol-value", builtin_symbol_value, 0, None);
    ctx.defsubr("symbol-function", builtin_symbol_function, 0, None);
    ctx.defsubr("function-get", builtin_function_get, 0, None);
    ctx.defsubr("set", builtin_set, 0, None);
    ctx.defsubr("fset", builtin_fset, 0, None);
    ctx.defsubr("makunbound", builtin_makunbound, 0, None);
    ctx.defsubr("fmakunbound", builtin_fmakunbound, 0, None);
    ctx.defsubr("macroexpand", builtin_macroexpand_eval, 0, None);
    ctx.defsubr("get", builtin_get, 0, None);
    ctx.defsubr("put", builtin_put, 0, None);
    ctx.defsubr("setplist", builtin_setplist_eval, 0, None);
    ctx.defsubr("symbol-plist", builtin_symbol_plist_fn, 0, None);
    ctx.defsubr("indirect-function", builtin_indirect_function, 0, None);
    ctx.defsubr("signal", super::errors::builtin_signal_eval, 0, None);
    ctx.defsubr(
        "getenv-internal",
        super::process::builtin_getenv_internal_eval,
        0,
        None,
    );
    ctx.defsubr("special-variable-p", builtin_special_variable_p, 0, None);
    ctx.defsubr("intern", builtin_intern_fn, 0, None);
    ctx.defsubr("intern-soft", builtin_intern_soft, 0, None);
    ctx.defsubr("run-hook-with-args", builtin_run_hook_with_args, 0, None);
    ctx.defsubr(
        "run-hook-with-args-until-success",
        builtin_run_hook_with_args_until_success,
        0,
        None,
    );
    ctx.defsubr(
        "run-hook-with-args-until-failure",
        builtin_run_hook_with_args_until_failure,
        0,
        None,
    );
    ctx.defsubr("run-hook-wrapped", builtin_run_hook_wrapped, 0, None);
    ctx.defsubr(
        "run-window-configuration-change-hook",
        builtin_run_window_configuration_change_hook,
        0,
        None,
    );
    ctx.defsubr(
        "run-window-scroll-functions",
        builtin_run_window_scroll_functions,
        0,
        None,
    );
    ctx.defsubr("featurep", builtin_featurep, 0, None);
    ctx.defsubr("garbage-collect", builtin_garbage_collect_eval, 0, None);
    ctx.defsubr(
        "neovm-precompile-file",
        builtin_neovm_precompile_file,
        0,
        None,
    );
    ctx.defsubr("eval", builtin_eval, 0, None);
    ctx.defsubr("get-buffer-create", builtin_get_buffer_create, 0, None);
    ctx.defsubr("get-buffer", builtin_get_buffer, 0, None);
    ctx.defsubr(
        "make-indirect-buffer",
        builtin_make_indirect_buffer,
        0,
        None,
    );
    ctx.defsubr("find-buffer", builtin_find_buffer, 0, None);
    ctx.defsubr("buffer-live-p", builtin_buffer_live_p, 0, None);
    ctx.defsubr(
        "barf-if-buffer-read-only",
        builtin_barf_if_buffer_read_only,
        0,
        None,
    );
    ctx.defsubr(
        "bury-buffer-internal",
        builtin_bury_buffer_internal,
        0,
        None,
    );
    ctx.defsubr("get-file-buffer", builtin_get_file_buffer, 0, None);
    ctx.defsubr("kill-buffer", builtin_kill_buffer, 0, None);
    ctx.defsubr("set-buffer", builtin_set_buffer, 0, None);
    ctx.defsubr("current-buffer", builtin_current_buffer, 0, None);
    ctx.defsubr("buffer-name", builtin_buffer_name, 0, None);
    ctx.defsubr("buffer-file-name", builtin_buffer_file_name, 0, None);
    ctx.defsubr("buffer-base-buffer", builtin_buffer_base_buffer, 0, None);
    ctx.defsubr("buffer-last-name", builtin_buffer_last_name, 0, None);
    ctx.defsubr("rename-buffer", builtin_rename_buffer, 0, None);
    ctx.defsubr("buffer-string", builtin_buffer_string, 0, None);
    ctx.defsubr(
        "buffer-line-statistics",
        builtin_buffer_line_statistics,
        0,
        None,
    );
    ctx.defsubr(
        "buffer-text-pixel-size",
        builtin_buffer_text_pixel_size,
        0,
        None,
    );
    ctx.defsubr(
        "base64-encode-region",
        super::fns::builtin_base64_encode_region_eval,
        0,
        None,
    );
    ctx.defsubr(
        "base64-decode-region",
        super::fns::builtin_base64_decode_region_eval,
        0,
        None,
    );
    ctx.defsubr(
        "base64url-encode-region",
        super::fns::builtin_base64url_encode_region_eval,
        0,
        None,
    );
    ctx.defsubr("md5", super::fns::builtin_md5_eval, 0, None);
    ctx.defsubr("secure-hash", super::fns::builtin_secure_hash_eval, 0, None);
    ctx.defsubr("buffer-hash", super::fns::builtin_buffer_hash_eval, 0, None);
    ctx.defsubr("buffer-substring", builtin_buffer_substring, 0, None);
    ctx.defsubr(
        "compare-buffer-substrings",
        builtin_compare_buffer_substrings,
        0,
        None,
    );
    ctx.defsubr("point", builtin_point, 0, None);
    ctx.defsubr("point-min", builtin_point_min, 0, None);
    ctx.defsubr("point-max", builtin_point_max, 0, None);
    ctx.defsubr("goto-char", builtin_goto_char, 0, None);
    ctx.defsubr("field-beginning", builtin_field_beginning, 0, None);
    ctx.defsubr("field-end", builtin_field_end, 0, None);
    ctx.defsubr("field-string", builtin_field_string, 0, None);
    ctx.defsubr(
        "field-string-no-properties",
        builtin_field_string_no_properties,
        0,
        None,
    );
    ctx.defsubr("constrain-to-field", builtin_constrain_to_field, 0, None);
    ctx.defsubr("insert", builtin_insert, 0, None);
    ctx.defsubr("insert-and-inherit", builtin_insert_and_inherit, 0, None);
    ctx.defsubr(
        "insert-before-markers-and-inherit",
        builtin_insert_before_markers_and_inherit,
        0,
        None,
    );
    ctx.defsubr(
        "insert-buffer-substring",
        builtin_insert_buffer_substring,
        0,
        None,
    );
    ctx.defsubr("insert-char", builtin_insert_char, 0, None);
    ctx.defsubr("insert-byte", builtin_insert_byte, 0, None);
    ctx.defsubr(
        "replace-region-contents",
        builtin_replace_region_contents_eval,
        0,
        None,
    );
    ctx.defsubr(
        "set-buffer-multibyte",
        builtin_set_buffer_multibyte_eval,
        0,
        None,
    );
    ctx.defsubr(
        "kill-all-local-variables",
        builtin_kill_all_local_variables,
        0,
        None,
    );
    ctx.defsubr("buffer-swap-text", builtin_buffer_swap_text, 0, None);
    ctx.defsubr("delete-region", builtin_delete_region, 0, None);
    ctx.defsubr(
        "delete-and-extract-region",
        builtin_delete_and_extract_region,
        0,
        None,
    );
    ctx.defsubr(
        "subst-char-in-region",
        builtin_subst_char_in_region,
        0,
        None,
    );
    ctx.defsubr("delete-field", builtin_delete_field, 0, None);
    ctx.defsubr("delete-all-overlays", builtin_delete_all_overlays, 0, None);
    ctx.defsubr("erase-buffer", builtin_erase_buffer, 0, None);
    ctx.defsubr("buffer-enable-undo", builtin_buffer_enable_undo, 0, None);
    ctx.defsubr("buffer-size", builtin_buffer_size, 0, None);
    ctx.defsubr("narrow-to-region", builtin_narrow_to_region, 0, None);
    ctx.defsubr("widen", builtin_widen, 0, None);
    ctx.defsubr(
        "internal--labeled-narrow-to-region",
        builtin_internal_labeled_narrow_to_region_eval,
        0,
        None,
    );
    ctx.defsubr(
        "internal--labeled-widen",
        builtin_internal_labeled_widen_eval,
        0,
        None,
    );
    ctx.defsubr("buffer-modified-p", builtin_buffer_modified_p, 0, None);
    ctx.defsubr(
        "set-buffer-modified-p",
        builtin_set_buffer_modified_p,
        0,
        None,
    );
    ctx.defsubr(
        "buffer-modified-tick",
        builtin_buffer_modified_tick,
        0,
        None,
    );
    ctx.defsubr(
        "buffer-chars-modified-tick",
        builtin_buffer_chars_modified_tick,
        0,
        None,
    );
    ctx.defsubr("buffer-list", builtin_buffer_list, 0, None);
    ctx.defsubr("other-buffer", builtin_other_buffer, 0, None);
    ctx.defsubr(
        "generate-new-buffer-name",
        builtin_generate_new_buffer_name,
        0,
        None,
    );
    ctx.defsubr("char-after", builtin_char_after, 0, None);
    ctx.defsubr("char-before", builtin_char_before, 0, None);
    ctx.defsubr("byte-to-position", builtin_byte_to_position, 0, None);
    ctx.defsubr("position-bytes", builtin_position_bytes, 0, None);
    ctx.defsubr("get-byte", builtin_get_byte, 0, None);
    ctx.defsubr("buffer-local-value", builtin_buffer_local_value, 0, None);
    ctx.defsubr(
        "local-variable-if-set-p",
        builtin_local_variable_if_set_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "variable-binding-locus",
        builtin_variable_binding_locus_eval,
        0,
        None,
    );
    ctx.defsubr("interactive-form", builtin_interactive_form_eval, 0, None);
    ctx.defsubr(
        "command-modes",
        super::interactive::builtin_command_modes_eval,
        0,
        None,
    );
    ctx.defsubr("search-forward", builtin_search_forward, 0, None);
    ctx.defsubr("search-backward", builtin_search_backward, 0, None);
    ctx.defsubr("re-search-forward", builtin_re_search_forward, 0, None);
    ctx.defsubr("re-search-backward", builtin_re_search_backward, 0, None);
    ctx.defsubr("looking-at", builtin_looking_at, 0, None);
    ctx.defsubr("posix-looking-at", builtin_posix_looking_at, 0, None);
    ctx.defsubr("string-match", builtin_string_match_eval, 0, None);
    ctx.defsubr("posix-string-match", builtin_posix_string_match, 0, None);
    ctx.defsubr("match-beginning", builtin_match_beginning, 0, None);
    ctx.defsubr("match-end", builtin_match_end, 0, None);
    ctx.defsubr("match-data", builtin_match_data_eval, 0, None);
    ctx.defsubr(
        "match-data--translate",
        builtin_match_data_translate_eval,
        0,
        None,
    );
    ctx.defsubr("set-match-data", builtin_set_match_data_eval, 0, None);
    ctx.defsubr("replace-match", builtin_replace_match, 0, None);
    ctx.defsubr(
        "find-charset-region",
        super::charset::builtin_find_charset_region_eval,
        0,
        None,
    );
    ctx.defsubr(
        "charset-after",
        super::charset::builtin_charset_after_eval,
        0,
        None,
    );
    ctx.defsubr(
        "format-mode-line",
        super::xdisp::builtin_format_mode_line_eval,
        0,
        None,
    );
    ctx.defsubr(
        "window-line-height",
        super::xdisp::builtin_window_line_height_eval,
        0,
        None,
    );
    ctx.defsubr(
        "posn-at-point",
        super::xdisp::builtin_posn_at_point_eval,
        0,
        None,
    );
    ctx.defsubr(
        "posn-at-x-y",
        super::xdisp::builtin_posn_at_x_y_eval,
        0,
        None,
    );
    ctx.defsubr(
        "coordinates-in-window-p",
        builtin_coordinates_in_window_p,
        0,
        None,
    );
    ctx.defsubr(
        "tool-bar-height",
        super::xdisp::builtin_tool_bar_height_eval,
        0,
        None,
    );
    ctx.defsubr(
        "tab-bar-height",
        super::xdisp::builtin_tab_bar_height_eval,
        0,
        None,
    );
    ctx.defsubr("list-fonts", super::font::builtin_list_fonts_eval, 0, None);
    ctx.defsubr("find-font", super::font::builtin_find_font_eval, 0, None);
    ctx.defsubr(
        "font-family-list",
        super::font::builtin_font_family_list_eval,
        0,
        None,
    );
    ctx.defsubr("font-info", super::font::builtin_font_info_eval, 0, None);
    ctx.defsubr("new-fontset", builtin_new_fontset_eval, 0, None);
    ctx.defsubr("set-fontset-font", builtin_set_fontset_font_eval, 0, None);
    ctx.defsubr(
        "insert-file-contents",
        super::fileio::builtin_insert_file_contents,
        0,
        None,
    );
    ctx.defsubr("write-region", super::fileio::builtin_write_region, 0, None);
    ctx.defsubr(
        "file-name-completion",
        super::dired::builtin_file_name_completion_eval,
        0,
        None,
    );
    ctx.defsubr(
        "set-visited-file-modtime",
        super::fileio::builtin_set_visited_file_modtime,
        0,
        None,
    );
    ctx.defsubr("make-keymap", builtin_make_keymap, 0, None);
    ctx.defsubr("make-sparse-keymap", builtin_make_sparse_keymap, 0, None);
    ctx.defsubr("copy-keymap", builtin_copy_keymap, 0, None);
    ctx.defsubr("define-key", builtin_define_key, 0, None);
    ctx.defsubr("lookup-key", builtin_lookup_key, 0, None);
    ctx.defsubr("use-local-map", builtin_use_local_map, 0, None);
    ctx.defsubr("use-global-map", builtin_use_global_map, 0, None);
    ctx.defsubr("current-local-map", builtin_current_local_map, 0, None);
    ctx.defsubr("current-global-map", builtin_current_global_map, 0, None);
    ctx.defsubr("current-active-maps", builtin_current_active_maps, 0, None);
    ctx.defsubr(
        "current-minor-mode-maps",
        builtin_current_minor_mode_maps,
        0,
        None,
    );
    ctx.defsubr("keymap-parent", builtin_keymap_parent, 0, None);
    ctx.defsubr("set-keymap-parent", builtin_set_keymap_parent, 0, None);
    ctx.defsubr("keymapp", builtin_keymapp, 0, None);
    ctx.defsubr("accessible-keymaps", builtin_accessible_keymaps, 0, None);
    ctx.defsubr("map-keymap", builtin_map_keymap, 0, None);
    ctx.defsubr("map-keymap-internal", builtin_map_keymap_internal, 0, None);
    ctx.defsubr(
        "print--preprocess",
        super::process::builtin_print_preprocess,
        0,
        None,
    );
    ctx.defsubr(
        "format-network-address",
        super::process::builtin_format_network_address,
        0,
        None,
    );
    ctx.defsubr(
        "network-interface-list",
        super::process::builtin_network_interface_list,
        0,
        None,
    );
    ctx.defsubr(
        "network-interface-info",
        super::process::builtin_network_interface_info,
        0,
        None,
    );
    ctx.defsubr(
        "signal-names",
        super::process::builtin_signal_names,
        0,
        None,
    );
    ctx.defsubr(
        "accept-process-output",
        super::process::builtin_accept_process_output,
        0,
        None,
    );
    ctx.defsubr(
        "list-system-processes",
        super::process::builtin_list_system_processes,
        0,
        None,
    );
    ctx.defsubr(
        "num-processors",
        super::process::builtin_num_processors,
        0,
        None,
    );
    ctx.defsubr(
        "make-process",
        super::process::builtin_make_process,
        0,
        None,
    );
    ctx.defsubr(
        "make-network-process",
        super::process::builtin_make_network_process,
        0,
        None,
    );
    ctx.defsubr(
        "make-pipe-process",
        super::process::builtin_make_pipe_process,
        0,
        None,
    );
    ctx.defsubr("gnutls-boot", super::process::builtin_gnutls_boot, 0, None);
    ctx.defsubr(
        "make-serial-process",
        super::process::builtin_make_serial_process,
        0,
        None,
    );
    ctx.defsubr(
        "serial-process-configure",
        super::process::builtin_serial_process_configure,
        0,
        None,
    );
    ctx.defsubr(
        "call-process",
        super::process::builtin_call_process,
        0,
        None,
    );
    ctx.defsubr(
        "call-process-region",
        super::process::builtin_call_process_region,
        0,
        None,
    );
    ctx.defsubr(
        "continue-process",
        super::process::builtin_continue_process,
        0,
        None,
    );
    ctx.defsubr(
        "delete-process",
        super::process::builtin_delete_process,
        0,
        None,
    );
    ctx.defsubr(
        "interrupt-process",
        super::process::builtin_interrupt_process,
        0,
        None,
    );
    ctx.defsubr(
        "kill-process",
        super::process::builtin_kill_process,
        0,
        None,
    );
    ctx.defsubr(
        "quit-process",
        super::process::builtin_quit_process,
        0,
        None,
    );
    ctx.defsubr(
        "signal-process",
        super::process::builtin_signal_process,
        0,
        None,
    );
    ctx.defsubr(
        "stop-process",
        super::process::builtin_stop_process,
        0,
        None,
    );
    ctx.defsubr("get-process", super::process::builtin_get_process, 0, None);
    ctx.defsubr(
        "get-buffer-process",
        super::process::builtin_get_buffer_process,
        0,
        None,
    );
    ctx.defsubr(
        "process-attributes",
        super::process::builtin_process_attributes,
        0,
        None,
    );
    ctx.defsubr("processp", super::process::builtin_processp, 0, None);
    ctx.defsubr("process-id", super::process::builtin_process_id, 0, None);
    ctx.defsubr(
        "process-command",
        super::process::builtin_process_command,
        0,
        None,
    );
    ctx.defsubr(
        "process-contact",
        super::process::builtin_process_contact,
        0,
        None,
    );
    ctx.defsubr(
        "process-filter",
        super::process::builtin_process_filter,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-filter",
        super::process::builtin_set_process_filter,
        0,
        None,
    );
    ctx.defsubr(
        "process-sentinel",
        super::process::builtin_process_sentinel,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-sentinel",
        super::process::builtin_set_process_sentinel,
        0,
        None,
    );
    ctx.defsubr(
        "process-coding-system",
        super::process::builtin_process_coding_system,
        0,
        None,
    );
    ctx.defsubr(
        "process-datagram-address",
        super::process::builtin_process_datagram_address,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-buffer",
        super::process::builtin_set_process_buffer,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-thread",
        super::process::builtin_set_process_thread,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-window-size",
        super::process::builtin_set_process_window_size,
        0,
        None,
    );
    ctx.defsubr(
        "process-tty-name",
        super::process::builtin_process_tty_name,
        0,
        None,
    );
    ctx.defsubr(
        "process-plist",
        super::process::builtin_process_plist,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-plist",
        super::process::builtin_set_process_plist,
        0,
        None,
    );
    ctx.defsubr(
        "process-mark",
        super::process::builtin_process_mark,
        0,
        None,
    );
    ctx.defsubr(
        "process-type",
        super::process::builtin_process_type,
        0,
        None,
    );
    ctx.defsubr(
        "process-thread",
        super::process::builtin_process_thread,
        0,
        None,
    );
    ctx.defsubr(
        "process-running-child-p",
        super::process::builtin_process_running_child_p,
        0,
        None,
    );
    ctx.defsubr(
        "process-send-region",
        super::process::builtin_process_send_region,
        0,
        None,
    );
    ctx.defsubr(
        "process-send-eof",
        super::process::builtin_process_send_eof,
        0,
        None,
    );
    ctx.defsubr(
        "process-send-string",
        super::process::builtin_process_send_string,
        0,
        None,
    );
    ctx.defsubr(
        "process-status",
        super::process::builtin_process_status,
        0,
        None,
    );
    ctx.defsubr(
        "process-exit-status",
        super::process::builtin_process_exit_status,
        0,
        None,
    );
    ctx.defsubr(
        "process-list",
        super::process::builtin_process_list,
        0,
        None,
    );
    ctx.defsubr(
        "process-name",
        super::process::builtin_process_name,
        0,
        None,
    );
    ctx.defsubr(
        "process-buffer",
        super::process::builtin_process_buffer,
        0,
        None,
    );
    ctx.defsubr("sleep-for", super::timer::builtin_sleep_for, 0, None);
    ctx.defsubr(
        "add-variable-watcher",
        super::advice::builtin_add_variable_watcher,
        0,
        None,
    );
    ctx.defsubr(
        "remove-variable-watcher",
        super::advice::builtin_remove_variable_watcher,
        0,
        None,
    );
    ctx.defsubr(
        "get-variable-watchers",
        super::advice::builtin_get_variable_watchers,
        0,
        None,
    );
    ctx.defsubr(
        "modify-syntax-entry",
        super::syntax::builtin_modify_syntax_entry,
        0,
        None,
    );
    ctx.defsubr("syntax-table", super::syntax::builtin_syntax_table, 0, None);
    ctx.defsubr(
        "set-syntax-table",
        super::syntax::builtin_set_syntax_table,
        0,
        None,
    );
    ctx.defsubr("char-syntax", super::syntax::builtin_char_syntax, 0, None);
    ctx.defsubr(
        "matching-paren",
        super::syntax::builtin_matching_paren_eval,
        0,
        None,
    );
    ctx.defsubr(
        "forward-comment",
        super::syntax::builtin_forward_comment,
        0,
        None,
    );
    ctx.defsubr(
        "backward-prefix-chars",
        super::syntax::builtin_backward_prefix_chars,
        0,
        None,
    );
    ctx.defsubr("forward-word", super::syntax::builtin_forward_word, 0, None);
    ctx.defsubr("scan-lists", super::syntax::builtin_scan_lists, 0, None);
    ctx.defsubr("scan-sexps", super::syntax::builtin_scan_sexps, 0, None);
    ctx.defsubr(
        "parse-partial-sexp",
        super::syntax::builtin_parse_partial_sexp,
        0,
        None,
    );
    ctx.defsubr(
        "skip-syntax-forward",
        super::syntax::builtin_skip_syntax_forward,
        0,
        None,
    );
    ctx.defsubr(
        "skip-syntax-backward",
        super::syntax::builtin_skip_syntax_backward,
        0,
        None,
    );
    ctx.defsubr(
        "start-kbd-macro",
        super::kmacro::builtin_start_kbd_macro,
        0,
        None,
    );
    ctx.defsubr(
        "end-kbd-macro",
        super::kmacro::builtin_end_kbd_macro,
        0,
        None,
    );
    ctx.defsubr(
        "call-last-kbd-macro",
        super::kmacro::builtin_call_last_kbd_macro,
        0,
        None,
    );
    ctx.defsubr(
        "execute-kbd-macro",
        super::kmacro::builtin_execute_kbd_macro,
        0,
        None,
    );
    ctx.defsubr(
        "store-kbd-macro-event",
        super::kmacro::builtin_store_kbd_macro_event,
        0,
        None,
    );
    ctx.defsubr(
        "put-text-property",
        super::textprop::builtin_put_text_property,
        0,
        None,
    );
    ctx.defsubr(
        "get-text-property",
        super::textprop::builtin_get_text_property,
        0,
        None,
    );
    ctx.defsubr(
        "get-char-property",
        super::textprop::builtin_get_char_property,
        0,
        None,
    );
    ctx.defsubr("get-pos-property", builtin_get_pos_property, 0, None);
    ctx.defsubr(
        "add-face-text-property",
        super::textprop::builtin_add_face_text_property,
        0,
        None,
    );
    ctx.defsubr(
        "add-text-properties",
        super::textprop::builtin_add_text_properties,
        0,
        None,
    );
    ctx.defsubr(
        "set-text-properties",
        super::textprop::builtin_set_text_properties,
        0,
        None,
    );
    ctx.defsubr(
        "remove-text-properties",
        super::textprop::builtin_remove_text_properties,
        0,
        None,
    );
    ctx.defsubr(
        "text-properties-at",
        super::textprop::builtin_text_properties_at,
        0,
        None,
    );
    ctx.defsubr(
        "get-display-property",
        super::textprop::builtin_get_display_property,
        0,
        None,
    );
    ctx.defsubr(
        "next-single-char-property-change",
        builtin_next_single_char_property_change,
        0,
        None,
    );
    ctx.defsubr(
        "previous-single-char-property-change",
        builtin_previous_single_char_property_change,
        0,
        None,
    );
    ctx.defsubr(
        "next-property-change",
        super::textprop::builtin_next_property_change,
        0,
        None,
    );
    ctx.defsubr(
        "next-char-property-change",
        builtin_next_char_property_change,
        0,
        None,
    );
    ctx.defsubr(
        "previous-property-change",
        builtin_previous_property_change,
        0,
        None,
    );
    ctx.defsubr(
        "previous-char-property-change",
        builtin_previous_char_property_change,
        0,
        None,
    );
    ctx.defsubr(
        "text-property-any",
        super::textprop::builtin_text_property_any,
        0,
        None,
    );
    ctx.defsubr(
        "text-property-not-all",
        super::textprop::builtin_text_property_not_all,
        0,
        None,
    );
    ctx.defsubr(
        "next-overlay-change",
        super::textprop::builtin_next_overlay_change,
        0,
        None,
    );
    ctx.defsubr(
        "previous-overlay-change",
        super::textprop::builtin_previous_overlay_change,
        0,
        None,
    );
    ctx.defsubr(
        "make-overlay",
        super::textprop::builtin_make_overlay,
        0,
        None,
    );
    ctx.defsubr(
        "delete-overlay",
        super::textprop::builtin_delete_overlay,
        0,
        None,
    );
    ctx.defsubr("overlay-put", super::textprop::builtin_overlay_put, 0, None);
    ctx.defsubr("overlay-get", super::textprop::builtin_overlay_get, 0, None);
    ctx.defsubr("overlays-at", super::textprop::builtin_overlays_at, 0, None);
    ctx.defsubr("overlays-in", super::textprop::builtin_overlays_in, 0, None);
    ctx.defsubr(
        "move-overlay",
        super::textprop::builtin_move_overlay,
        0,
        None,
    );
    ctx.defsubr(
        "overlay-start",
        super::textprop::builtin_overlay_start,
        0,
        None,
    );
    ctx.defsubr("overlay-end", super::textprop::builtin_overlay_end, 0, None);
    ctx.defsubr(
        "overlay-buffer",
        super::textprop::builtin_overlay_buffer,
        0,
        None,
    );
    ctx.defsubr(
        "overlay-properties",
        super::textprop::builtin_overlay_properties,
        0,
        None,
    );
    ctx.defsubr("overlayp", super::textprop::builtin_overlayp, 0, None);
    ctx.defsubr("bobp", super::navigation::builtin_bobp, 0, None);
    ctx.defsubr("eobp", super::navigation::builtin_eobp, 0, None);
    ctx.defsubr("bolp", super::navigation::builtin_bolp, 0, None);
    ctx.defsubr("eolp", super::navigation::builtin_eolp, 0, None);
    ctx.defsubr("pos-bol", builtin_pos_bol, 0, None);
    ctx.defsubr(
        "line-end-position",
        super::navigation::builtin_line_end_position,
        0,
        None,
    );
    ctx.defsubr("pos-eol", builtin_pos_eol, 0, None);
    ctx.defsubr(
        "line-number-at-pos",
        super::navigation::builtin_line_number_at_pos,
        0,
        None,
    );
    ctx.defsubr(
        "forward-line",
        super::navigation::builtin_forward_line,
        0,
        None,
    );
    ctx.defsubr(
        "beginning-of-line",
        super::navigation::builtin_beginning_of_line,
        0,
        None,
    );
    ctx.defsubr(
        "end-of-line",
        super::navigation::builtin_end_of_line,
        0,
        None,
    );
    ctx.defsubr(
        "forward-char",
        super::navigation::builtin_forward_char,
        0,
        None,
    );
    ctx.defsubr(
        "backward-char",
        super::navigation::builtin_backward_char,
        0,
        None,
    );
    ctx.defsubr(
        "skip-chars-forward",
        super::navigation::builtin_skip_chars_forward,
        0,
        None,
    );
    ctx.defsubr(
        "skip-chars-backward",
        super::navigation::builtin_skip_chars_backward,
        0,
        None,
    );
    ctx.defsubr("mark-marker", super::marker::builtin_mark_marker, 0, None);
    ctx.defsubr(
        "region-beginning",
        super::navigation::builtin_region_beginning,
        0,
        None,
    );
    ctx.defsubr("region-end", super::navigation::builtin_region_end, 0, None);
    ctx.defsubr(
        "transient-mark-mode",
        super::navigation::builtin_transient_mark_mode,
        0,
        None,
    );
    ctx.defsubr(
        "make-local-variable",
        super::custom::builtin_make_local_variable,
        0,
        None,
    );
    ctx.defsubr(
        "local-variable-p",
        super::custom::builtin_local_variable_p,
        0,
        None,
    );
    ctx.defsubr(
        "buffer-local-variables",
        super::custom::builtin_buffer_local_variables,
        0,
        None,
    );
    ctx.defsubr(
        "kill-local-variable",
        super::custom::builtin_kill_local_variable,
        0,
        None,
    );
    ctx.defsubr(
        "default-value",
        super::custom::builtin_default_value,
        0,
        None,
    );
    ctx.defsubr("set-default", super::custom::builtin_set_default, 0, None);
    ctx.defsubr(
        "set-default-toplevel-value",
        builtin_set_default_toplevel_value,
        0,
        None,
    );
    ctx.defsubr("autoload", super::autoload::builtin_autoload, 0, None);
    ctx.defsubr(
        "autoload-do-load",
        super::autoload::builtin_autoload_do_load,
        0,
        None,
    );
    ctx.defsubr(
        "symbol-file",
        super::autoload::builtin_symbol_file_eval,
        0,
        None,
    );
    ctx.defsubr(
        "downcase-region",
        super::casefiddle::builtin_downcase_region,
        0,
        None,
    );
    ctx.defsubr(
        "upcase-region",
        super::casefiddle::builtin_upcase_region,
        0,
        None,
    );
    ctx.defsubr(
        "capitalize-region",
        super::casefiddle::builtin_capitalize_region,
        0,
        None,
    );
    ctx.defsubr(
        "downcase-word",
        super::casefiddle::builtin_downcase_word,
        0,
        None,
    );
    ctx.defsubr(
        "upcase-word",
        super::casefiddle::builtin_upcase_word,
        0,
        None,
    );
    ctx.defsubr(
        "capitalize-word",
        super::casefiddle::builtin_capitalize_word,
        0,
        None,
    );
    ctx.defsubr("indent-to", super::indent::builtin_indent_to_eval, 0, None);
    ctx.defsubr(
        "selected-window",
        super::window_cmds::builtin_selected_window,
        0,
        None,
    );
    ctx.defsubr(
        "old-selected-window",
        super::window_cmds::builtin_old_selected_window,
        0,
        None,
    );
    ctx.defsubr(
        "minibuffer-window",
        super::window_cmds::builtin_minibuffer_window,
        0,
        None,
    );
    ctx.defsubr(
        "window-parameter",
        super::window_cmds::builtin_window_parameter,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-parameter",
        super::window_cmds::builtin_set_window_parameter,
        0,
        None,
    );
    ctx.defsubr(
        "window-parameters",
        super::window_cmds::builtin_window_parameters,
        0,
        None,
    );
    ctx.defsubr(
        "window-parent",
        super::window_cmds::builtin_window_parent,
        0,
        None,
    );
    ctx.defsubr(
        "window-top-child",
        super::window_cmds::builtin_window_top_child,
        0,
        None,
    );
    ctx.defsubr(
        "window-left-child",
        super::window_cmds::builtin_window_left_child,
        0,
        None,
    );
    ctx.defsubr(
        "window-next-sibling",
        super::window_cmds::builtin_window_next_sibling,
        0,
        None,
    );
    ctx.defsubr(
        "window-prev-sibling",
        super::window_cmds::builtin_window_prev_sibling,
        0,
        None,
    );
    ctx.defsubr(
        "window-normal-size",
        super::window_cmds::builtin_window_normal_size,
        0,
        None,
    );
    ctx.defsubr(
        "window-display-table",
        super::window_cmds::builtin_window_display_table,
        0,
        None,
    );
    ctx.defsubr(
        "window-cursor-type",
        super::window_cmds::builtin_window_cursor_type,
        0,
        None,
    );
    ctx.defsubr(
        "window-buffer",
        super::window_cmds::builtin_window_buffer,
        0,
        None,
    );
    ctx.defsubr(
        "window-start",
        super::window_cmds::builtin_window_start,
        0,
        None,
    );
    ctx.defsubr(
        "window-end",
        super::window_cmds::builtin_window_end,
        0,
        None,
    );
    ctx.defsubr(
        "window-point",
        super::window_cmds::builtin_window_point,
        0,
        None,
    );
    ctx.defsubr(
        "window-use-time",
        super::window_cmds::builtin_window_use_time,
        0,
        None,
    );
    ctx.defsubr(
        "window-bump-use-time",
        super::window_cmds::builtin_window_bump_use_time,
        0,
        None,
    );
    ctx.defsubr(
        "window-old-point",
        super::window_cmds::builtin_window_old_point,
        0,
        None,
    );
    ctx.defsubr(
        "window-old-buffer",
        super::window_cmds::builtin_window_old_buffer,
        0,
        None,
    );
    ctx.defsubr(
        "window-prev-buffers",
        super::window_cmds::builtin_window_prev_buffers,
        0,
        None,
    );
    ctx.defsubr(
        "window-next-buffers",
        super::window_cmds::builtin_window_next_buffers,
        0,
        None,
    );
    ctx.defsubr(
        "window-left-column",
        super::window_cmds::builtin_window_left_column,
        0,
        None,
    );
    ctx.defsubr(
        "window-top-line",
        super::window_cmds::builtin_window_top_line,
        0,
        None,
    );
    ctx.defsubr(
        "window-pixel-left",
        super::window_cmds::builtin_window_pixel_left,
        0,
        None,
    );
    ctx.defsubr(
        "window-pixel-top",
        super::window_cmds::builtin_window_pixel_top,
        0,
        None,
    );
    ctx.defsubr(
        "window-hscroll",
        super::window_cmds::builtin_window_hscroll,
        0,
        None,
    );
    ctx.defsubr(
        "window-vscroll",
        super::window_cmds::builtin_window_vscroll,
        0,
        None,
    );
    ctx.defsubr(
        "window-margins",
        super::window_cmds::builtin_window_margins,
        0,
        None,
    );
    ctx.defsubr(
        "window-fringes",
        super::window_cmds::builtin_window_fringes,
        0,
        None,
    );
    ctx.defsubr(
        "window-scroll-bars",
        super::window_cmds::builtin_window_scroll_bars,
        0,
        None,
    );
    ctx.defsubr(
        "window-pixel-height",
        super::window_cmds::builtin_window_pixel_height,
        0,
        None,
    );
    ctx.defsubr(
        "window-pixel-width",
        super::window_cmds::builtin_window_pixel_width,
        0,
        None,
    );
    ctx.defsubr(
        "window-body-height",
        super::window_cmds::builtin_window_body_height,
        0,
        None,
    );
    ctx.defsubr(
        "window-body-width",
        super::window_cmds::builtin_window_body_width,
        0,
        None,
    );
    ctx.defsubr(
        "window-text-height",
        super::window_cmds::builtin_window_text_height,
        0,
        None,
    );
    ctx.defsubr(
        "window-text-width",
        super::window_cmds::builtin_window_text_width,
        0,
        None,
    );
    ctx.defsubr(
        "window-total-height",
        super::window_cmds::builtin_window_total_height,
        0,
        None,
    );
    ctx.defsubr(
        "window-total-width",
        super::window_cmds::builtin_window_total_width,
        0,
        None,
    );
    ctx.defsubr(
        "window-list",
        super::window_cmds::builtin_window_list,
        0,
        None,
    );
    ctx.defsubr(
        "window-list-1",
        super::window_cmds::builtin_window_list_1,
        0,
        None,
    );
    ctx.defsubr(
        "get-buffer-window",
        super::window_cmds::builtin_get_buffer_window,
        0,
        None,
    );
    ctx.defsubr(
        "window-dedicated-p",
        super::window_cmds::builtin_window_dedicated_p,
        0,
        None,
    );
    ctx.defsubr(
        "window-minibuffer-p",
        super::window_cmds::builtin_window_minibuffer_p,
        0,
        None,
    );
    ctx.defsubr("window-at", super::window_cmds::builtin_window_at, 0, None);
    ctx.defsubr(
        "window-live-p",
        super::window_cmds::builtin_window_live_p,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-start",
        super::window_cmds::builtin_set_window_start,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-hscroll",
        super::window_cmds::builtin_set_window_hscroll,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-margins",
        super::window_cmds::builtin_set_window_margins,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-fringes",
        super::window_cmds::builtin_set_window_fringes,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-vscroll",
        super::window_cmds::builtin_set_window_vscroll,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-point",
        super::window_cmds::builtin_set_window_point,
        0,
        None,
    );
    ctx.defsubr(
        "split-window-internal",
        builtin_split_window_internal,
        0,
        None,
    );
    ctx.defsubr(
        "delete-window",
        super::window_cmds::builtin_delete_window,
        0,
        None,
    );
    ctx.defsubr(
        "delete-other-windows",
        super::window_cmds::builtin_delete_other_windows,
        0,
        None,
    );
    ctx.defsubr(
        "select-window",
        super::window_cmds::builtin_select_window,
        0,
        None,
    );
    ctx.defsubr("scroll-up", super::window_cmds::builtin_scroll_up, 0, None);
    ctx.defsubr(
        "scroll-down",
        super::window_cmds::builtin_scroll_down,
        0,
        None,
    );
    ctx.defsubr(
        "scroll-left",
        super::window_cmds::builtin_scroll_left,
        0,
        None,
    );
    ctx.defsubr(
        "scroll-right",
        super::window_cmds::builtin_scroll_right,
        0,
        None,
    );
    ctx.defsubr(
        "window-resize-apply",
        super::window_cmds::builtin_window_resize_apply,
        0,
        None,
    );
    ctx.defsubr("recenter", super::window_cmds::builtin_recenter, 0, None);
    ctx.defsubr("vertical-motion", builtin_vertical_motion, 0, None);
    ctx.defsubr(
        "next-window",
        super::window_cmds::builtin_next_window,
        0,
        None,
    );
    ctx.defsubr(
        "previous-window",
        super::window_cmds::builtin_previous_window,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-buffer",
        super::window_cmds::builtin_set_window_buffer,
        0,
        None,
    );
    ctx.defsubr(
        "current-window-configuration",
        builtin_current_window_configuration,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-configuration",
        builtin_set_window_configuration,
        0,
        None,
    );
    ctx.defsubr(
        "old-selected-frame",
        builtin_old_selected_frame_eval,
        0,
        None,
    );
    ctx.defsubr(
        "selected-frame",
        super::window_cmds::builtin_selected_frame,
        0,
        None,
    );
    ctx.defsubr(
        "mouse-pixel-position",
        builtin_mouse_pixel_position_eval,
        0,
        None,
    );
    ctx.defsubr("mouse-position", builtin_mouse_position_eval, 0, None);
    ctx.defsubr("next-frame", builtin_next_frame_eval, 0, None);
    ctx.defsubr("previous-frame", builtin_previous_frame_eval, 0, None);
    ctx.defsubr(
        "select-frame",
        super::window_cmds::builtin_select_frame,
        0,
        None,
    );
    ctx.defsubr(
        "last-nonminibuffer-frame",
        super::window_cmds::builtin_selected_frame,
        0,
        None,
    );
    ctx.defsubr(
        "visible-frame-list",
        super::window_cmds::builtin_visible_frame_list,
        0,
        None,
    );
    ctx.defsubr(
        "frame-list",
        super::window_cmds::builtin_frame_list,
        0,
        None,
    );
    ctx.defsubr(
        "x-create-frame",
        super::window_cmds::builtin_x_create_frame,
        0,
        None,
    );
    ctx.defsubr(
        "make-frame-visible",
        super::window_cmds::builtin_make_frame_visible,
        0,
        None,
    );
    ctx.defsubr(
        "make-frame",
        super::window_cmds::builtin_make_frame,
        0,
        None,
    );
    ctx.defsubr(
        "iconify-frame",
        super::window_cmds::builtin_iconify_frame,
        0,
        None,
    );
    ctx.defsubr(
        "delete-frame",
        super::window_cmds::builtin_delete_frame,
        0,
        None,
    );
    ctx.defsubr(
        "frame-char-height",
        super::window_cmds::builtin_frame_char_height,
        0,
        None,
    );
    ctx.defsubr(
        "frame-char-width",
        super::window_cmds::builtin_frame_char_width,
        0,
        None,
    );
    ctx.defsubr(
        "frame-native-height",
        super::window_cmds::builtin_frame_native_height,
        0,
        None,
    );
    ctx.defsubr(
        "frame-native-width",
        super::window_cmds::builtin_frame_native_width,
        0,
        None,
    );
    ctx.defsubr(
        "frame-text-cols",
        super::window_cmds::builtin_frame_text_cols,
        0,
        None,
    );
    ctx.defsubr(
        "frame-text-height",
        super::window_cmds::builtin_frame_text_height,
        0,
        None,
    );
    ctx.defsubr(
        "frame-text-lines",
        super::window_cmds::builtin_frame_text_lines,
        0,
        None,
    );
    ctx.defsubr(
        "frame-text-width",
        super::window_cmds::builtin_frame_text_width,
        0,
        None,
    );
    ctx.defsubr(
        "frame-total-cols",
        super::window_cmds::builtin_frame_total_cols,
        0,
        None,
    );
    ctx.defsubr(
        "frame-total-lines",
        super::window_cmds::builtin_frame_total_lines,
        0,
        None,
    );
    ctx.defsubr(
        "frame-position",
        super::window_cmds::builtin_frame_position,
        0,
        None,
    );
    ctx.defsubr(
        "frame-parameters",
        super::window_cmds::builtin_frame_parameters,
        0,
        None,
    );
    ctx.defsubr(
        "set-frame-height",
        super::window_cmds::builtin_set_frame_height,
        0,
        None,
    );
    ctx.defsubr(
        "set-frame-width",
        super::window_cmds::builtin_set_frame_width,
        0,
        None,
    );
    ctx.defsubr(
        "set-frame-size",
        super::window_cmds::builtin_set_frame_size,
        0,
        None,
    );
    ctx.defsubr(
        "set-frame-position",
        super::window_cmds::builtin_set_frame_position,
        0,
        None,
    );
    ctx.defsubr(
        "frame-visible-p",
        super::window_cmds::builtin_frame_visible_p,
        0,
        None,
    );
    ctx.defsubr(
        "frame-live-p",
        super::window_cmds::builtin_frame_live_p,
        0,
        None,
    );
    ctx.defsubr(
        "frame-first-window",
        super::window_cmds::builtin_frame_first_window,
        0,
        None,
    );
    ctx.defsubr(
        "frame-root-window",
        super::window_cmds::builtin_frame_root_window,
        0,
        None,
    );
    ctx.defsubr("windowp", super::window_cmds::builtin_windowp, 0, None);
    ctx.defsubr(
        "window-valid-p",
        super::window_cmds::builtin_window_valid_p,
        0,
        None,
    );
    ctx.defsubr(
        "window-height",
        super::window_cmds::builtin_window_height,
        0,
        None,
    );
    ctx.defsubr(
        "window-width",
        super::window_cmds::builtin_window_width,
        0,
        None,
    );
    ctx.defsubr("framep", super::window_cmds::builtin_framep, 0, None);
    ctx.defsubr(
        "window-frame",
        super::window_cmds::builtin_window_frame,
        0,
        None,
    );
    ctx.defsubr("frame-id", builtin_frame_id_eval, 0, None);
    ctx.defsubr("frame-root-frame", builtin_frame_root_frame_eval, 0, None);
    ctx.defsubr(
        "x-open-connection",
        super::display::builtin_x_open_connection_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-get-resource",
        super::display::builtin_x_get_resource_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-list-fonts",
        super::display::builtin_x_list_fonts_eval,
        0,
        None,
    );
    ctx.defsubr(
        "window-system",
        super::display::builtin_window_system_eval,
        0,
        None,
    );
    ctx.defsubr("current-idle-time", builtin_current_idle_time_eval, 0, None);
    ctx.defsubr(
        "x-server-version",
        super::display::builtin_x_server_version_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-server-input-extension-version",
        super::display::builtin_x_server_input_extension_version_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-server-vendor",
        super::display::builtin_x_server_vendor_eval,
        0,
        None,
    );
    ctx.defsubr(
        "display-color-cells",
        super::display::builtin_display_color_cells_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-mm-height",
        super::display::builtin_x_display_mm_height_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-mm-width",
        super::display::builtin_x_display_mm_width_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-planes",
        super::display::builtin_x_display_planes_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-screens",
        super::display::builtin_x_display_screens_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-close-connection",
        super::display::builtin_x_close_connection_eval,
        0,
        None,
    );
    ctx.defsubr(
        "call-interactively",
        super::interactive::builtin_call_interactively,
        0,
        None,
    );
    ctx.defsubr(
        "commandp",
        super::interactive::builtin_commandp_interactive,
        0,
        None,
    );
    ctx.defsubr(
        "command-remapping",
        super::interactive::builtin_command_remapping,
        0,
        None,
    );
    ctx.defsubr(
        "self-insert-command",
        super::interactive::builtin_self_insert_command,
        0,
        None,
    );
    ctx.defsubr(
        "key-binding",
        super::interactive::builtin_key_binding,
        0,
        None,
    );
    ctx.defsubr(
        "where-is-internal",
        super::interactive::builtin_where_is_internal,
        0,
        None,
    );
    ctx.defsubr(
        "this-command-keys",
        super::interactive::builtin_this_command_keys,
        0,
        None,
    );
    ctx.defsubr("format", builtin_format_eval, 0, None);
    ctx.defsubr("format-message", builtin_format_message_eval, 0, None);
    ctx.defsubr("message-box", builtin_message_box_eval, 0, None);
    ctx.defsubr("message-or-box", builtin_message_or_box_eval, 0, None);
    ctx.defsubr("current-message", builtin_current_message_eval, 0, None);
    ctx.defsubr(
        "read-from-string",
        super::reader::builtin_read_from_string,
        0,
        None,
    );
    ctx.defsubr("read", super::reader::builtin_read, 0, None);
    ctx.defsubr(
        "read-from-minibuffer",
        super::reader::builtin_read_from_minibuffer,
        0,
        None,
    );
    ctx.defsubr("read-string", super::reader::builtin_read_string, 0, None);
    ctx.defsubr(
        "completing-read",
        super::reader::builtin_completing_read,
        0,
        None,
    );
    ctx.defsubr(
        "read-buffer",
        super::minibuffer::builtin_read_buffer,
        0,
        None,
    );
    ctx.defsubr(
        "read-command",
        super::minibuffer::builtin_read_command,
        0,
        None,
    );
    ctx.defsubr(
        "read-variable",
        super::minibuffer::builtin_read_variable,
        0,
        None,
    );
    ctx.defsubr(
        "try-completion",
        super::minibuffer::builtin_try_completion_eval,
        0,
        None,
    );
    ctx.defsubr(
        "all-completions",
        super::minibuffer::builtin_all_completions_eval,
        0,
        None,
    );
    ctx.defsubr(
        "test-completion",
        super::minibuffer::builtin_test_completion_eval,
        0,
        None,
    );
    ctx.defsubr(
        "input-pending-p",
        super::reader::builtin_input_pending_p,
        0,
        None,
    );
    ctx.defsubr(
        "discard-input",
        super::reader::builtin_discard_input,
        0,
        None,
    );
    ctx.defsubr(
        "current-input-mode",
        super::reader::builtin_current_input_mode,
        0,
        None,
    );
    ctx.defsubr(
        "set-input-mode",
        super::reader::builtin_set_input_mode,
        0,
        None,
    );
    ctx.defsubr(
        "set-input-interrupt-mode",
        super::reader::builtin_set_input_interrupt_mode,
        0,
        None,
    );
    ctx.defsubr(
        "read-key-sequence",
        super::reader::builtin_read_key_sequence,
        0,
        None,
    );
    ctx.defsubr(
        "read-key-sequence-vector",
        super::reader::builtin_read_key_sequence_vector,
        0,
        None,
    );
    ctx.defsubr("recent-keys", builtin_recent_keys, 0, None);
    ctx.defsubr(
        "minibufferp",
        super::minibuffer::builtin_minibufferp_eval,
        0,
        None,
    );
    ctx.defsubr(
        "minibuffer-contents",
        super::minibuffer::builtin_minibuffer_contents,
        0,
        None,
    );
    ctx.defsubr(
        "minibuffer-contents-no-properties",
        super::minibuffer::builtin_minibuffer_contents_no_properties,
        0,
        None,
    );
    ctx.defsubr(
        "minibuffer-depth",
        super::minibuffer::builtin_minibuffer_depth_eval,
        0,
        None,
    );
    ctx.defsubr("princ", builtin_princ_eval, 0, None);
    ctx.defsubr("prin1", builtin_prin1_eval, 0, None);
    ctx.defsubr("prin1-to-string", builtin_prin1_to_string_eval, 0, None);
    ctx.defsubr("print", builtin_print_eval, 0, None);
    ctx.defsubr("terpri", builtin_terpri_eval, 0, None);
    ctx.defsubr("write-char", builtin_write_char_eval, 0, None);
    ctx.defsubr(
        "backtrace--locals",
        super::misc::builtin_backtrace_locals,
        0,
        None,
    );
    ctx.defsubr(
        "backtrace-debug",
        super::misc::builtin_backtrace_debug,
        0,
        None,
    );
    ctx.defsubr(
        "backtrace-eval",
        super::misc::builtin_backtrace_eval,
        0,
        None,
    );
    ctx.defsubr(
        "backtrace-frame--internal",
        super::misc::builtin_backtrace_frame_internal,
        0,
        None,
    );
    ctx.defsubr(
        "recursion-depth",
        super::misc::builtin_recursion_depth,
        0,
        None,
    );
    ctx.defsubr("kill-emacs", builtin_kill_emacs_eval, 0, None);
    ctx.defsubr(
        "exit-recursive-edit",
        super::minibuffer::builtin_exit_recursive_edit,
        0,
        None,
    );
    ctx.defsubr(
        "abort-recursive-edit",
        super::minibuffer::builtin_abort_recursive_edit,
        0,
        None,
    );
    ctx.defsubr("make-thread", super::threads::builtin_make_thread, 0, None);
    ctx.defsubr("thread-join", super::threads::builtin_thread_join, 0, None);
    ctx.defsubr(
        "thread-yield",
        super::threads::builtin_thread_yield,
        0,
        None,
    );
    ctx.defsubr("thread-name", super::threads::builtin_thread_name, 0, None);
    ctx.defsubr(
        "thread-live-p",
        super::threads::builtin_thread_live_p,
        0,
        None,
    );
    ctx.defsubr("threadp", super::threads::builtin_threadp, 0, None);
    ctx.defsubr(
        "thread-signal",
        super::threads::builtin_thread_signal,
        0,
        None,
    );
    ctx.defsubr(
        "current-thread",
        super::threads::builtin_current_thread,
        0,
        None,
    );
    ctx.defsubr("all-threads", super::threads::builtin_all_threads, 0, None);
    ctx.defsubr(
        "thread-last-error",
        super::threads::builtin_thread_last_error,
        0,
        None,
    );
    ctx.defsubr("make-mutex", super::threads::builtin_make_mutex, 0, None);
    ctx.defsubr("mutex-name", super::threads::builtin_mutex_name, 0, None);
    ctx.defsubr("mutex-lock", super::threads::builtin_mutex_lock, 0, None);
    ctx.defsubr(
        "mutex-unlock",
        super::threads::builtin_mutex_unlock,
        0,
        None,
    );
    ctx.defsubr("mutexp", super::threads::builtin_mutexp, 0, None);
    ctx.defsubr(
        "make-condition-variable",
        super::threads::builtin_make_condition_variable,
        0,
        None,
    );
    ctx.defsubr(
        "condition-variable-p",
        super::threads::builtin_condition_variable_p,
        0,
        None,
    );
    ctx.defsubr(
        "condition-name",
        super::threads::builtin_condition_name,
        0,
        None,
    );
    ctx.defsubr(
        "condition-mutex",
        super::threads::builtin_condition_mutex,
        0,
        None,
    );
    ctx.defsubr(
        "condition-wait",
        super::threads::builtin_condition_wait,
        0,
        None,
    );
    ctx.defsubr(
        "condition-notify",
        super::threads::builtin_condition_notify,
        0,
        None,
    );
    ctx.defsubr(
        "undo-boundary",
        super::undo::builtin_undo_boundary_eval,
        0,
        None,
    );
    ctx.defsubr("maphash", super::hashtab::builtin_maphash, 0, None);
    ctx.defsubr("mapatoms", super::hashtab::builtin_mapatoms, 0, None);
    ctx.defsubr("unintern", super::hashtab::builtin_unintern, 0, None);
    ctx.defsubr("set-marker", super::marker::builtin_set_marker, 0, None);
    ctx.defsubr("move-marker", super::marker::builtin_move_marker, 0, None);
    ctx.defsubr(
        "marker-position",
        super::marker::builtin_marker_position_eval,
        0,
        None,
    );
    ctx.defsubr(
        "marker-buffer",
        super::marker::builtin_marker_buffer_eval,
        0,
        None,
    );
    ctx.defsubr(
        "copy-marker",
        super::marker::builtin_copy_marker_eval,
        0,
        None,
    );
    ctx.defsubr("point-marker", super::marker::builtin_point_marker, 0, None);
    ctx.defsubr(
        "point-min-marker",
        super::marker::builtin_point_min_marker,
        0,
        None,
    );
    ctx.defsubr(
        "point-max-marker",
        super::marker::builtin_point_max_marker,
        0,
        None,
    );
    ctx.defsubr(
        "current-case-table",
        super::casetab::builtin_current_case_table_eval,
        0,
        None,
    );
    ctx.defsubr(
        "standard-case-table",
        super::casetab::builtin_standard_case_table_eval,
        0,
        None,
    );
    ctx.defsubr(
        "set-case-table",
        super::casetab::builtin_set_case_table_eval,
        0,
        None,
    );
    ctx.defsubr(
        "define-category",
        super::category::builtin_define_category_eval,
        0,
        None,
    );
    ctx.defsubr(
        "category-docstring",
        super::category::builtin_category_docstring_eval,
        0,
        None,
    );
    ctx.defsubr(
        "modify-category-entry",
        super::category::builtin_modify_category_entry,
        0,
        None,
    );
    ctx.defsubr(
        "char-category-set",
        super::category::builtin_char_category_set,
        0,
        None,
    );
    ctx.defsubr(
        "category-table",
        super::category::builtin_category_table_eval,
        0,
        None,
    );
    ctx.defsubr(
        "set-category-table",
        super::category::builtin_set_category_table_eval,
        0,
        None,
    );
    ctx.defsubr(
        "map-char-table",
        super::chartable::builtin_map_char_table,
        0,
        None,
    );
    ctx.defsubr("assoc", builtin_assoc_eval, 0, None);
    ctx.defsubr("plist-member", builtin_plist_member, 0, None);
    ctx.defsubr(
        "json-parse-buffer",
        super::json::builtin_json_parse_buffer,
        0,
        None,
    );
    ctx.defsubr("json-insert", super::json::builtin_json_insert, 0, None);
    ctx.defsubr("documentation", super::doc::builtin_documentation, 0, None);
    ctx.defsubr(
        "documentation-property",
        super::doc::builtin_documentation_property_eval,
        0,
        None,
    );
    ctx.defsubr(
        "current-indentation",
        super::indent::builtin_current_indentation_eval,
        0,
        None,
    );
    ctx.defsubr(
        "current-column",
        super::indent::builtin_current_column_eval,
        0,
        None,
    );
    ctx.defsubr(
        "move-to-column",
        super::indent::builtin_move_to_column_eval,
        0,
        None,
    );
    ctx.defsubr("eval-buffer", super::lread::builtin_eval_buffer, 0, None);
    ctx.defsubr("eval-region", super::lread::builtin_eval_region, 0, None);
    ctx.defsubr(
        "read-char-exclusive",
        super::lread::builtin_read_char_exclusive,
        0,
        None,
    );
    ctx.defsubr(
        "insert-before-markers",
        super::editfns::builtin_insert_before_markers,
        0,
        None,
    );
    ctx.defsubr("delete-char", super::editfns::builtin_delete_char, 0, None);
    ctx.defsubr(
        "following-char",
        super::editfns::builtin_following_char,
        0,
        None,
    );
    ctx.defsubr(
        "preceding-char",
        super::editfns::builtin_preceding_char,
        0,
        None,
    );
    ctx.defsubr("font-at", super::font::builtin_font_at_eval, 0, None);
    ctx.defsubr("face-font", super::font::builtin_face_font_eval, 0, None);
    ctx.defsubr(
        "access-file",
        super::fileio::builtin_access_file_eval,
        0,
        None,
    );
    ctx.defsubr(
        "expand-file-name",
        super::fileio::builtin_expand_file_name_eval,
        0,
        None,
    );
    ctx.defsubr(
        "delete-file-internal",
        super::fileio::builtin_delete_file_internal_eval,
        0,
        None,
    );
    ctx.defsubr(
        "rename-file",
        super::fileio::builtin_rename_file_eval,
        0,
        None,
    );
    ctx.defsubr("copy-file", super::fileio::builtin_copy_file_eval, 0, None);
    ctx.defsubr(
        "add-name-to-file",
        super::fileio::builtin_add_name_to_file_eval,
        0,
        None,
    );
    ctx.defsubr(
        "make-symbolic-link",
        super::fileio::builtin_make_symbolic_link_eval,
        0,
        None,
    );
    ctx.defsubr(
        "directory-files",
        super::fileio::builtin_directory_files_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-attributes",
        super::dired::builtin_file_attributes_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-exists-p",
        super::fileio::builtin_file_exists_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-readable-p",
        super::fileio::builtin_file_readable_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-writable-p",
        super::fileio::builtin_file_writable_p_eval,
        0,
        None,
    );
    ctx.defsubr("file-acl", super::fileio::builtin_file_acl_eval, 0, None);
    ctx.defsubr(
        "file-executable-p",
        super::fileio::builtin_file_executable_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-locked-p",
        super::fileio::builtin_file_locked_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-selinux-context",
        super::fileio::builtin_file_selinux_context_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-system-info",
        super::fileio::builtin_file_system_info_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-directory-p",
        super::fileio::builtin_file_directory_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-regular-p",
        super::fileio::builtin_file_regular_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-symlink-p",
        super::fileio::builtin_file_symlink_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-modes",
        super::fileio::builtin_file_modes_eval,
        0,
        None,
    );
    ctx.defsubr(
        "set-file-modes",
        super::fileio::builtin_set_file_modes_eval,
        0,
        None,
    );
    ctx.defsubr(
        "set-file-times",
        super::fileio::builtin_set_file_times_eval,
        0,
        None,
    );
    ctx.defsubr(
        "error-message-string",
        super::errors::builtin_error_message_string,
        0,
        None,
    );
    ctx.defsubr("char-equal", builtin_char_equal, 0, None);
    ctx.defsubr(
        "macrop",
        super::builtins::symbols::builtin_macrop_eval,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-inherit-coding-system-flag",
        super::process::builtin_set_process_inherit_coding_system_flag,
        0,
        None,
    );
    ctx.defsubr(
        "compute-motion",
        super::builtins::buffers::builtin_compute_motion,
        0,
        None,
    );
    ctx.defsubr(
        "frame-parameter",
        super::window_cmds::builtin_frame_parameter,
        0,
        None,
    );
    ctx.defsubr(
        "send-string-to-terminal",
        super::dispnew::pure::builtin_send_string_to_terminal_eval,
        0,
        None,
    );
    ctx.defsubr(
        "internal-show-cursor",
        super::dispnew::pure::builtin_internal_show_cursor_eval,
        0,
        None,
    );
    ctx.defsubr(
        "internal-show-cursor-p",
        super::dispnew::pure::builtin_internal_show_cursor_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "redraw-frame",
        super::dispnew::pure::builtin_redraw_frame_eval,
        0,
        None,
    );
    ctx.defsubr(
        "display-supports-face-attributes-p",
        super::display::builtin_display_supports_face_attributes_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "terminal-name",
        super::terminal::pure::builtin_terminal_name_eval,
        0,
        None,
    );
    ctx.defsubr(
        "terminal-live-p",
        super::terminal::pure::builtin_terminal_live_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "terminal-parameter",
        super::terminal::pure::builtin_terminal_parameter_eval,
        0,
        None,
    );
    ctx.defsubr(
        "terminal-parameters",
        super::terminal::pure::builtin_terminal_parameters_eval,
        0,
        None,
    );
    ctx.defsubr(
        "set-terminal-parameter",
        super::terminal::pure::builtin_set_terminal_parameter_eval,
        0,
        None,
    );
    ctx.defsubr(
        "tty-type",
        super::terminal::pure::builtin_tty_type_eval,
        0,
        None,
    );
    ctx.defsubr(
        "tty-top-frame",
        super::terminal::pure::builtin_tty_top_frame_eval,
        0,
        None,
    );
    ctx.defsubr(
        "tty-display-color-p",
        super::terminal::pure::builtin_tty_display_color_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "tty-display-color-cells",
        super::terminal::pure::builtin_tty_display_color_cells_eval,
        0,
        None,
    );
    ctx.defsubr(
        "tty-no-underline",
        super::terminal::pure::builtin_tty_no_underline_eval,
        0,
        None,
    );
    ctx.defsubr(
        "controlling-tty-p",
        super::terminal::pure::builtin_controlling_tty_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "suspend-tty",
        super::terminal::pure::builtin_suspend_tty_eval,
        0,
        None,
    );
    ctx.defsubr(
        "resume-tty",
        super::terminal::pure::builtin_resume_tty_eval,
        0,
        None,
    );
    ctx.defsubr(
        "frame-terminal",
        super::terminal::pure::builtin_frame_terminal_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-monitor-attributes-list",
        super::display::builtin_x_display_monitor_attributes_list_eval,
        0,
        None,
    );
    ctx.defsubr("read-char", super::reader::builtin_read_char, 0, None);
    ctx.defsubr(
        "minibuffer-innermost-command-loop-p",
        super::minibuffer::builtin_minibuffer_innermost_command_loop_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "recursive-edit",
        super::minibuffer::builtin_recursive_edit_eval,
        0,
        None,
    );
    ctx.defsubr(
        "find-coding-systems-region-internal",
        super::coding::builtin_find_coding_systems_region_internal_eval,
        0,
        None,
    );
    ctx.defsubr("posix-search-forward", builtin_re_search_forward, 0, None);
    ctx.defsubr("posix-search-backward", builtin_re_search_backward, 0, None);
    ctx.defsubr("read-event", super::lread::builtin_read_event, 0, None);
    ctx.defsubr("run-hooks", defsubr_run_hooks, 0, None);
    ctx.defsubr("load", defsubr_load, 0, None);
    ctx.defsubr("message", defsubr_message, 0, None);
    ctx.defsubr(
        "coding-system-aliases",
        defsubr_coding_system_aliases,
        0,
        None,
    );
    ctx.defsubr("coding-system-plist", defsubr_coding_system_plist, 0, None);
    ctx.defsubr("coding-system-put", defsubr_coding_system_put, 0, None);
    ctx.defsubr("coding-system-base", defsubr_coding_system_base, 0, None);
    ctx.defsubr(
        "coding-system-eol-type",
        defsubr_coding_system_eol_type,
        0,
        None,
    );
    ctx.defsubr(
        "detect-coding-string",
        defsubr_detect_coding_string,
        0,
        None,
    );
    ctx.defsubr(
        "detect-coding-region",
        defsubr_detect_coding_region,
        0,
        None,
    );
    ctx.defsubr(
        "keyboard-coding-system",
        defsubr_keyboard_coding_system,
        0,
        None,
    );
    ctx.defsubr(
        "terminal-coding-system",
        defsubr_terminal_coding_system,
        0,
        None,
    );
    ctx.defsubr(
        "coding-system-priority-list",
        defsubr_coding_system_priority_list,
        0,
        None,
    );
    ctx.defsubr(
        "integer-or-marker-p",
        |_ctx, args| builtin_integer_or_marker_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "number-or-marker-p",
        |_ctx, args| builtin_number_or_marker_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "vector-or-char-table-p",
        |_ctx, args| builtin_vector_or_char_table_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "markerp",
        |_ctx, args| super::marker::builtin_markerp(args),
        0,
        None,
    );
    ctx.defsubr(
        "marker-insertion-type",
        |_ctx, args| super::marker::builtin_marker_insertion_type(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-marker",
        |_ctx, args| super::marker::builtin_make_marker(args),
        0,
        None,
    );
    ctx.defsubr(
        "bool-vector-p",
        |_ctx, args| super::chartable::builtin_bool_vector_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-category-set",
        |_ctx, args| super::category::builtin_make_category_set(args),
        0,
        None,
    );
    ctx.defsubr(
        "function-equal",
        |_ctx, args| builtin_function_equal(args),
        0,
        None,
    );
    ctx.defsubr(
        "module-function-p",
        |_ctx, args| builtin_module_function_p(args),
        0,
        None,
    );
    ctx.defsubr("user-ptrp", |_ctx, args| builtin_user_ptrp(args), 0, None);
    ctx.defsubr(
        "symbol-with-pos-p",
        |_ctx, args| builtin_symbol_with_pos_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "symbol-with-pos-pos",
        |_ctx, args| builtin_symbol_with_pos_pos(args),
        0,
        None,
    );
    ctx.defsubr("length<", |_ctx, args| builtin_length_lt(args), 0, None);
    ctx.defsubr("length=", |_ctx, args| builtin_length_eq(args), 0, None);
    ctx.defsubr("length>", |_ctx, args| builtin_length_gt(args), 0, None);
    ctx.defsubr(
        "substring-no-properties",
        |_ctx, args| builtin_substring_no_properties(args),
        0,
        None,
    );
    ctx.defsubr("sqrt", |_ctx, args| builtin_sqrt(args), 0, None);
    ctx.defsubr("sin", |_ctx, args| builtin_sin(args), 0, None);
    ctx.defsubr("cos", |_ctx, args| builtin_cos(args), 0, None);
    ctx.defsubr("tan", |_ctx, args| builtin_tan(args), 0, None);
    ctx.defsubr("asin", |_ctx, args| builtin_asin(args), 0, None);
    ctx.defsubr("acos", |_ctx, args| builtin_acos(args), 0, None);
    ctx.defsubr("atan", |_ctx, args| builtin_atan(args), 0, None);
    ctx.defsubr("exp", |_ctx, args| builtin_exp(args), 0, None);
    ctx.defsubr("log", |_ctx, args| builtin_log(args), 0, None);
    ctx.defsubr("expt", |_ctx, args| builtin_expt(args), 0, None);
    ctx.defsubr("random", |_ctx, args| builtin_random(args), 0, None);
    ctx.defsubr("isnan", |_ctx, args| builtin_isnan(args), 0, None);
    ctx.defsubr(
        "make-string",
        |_ctx, args| builtin_make_string(args),
        0,
        None,
    );
    ctx.defsubr("string", |_ctx, args| builtin_string(args), 0, None);
    ctx.defsubr(
        "string-width",
        |_ctx, args| builtin_string_width(args),
        0,
        None,
    );
    ctx.defsubr("delete", |_ctx, args| builtin_delete(args), 0, None);
    ctx.defsubr("delq", |_ctx, args| builtin_delq(args), 0, None);
    ctx.defsubr("elt", |_ctx, args| builtin_elt(args), 0, None);
    ctx.defsubr("memql", |_ctx, args| builtin_memql(args), 0, None);
    ctx.defsubr("nconc", |_ctx, args| builtin_nconc(args), 0, None);
    ctx.defsubr("identity", |_ctx, args| builtin_identity(args), 0, None);
    ctx.defsubr("ngettext", |_ctx, args| builtin_ngettext(args), 0, None);
    ctx.defsubr(
        "secure-hash-algorithms",
        |_ctx, args| builtin_secure_hash_algorithms(args),
        0,
        None,
    );
    ctx.defsubr(
        "prefix-numeric-value",
        |_ctx, args| builtin_prefix_numeric_value(args),
        0,
        None,
    );
    ctx.defsubr("propertize", |_ctx, args| builtin_propertize(args), 0, None);
    ctx.defsubr(
        "bare-symbol",
        |_ctx, args| super::builtins_extra::builtin_bare_symbol(args),
        0,
        None,
    );
    ctx.defsubr(
        "capitalize",
        |_ctx, args| super::casefiddle::builtin_capitalize(args),
        0,
        None,
    );
    ctx.defsubr(
        "charsetp",
        |_ctx, args| super::charset::builtin_charsetp(args),
        0,
        None,
    );
    ctx.defsubr(
        "charset-plist",
        |_ctx, args| super::charset::builtin_charset_plist(args),
        0,
        None,
    );
    ctx.defsubr(
        "define-charset-internal",
        |_ctx, args| super::charset::builtin_define_charset_internal(args),
        0,
        None,
    );
    ctx.defsubr(
        "define-charset-alias",
        |_ctx, args| super::charset::builtin_define_charset_alias(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-lisp-face-p",
        |_ctx, args| super::font::builtin_internal_lisp_face_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-make-lisp-face",
        |_ctx, args| super::font::builtin_internal_make_lisp_face(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-set-lisp-face-attribute",
        |_ctx, args| super::font::builtin_internal_set_lisp_face_attribute(args),
        0,
        None,
    );
    ctx.defsubr(
        "string-to-syntax",
        |_ctx, args| builtin_string_to_syntax(args),
        0,
        None,
    );
    ctx.defsubr(
        "syntax-class-to-char",
        |_ctx, args| super::syntax::builtin_syntax_class_to_char(args),
        0,
        None,
    );
    ctx.defsubr(
        "copy-syntax-table",
        |_ctx, args| super::syntax::builtin_copy_syntax_table(args),
        0,
        None,
    );
    ctx.defsubr(
        "syntax-table-p",
        |_ctx, args| super::syntax::builtin_syntax_table_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "standard-syntax-table",
        |_ctx, args| super::syntax::builtin_standard_syntax_table(args),
        0,
        None,
    );
    ctx.defsubr(
        "current-time",
        |_ctx, args| super::timefns::builtin_current_time(args),
        0,
        None,
    );
    ctx.defsubr(
        "current-cpu-time",
        |_ctx, args| builtin_current_cpu_time(args),
        0,
        None,
    );
    ctx.defsubr(
        "get-internal-run-time",
        |_ctx, args| builtin_get_internal_run_time(args),
        0,
        None,
    );
    ctx.defsubr(
        "float-time",
        |_ctx, args| super::timefns::builtin_float_time(args),
        0,
        None,
    );
    ctx.defsubr("daemonp", |_ctx, args| builtin_daemonp(args), 0, None);
    ctx.defsubr(
        "daemon-initialized",
        |_ctx, args| builtin_daemon_initialized(args),
        0,
        None,
    );
    ctx.defsubr(
        "flush-standard-output",
        |_ctx, args| builtin_flush_standard_output(args),
        0,
        None,
    );
    ctx.defsubr(
        "force-mode-line-update",
        |_ctx, args| builtin_force_mode_line_update(args),
        0,
        None,
    );
    ctx.defsubr(
        "invocation-directory",
        |_ctx, args| builtin_invocation_directory(args),
        0,
        None,
    );
    ctx.defsubr(
        "invocation-name",
        |_ctx, args| builtin_invocation_name(args),
        0,
        None,
    );
    ctx.defsubr(
        "file-name-directory",
        |_ctx, args| super::fileio::builtin_file_name_directory(args),
        0,
        None,
    );
    ctx.defsubr(
        "file-name-nondirectory",
        |_ctx, args| super::fileio::builtin_file_name_nondirectory(args),
        0,
        None,
    );
    ctx.defsubr(
        "file-name-as-directory",
        |_ctx, args| super::fileio::builtin_file_name_as_directory(args),
        0,
        None,
    );
    ctx.defsubr(
        "directory-file-name",
        |_ctx, args| super::fileio::builtin_directory_file_name(args),
        0,
        None,
    );
    ctx.defsubr(
        "file-name-concat",
        |_ctx, args| super::fileio::builtin_file_name_concat(args),
        0,
        None,
    );
    ctx.defsubr(
        "file-name-absolute-p",
        |_ctx, args| super::fileio::builtin_file_name_absolute_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "directory-name-p",
        |_ctx, args| super::fileio::builtin_directory_name_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "substitute-in-file-name",
        |_ctx, args| super::fileio::builtin_substitute_in_file_name(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-file-acl",
        |_ctx, args| super::fileio::builtin_set_file_acl(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-file-selinux-context",
        |_ctx, args| super::fileio::builtin_set_file_selinux_context(args),
        0,
        None,
    );
    ctx.defsubr(
        "visited-file-modtime",
        |_ctx, args| super::fileio::builtin_visited_file_modtime(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-temp-name",
        |_ctx, args| super::fileio::builtin_make_temp_name(args),
        0,
        None,
    );
    ctx.defsubr(
        "next-read-file-uses-dialog-p",
        |_ctx, args| super::fileio::builtin_next_read_file_uses_dialog_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "unhandled-file-name-directory",
        |_ctx, args| super::fileio::builtin_unhandled_file_name_directory(args),
        0,
        None,
    );
    ctx.defsubr(
        "get-truename-buffer",
        |_ctx, args| super::fileio::builtin_get_truename_buffer(args),
        0,
        None,
    );
    ctx.defsubr(
        "single-key-description",
        |_ctx, args| builtin_single_key_description(args),
        0,
        None,
    );
    ctx.defsubr(
        "key-description",
        |_ctx, args| builtin_key_description(args),
        0,
        None,
    );
    ctx.defsubr(
        "event-convert-list",
        |_ctx, args| builtin_event_convert_list(args),
        0,
        None,
    );
    ctx.defsubr(
        "text-char-description",
        |_ctx, args| builtin_text_char_description(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-binary-mode",
        |_ctx, args| super::process::builtin_set_binary_mode(args),
        0,
        None,
    );
    ctx.defsubr(
        "group-name",
        |_ctx, args| super::editfns::builtin_group_name(args),
        0,
        None,
    );
    ctx.defsubr(
        "group-gid",
        |_ctx, args| super::editfns::builtin_group_gid(args),
        0,
        None,
    );
    ctx.defsubr(
        "group-real-gid",
        |_ctx, args| super::editfns::builtin_group_real_gid(args),
        0,
        None,
    );
    ctx.defsubr(
        "load-average",
        |_ctx, args| super::editfns::builtin_load_average(args),
        0,
        None,
    );
    ctx.defsubr(
        "logcount",
        |_ctx, args| super::editfns::builtin_logcount(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-frame-size-and-position-pixelwise",
        |_ctx, args| builtin_set_frame_size_and_position_pixelwise(args),
        0,
        None,
    );
    ctx.defsubr(
        "mouse-position-in-root-frame",
        |_ctx, args| builtin_mouse_position_in_root_frame(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-load-color-file",
        |_ctx, args| super::font::builtin_x_load_color_file(args),
        0,
        None,
    );
    ctx.defsubr(
        "define-fringe-bitmap",
        |_ctx, args| builtin_define_fringe_bitmap(args),
        0,
        None,
    );
    ctx.defsubr(
        "destroy-fringe-bitmap",
        |_ctx, args| builtin_destroy_fringe_bitmap(args),
        0,
        None,
    );
    ctx.defsubr(
        "display--line-is-continued-p",
        |_ctx, args| builtin_display_line_is_continued_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "display--update-for-mouse-movement",
        |_ctx, args| builtin_display_update_for_mouse_movement(args),
        0,
        None,
    );
    ctx.defsubr(
        "do-auto-save",
        |_ctx, args| builtin_do_auto_save(args),
        0,
        None,
    );
    ctx.defsubr(
        "external-debugging-output",
        |_ctx, args| builtin_external_debugging_output(args),
        0,
        None,
    );
    ctx.defsubr(
        "describe-buffer-bindings",
        |_ctx, args| builtin_describe_buffer_bindings(args),
        0,
        None,
    );
    ctx.defsubr(
        "describe-vector",
        |_ctx, args| builtin_describe_vector(args),
        0,
        None,
    );
    ctx.defsubr(
        "face-attributes-as-vector",
        |_ctx, args| builtin_face_attributes_as_vector(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-face-attributes",
        |_ctx, args| builtin_font_face_attributes(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-get-glyphs",
        |_ctx, args| builtin_font_get_glyphs(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-get-system-font",
        |_ctx, args| builtin_font_get_system_font(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-get-system-normal-font",
        |_ctx, args| builtin_font_get_system_normal_font(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-has-char-p",
        |_ctx, args| builtin_font_has_char_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-match-p",
        |_ctx, args| builtin_font_match_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-shape-gstring",
        |_ctx, args| builtin_font_shape_gstring(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-variation-glyphs",
        |_ctx, args| builtin_font_variation_glyphs(args),
        0,
        None,
    );
    ctx.defsubr(
        "fontset-font",
        |_ctx, args| builtin_fontset_font(args),
        0,
        None,
    );
    ctx.defsubr(
        "fontset-info",
        |_ctx, args| builtin_fontset_info(args),
        0,
        None,
    );
    ctx.defsubr(
        "fontset-list",
        |_ctx, args| builtin_fontset_list(args),
        0,
        None,
    );
    ctx.defsubr(
        "fontset-list-all",
        |_ctx, args| builtin_fontset_list_all(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame--set-was-invisible",
        |_ctx, args| builtin_frame_set_was_invisible(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-after-make-frame",
        |_ctx, args| builtin_frame_after_make_frame(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-ancestor-p",
        |_ctx, args| builtin_frame_ancestor_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-bottom-divider-width",
        |_ctx, args| builtin_frame_bottom_divider_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-child-frame-border-width",
        |_ctx, args| builtin_frame_child_frame_border_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-focus",
        |_ctx, args| builtin_frame_focus(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-font-cache",
        |_ctx, args| builtin_frame_font_cache(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-fringe-width",
        |_ctx, args| builtin_frame_fringe_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-internal-border-width",
        |_ctx, args| builtin_frame_internal_border_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-or-buffer-changed-p",
        |_ctx, args| builtin_frame_or_buffer_changed_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-parent",
        |_ctx, args| builtin_frame_parent(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-pointer-visible-p",
        |_ctx, args| builtin_frame_pointer_visible_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-right-divider-width",
        |_ctx, args| builtin_frame_right_divider_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-scale-factor",
        |_ctx, args| builtin_frame_scale_factor(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-scroll-bar-height",
        |_ctx, args| builtin_frame_scroll_bar_height(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-scroll-bar-width",
        |_ctx, args| builtin_frame_scroll_bar_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-window-state-change",
        |_ctx, args| builtin_frame_window_state_change(args),
        0,
        None,
    );
    ctx.defsubr(
        "fringe-bitmaps-at-pos",
        |_ctx, args| builtin_fringe_bitmaps_at_pos(args),
        0,
        None,
    );
    ctx.defsubr(
        "gap-position",
        |_ctx, args| builtin_gap_position(args),
        0,
        None,
    );
    ctx.defsubr("gap-size", |_ctx, args| builtin_gap_size(args), 0, None);
    ctx.defsubr(
        "garbage-collect-heapsize",
        |_ctx, args| builtin_garbage_collect_heapsize(args),
        0,
        None,
    );
    ctx.defsubr(
        "garbage-collect-maybe",
        |_ctx, args| builtin_garbage_collect_maybe(args),
        0,
        None,
    );
    ctx.defsubr(
        "get-unicode-property-internal",
        |_ctx, args| builtin_get_unicode_property_internal(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-available-p",
        |_ctx, args| builtin_gnutls_available_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-asynchronous-parameters",
        |_ctx, args| builtin_gnutls_asynchronous_parameters(args),
        0,
        None,
    );
    ctx.defsubr("gnutls-bye", |_ctx, args| builtin_gnutls_bye(args), 0, None);
    ctx.defsubr(
        "gnutls-ciphers",
        |_ctx, args| builtin_gnutls_ciphers(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-deinit",
        |_ctx, args| builtin_gnutls_deinit(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-digests",
        |_ctx, args| builtin_gnutls_digests(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-error-fatalp",
        |_ctx, args| builtin_gnutls_error_fatalp(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-error-string",
        |_ctx, args| builtin_gnutls_error_string(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-errorp",
        |_ctx, args| builtin_gnutls_errorp(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-format-certificate",
        |_ctx, args| builtin_gnutls_format_certificate(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-get-initstage",
        |_ctx, args| builtin_gnutls_get_initstage(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-hash-digest",
        |_ctx, args| builtin_gnutls_hash_digest(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-hash-mac",
        |_ctx, args| builtin_gnutls_hash_mac(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-macs",
        |_ctx, args| builtin_gnutls_macs(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-peer-status",
        |_ctx, args| builtin_gnutls_peer_status(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-peer-status-warning-describe",
        |_ctx, args| builtin_gnutls_peer_status_warning_describe(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-symmetric-decrypt",
        |_ctx, args| builtin_gnutls_symmetric_decrypt(args),
        0,
        None,
    );
    ctx.defsubr(
        "gnutls-symmetric-encrypt",
        |_ctx, args| builtin_gnutls_symmetric_encrypt(args),
        0,
        None,
    );
    ctx.defsubr(
        "gpm-mouse-start",
        |_ctx, args| builtin_gpm_mouse_start(args),
        0,
        None,
    );
    ctx.defsubr(
        "gpm-mouse-stop",
        |_ctx, args| builtin_gpm_mouse_stop(args),
        0,
        None,
    );
    ctx.defsubr(
        "handle-save-session",
        |_ctx, args| builtin_handle_save_session(args),
        0,
        None,
    );
    ctx.defsubr(
        "handle-switch-frame",
        |_ctx, args| builtin_handle_switch_frame(args),
        0,
        None,
    );
    ctx.defsubr(
        "help--describe-vector",
        |_ctx, args| builtin_help_describe_vector(args),
        0,
        None,
    );
    ctx.defsubr(
        "init-image-library",
        |_ctx, args| builtin_init_image_library(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal--obarray-buckets",
        |_ctx, args| builtin_internal_obarray_buckets(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal--set-buffer-modified-tick",
        |_ctx, args| builtin_internal_set_buffer_modified_tick(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal--track-mouse",
        |_ctx, args| builtin_internal_track_mouse(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-char-font",
        |_ctx, args| builtin_internal_char_font(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-complete-buffer",
        |_ctx, args| builtin_internal_complete_buffer(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-describe-syntax-value",
        |_ctx, args| builtin_internal_describe_syntax_value(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-event-symbol-parse-modifiers",
        |_ctx, args| builtin_internal_event_symbol_parse_modifiers(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-handle-focus-in",
        |_ctx, args| builtin_internal_handle_focus_in(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-set-lisp-face-attribute-from-resource",
        |_ctx, args| builtin_internal_set_lisp_face_attribute_from_resource(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-stack-stats",
        |_ctx, args| builtin_internal_stack_stats(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-subr-documentation",
        |_ctx, args| builtin_internal_subr_documentation(args),
        0,
        None,
    );
    ctx.defsubr("byte-code", |_ctx, args| builtin_byte_code(args), 0, None);
    ctx.defsubr(
        "decode-coding-region",
        |_ctx, args| builtin_decode_coding_region(args),
        0,
        None,
    );
    ctx.defsubr(
        "dump-emacs-portable",
        |_ctx, args| builtin_dump_emacs_portable(args),
        0,
        None,
    );
    ctx.defsubr(
        "dump-emacs-portable--sort-predicate",
        |_ctx, args| builtin_dump_emacs_portable_sort_predicate(args),
        0,
        None,
    );
    ctx.defsubr(
        "dump-emacs-portable--sort-predicate-copied",
        |_ctx, args| builtin_dump_emacs_portable_sort_predicate_copied(args),
        0,
        None,
    );
    ctx.defsubr(
        "emacs-repository-get-version",
        |_ctx, args| builtin_emacs_repository_get_version(args),
        0,
        None,
    );
    ctx.defsubr(
        "emacs-repository-get-branch",
        |_ctx, args| builtin_emacs_repository_get_branch(args),
        0,
        None,
    );
    ctx.defsubr(
        "emacs-repository-get-dirty",
        |_ctx, args| builtin_emacs_repository_get_dirty(args),
        0,
        None,
    );
    ctx.defsubr(
        "encode-coding-region",
        |_ctx, args| builtin_encode_coding_region(args),
        0,
        None,
    );
    ctx.defsubr(
        "find-operation-coding-system",
        |_ctx, args| builtin_find_operation_coding_system(args),
        0,
        None,
    );
    ctx.defsubr(
        "iso-charset",
        |_ctx, args| builtin_iso_charset(args),
        0,
        None,
    );
    ctx.defsubr(
        "keymap--get-keyelt",
        |_ctx, args| builtin_keymap_get_keyelt(args),
        0,
        None,
    );
    ctx.defsubr(
        "keymap-prompt",
        |_ctx, args| builtin_keymap_prompt(args),
        0,
        None,
    );
    ctx.defsubr(
        "lower-frame",
        |_ctx, args| builtin_lower_frame(args),
        0,
        None,
    );
    ctx.defsubr(
        "lread--substitute-object-in-subtree",
        |_ctx, args| builtin_lread_substitute_object_in_subtree(args),
        0,
        None,
    );
    ctx.defsubr(
        "malloc-info",
        |_ctx, args| builtin_malloc_info(args),
        0,
        None,
    );
    ctx.defsubr(
        "malloc-trim",
        |_ctx, args| builtin_malloc_trim(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-byte-code",
        |_ctx, args| builtin_make_byte_code(args),
        0,
        None,
    );
    ctx.defsubr("make-char", |_ctx, args| builtin_make_char(args), 0, None);
    ctx.defsubr(
        "make-closure",
        |_ctx, args| builtin_make_closure(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-finalizer",
        |_ctx, args| builtin_make_finalizer(args),
        0,
        None,
    );
    ctx.defsubr(
        "marker-last-position",
        |_ctx, args| builtin_marker_last_position(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-interpreted-closure",
        |_ctx, args| builtin_make_interpreted_closure(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-record",
        |_ctx, args| builtin_make_record(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-temp-file-internal",
        |_ctx, args| builtin_make_temp_file_internal(args),
        0,
        None,
    );
    ctx.defsubr(
        "map-charset-chars",
        |_ctx, args| builtin_map_charset_chars(args),
        0,
        None,
    );
    ctx.defsubr(
        "mapbacktrace",
        |_ctx, args| builtin_mapbacktrace(args),
        0,
        None,
    );
    ctx.defsubr(
        "memory-info",
        |_ctx, args| builtin_memory_info(args),
        0,
        None,
    );
    ctx.defsubr(
        "make-frame-invisible",
        |_ctx, args| builtin_make_frame_invisible(args),
        0,
        None,
    );
    ctx.defsubr(
        "menu-bar-menu-at-x-y",
        |_ctx, args| builtin_menu_bar_menu_at_x_y(args),
        0,
        None,
    );
    ctx.defsubr(
        "menu-or-popup-active-p",
        |_ctx, args| builtin_menu_or_popup_active_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "module-load",
        |_ctx, args| builtin_module_load(args),
        0,
        None,
    );
    ctx.defsubr(
        "newline-cache-check",
        |_ctx, args| builtin_newline_cache_check(args),
        0,
        None,
    );
    ctx.defsubr(
        "native-comp-available-p",
        |_ctx, args| builtin_native_comp_available_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "native-comp-unit-file",
        |_ctx, args| builtin_native_comp_unit_file(args),
        0,
        None,
    );
    ctx.defsubr(
        "native-comp-unit-set-file",
        |_ctx, args| builtin_native_comp_unit_set_file(args),
        0,
        None,
    );
    ctx.defsubr(
        "native-elisp-load",
        |_ctx, args| builtin_native_elisp_load(args),
        0,
        None,
    );
    ctx.defsubr(
        "obarray-clear",
        |_ctx, args| builtin_obarray_clear(args),
        0,
        None,
    );
    ctx.defsubr(
        "obarray-make",
        |_ctx, args| builtin_obarray_make(args),
        0,
        None,
    );
    ctx.defsubr(
        "object-intervals",
        |_ctx, args| builtin_object_intervals(args),
        0,
        None,
    );
    ctx.defsubr(
        "open-dribble-file",
        |_ctx, args| builtin_open_dribble_file(args),
        0,
        None,
    );
    ctx.defsubr("open-font", |_ctx, args| builtin_open_font(args), 0, None);
    ctx.defsubr(
        "optimize-char-table",
        |_ctx, args| builtin_optimize_char_table(args),
        0,
        None,
    );
    ctx.defsubr(
        "overlay-lists",
        |_ctx, args| builtin_overlay_lists(args),
        0,
        None,
    );
    ctx.defsubr(
        "overlay-recenter",
        |_ctx, args| builtin_overlay_recenter(args),
        0,
        None,
    );
    ctx.defsubr(
        "pdumper-stats",
        |_ctx, args| builtin_pdumper_stats(args),
        0,
        None,
    );
    ctx.defsubr(
        "play-sound-internal",
        |_ctx, args| builtin_play_sound_internal(args),
        0,
        None,
    );
    ctx.defsubr(
        "position-symbol",
        |_ctx, args| builtin_position_symbol(args),
        0,
        None,
    );
    ctx.defsubr(
        "profiler-cpu-log",
        |_ctx, args| builtin_profiler_cpu_log(args),
        0,
        None,
    );
    ctx.defsubr(
        "profiler-cpu-running-p",
        |_ctx, args| builtin_profiler_cpu_running_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "profiler-cpu-start",
        |_ctx, args| builtin_profiler_cpu_start(args),
        0,
        None,
    );
    ctx.defsubr(
        "profiler-cpu-stop",
        |_ctx, args| builtin_profiler_cpu_stop(args),
        0,
        None,
    );
    ctx.defsubr(
        "profiler-memory-log",
        |_ctx, args| builtin_profiler_memory_log(args),
        0,
        None,
    );
    ctx.defsubr(
        "profiler-memory-running-p",
        |_ctx, args| builtin_profiler_memory_running_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "profiler-memory-start",
        |_ctx, args| builtin_profiler_memory_start(args),
        0,
        None,
    );
    ctx.defsubr(
        "profiler-memory-stop",
        |_ctx, args| builtin_profiler_memory_stop(args),
        0,
        None,
    );
    ctx.defsubr(
        "put-unicode-property-internal",
        |_ctx, args| builtin_put_unicode_property_internal(args),
        0,
        None,
    );
    ctx.defsubr("query-font", |_ctx, args| builtin_query_font(args), 0, None);
    ctx.defsubr(
        "query-fontset",
        |_ctx, args| builtin_query_fontset(args),
        0,
        None,
    );
    ctx.defsubr(
        "raise-frame",
        |_ctx, args| builtin_raise_frame(args),
        0,
        None,
    );
    ctx.defsubr(
        "read-positioning-symbols",
        |_ctx, args| builtin_read_positioning_symbols(args),
        0,
        None,
    );
    ctx.defsubr(
        "re--describe-compiled",
        |_ctx, args| builtin_re_describe_compiled(args),
        0,
        None,
    );
    ctx.defsubr(
        "recent-auto-save-p",
        |_ctx, args| builtin_recent_auto_save_p(args),
        0,
        None,
    );
    ctx.defsubr("redisplay", |_ctx, args| builtin_redisplay(args), 0, None);
    ctx.defsubr("record", |_ctx, args| builtin_record(args), 0, None);
    ctx.defsubr("recordp", |_ctx, args| builtin_recordp(args), 0, None);
    ctx.defsubr(
        "reconsider-frame-fonts",
        |_ctx, args| builtin_reconsider_frame_fonts(args),
        0,
        None,
    );
    ctx.defsubr(
        "redirect-debugging-output",
        |_ctx, args| builtin_redirect_debugging_output(args),
        0,
        None,
    );
    ctx.defsubr(
        "redirect-frame-focus",
        |_ctx, args| builtin_redirect_frame_focus(args),
        0,
        None,
    );
    ctx.defsubr(
        "remove-pos-from-symbol",
        |_ctx, args| builtin_remove_pos_from_symbol(args),
        0,
        None,
    );
    ctx.defsubr(
        "resize-mini-window-internal",
        |_ctx, args| builtin_resize_mini_window_internal(args),
        0,
        None,
    );
    ctx.defsubr(
        "restore-buffer-modified-p",
        |_ctx, args| builtin_restore_buffer_modified_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "set--this-command-keys",
        |_ctx, args| builtin_set_this_command_keys(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-buffer-auto-saved",
        |_ctx, args| builtin_set_buffer_auto_saved(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-buffer-major-mode",
        |_ctx, args| builtin_set_buffer_major_mode(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-buffer-redisplay",
        |_ctx, args| builtin_set_buffer_redisplay(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-charset-plist",
        |_ctx, args| builtin_set_charset_plist(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-frame-window-state-change",
        |_ctx, args| builtin_set_frame_window_state_change(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-fringe-bitmap-face",
        |_ctx, args| builtin_set_fringe_bitmap_face(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-minibuffer-window",
        |_ctx, args| builtin_set_minibuffer_window(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-mouse-pixel-position",
        |_ctx, args| builtin_set_mouse_pixel_position(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-mouse-position",
        |_ctx, args| builtin_set_mouse_position(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-window-new-normal",
        |_ctx, args| builtin_set_window_new_normal(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-window-new-pixel",
        |_ctx, args| builtin_set_window_new_pixel(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-window-new-total",
        |_ctx, args| builtin_set_window_new_total(args),
        0,
        None,
    );
    ctx.defsubr(
        "sort-charsets",
        |_ctx, args| builtin_sort_charsets(args),
        0,
        None,
    );
    ctx.defsubr("split-char", |_ctx, args| builtin_split_char(args), 0, None);
    ctx.defsubr(
        "string-distance",
        |_ctx, args| builtin_string_distance(args),
        0,
        None,
    );
    ctx.defsubr(
        "subr-native-comp-unit",
        |_ctx, args| builtin_subr_native_comp_unit(args),
        0,
        None,
    );
    ctx.defsubr(
        "subr-native-lambda-list",
        |_ctx, args| builtin_subr_native_lambda_list(args),
        0,
        None,
    );
    ctx.defsubr("subr-type", |_ctx, args| builtin_subr_type(args), 0, None);
    ctx.defsubr(
        "suspend-emacs",
        |_ctx, args| builtin_suspend_emacs(args),
        0,
        None,
    );
    ctx.defsubr(
        "thread--blocker",
        |_ctx, args| builtin_thread_blocker(args),
        0,
        None,
    );
    ctx.defsubr(
        "tool-bar-get-system-style",
        |_ctx, args| builtin_tool_bar_get_system_style(args),
        0,
        None,
    );
    ctx.defsubr(
        "tool-bar-pixel-width",
        |_ctx, args| builtin_tool_bar_pixel_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "translate-region-internal",
        |_ctx, args| builtin_translate_region_internal(args),
        0,
        None,
    );
    ctx.defsubr(
        "transpose-regions",
        |_ctx, args| builtin_transpose_regions(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty--output-buffer-size",
        |_ctx, args| builtin_tty_output_buffer_size(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty--set-output-buffer-size",
        |_ctx, args| builtin_tty_set_output_buffer_size(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-display-pixel-height",
        |_ctx, args| builtin_tty_display_pixel_height(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-display-pixel-width",
        |_ctx, args| builtin_tty_display_pixel_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-frame-at",
        |_ctx, args| builtin_tty_frame_at(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-frame-edges",
        |_ctx, args| builtin_tty_frame_edges(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-frame-geometry",
        |_ctx, args| builtin_tty_frame_geometry(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-frame-list-z-order",
        |_ctx, args| builtin_tty_frame_list_z_order(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-frame-restack",
        |_ctx, args| builtin_tty_frame_restack(args),
        0,
        None,
    );
    ctx.defsubr(
        "tty-suppress-bold-inverse-default-colors",
        |_ctx, args| builtin_tty_suppress_bold_inverse_default_colors(args),
        0,
        None,
    );
    ctx.defsubr(
        "unencodable-char-position",
        |_ctx, args| builtin_unencodable_char_position(args),
        0,
        None,
    );
    ctx.defsubr(
        "unicode-property-table-internal",
        |_ctx, args| builtin_unicode_property_table_internal(args),
        0,
        None,
    );
    ctx.defsubr(
        "unify-charset",
        |_ctx, args| builtin_unify_charset(args),
        0,
        None,
    );
    ctx.defsubr("unix-sync", |_ctx, args| builtin_unix_sync(args), 0, None);
    ctx.defsubr("value<", |_ctx, args| builtin_value_lt(args), 0, None);
    ctx.defsubr(
        "x-begin-drag",
        |_ctx, args| builtin_x_begin_drag(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-double-buffered-p",
        |_ctx, args| builtin_x_double_buffered_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-menu-bar-open-internal",
        |_ctx, args| builtin_x_menu_bar_open_internal(args),
        0,
        None,
    );
    ctx.defsubr(
        "xw-color-defined-p",
        |_ctx, args| builtin_xw_color_defined_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "xw-color-values",
        |_ctx, args| builtin_xw_color_values(args),
        0,
        None,
    );
    ctx.defsubr(
        "xw-display-color-p",
        |_ctx, args| builtin_xw_display_color_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "inotify-add-watch",
        |_ctx, args| builtin_inotify_add_watch(args),
        0,
        None,
    );
    ctx.defsubr(
        "inotify-allocated-p",
        |_ctx, args| builtin_inotify_allocated_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "inotify-rm-watch",
        |_ctx, args| builtin_inotify_rm_watch(args),
        0,
        None,
    );
    ctx.defsubr(
        "inotify-valid-p",
        |_ctx, args| builtin_inotify_valid_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "inotify-watch-list",
        |_ctx, args| builtin_inotify_watch_list(args),
        0,
        None,
    );
    ctx.defsubr(
        "lock-buffer",
        |_ctx, args| builtin_lock_buffer(args),
        0,
        None,
    );
    ctx.defsubr("lock-file", |_ctx, args| builtin_lock_file(args), 0, None);
    ctx.defsubr(
        "lossage-size",
        |_ctx, args| builtin_lossage_size(args),
        0,
        None,
    );
    ctx.defsubr(
        "unlock-buffer",
        |_ctx, args| builtin_unlock_buffer(args),
        0,
        None,
    );
    ctx.defsubr(
        "unlock-file",
        |_ctx, args| builtin_unlock_file(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-bottom-divider-width",
        |_ctx, args| builtin_window_bottom_divider_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-lines-pixel-dimensions",
        |_ctx, args| builtin_window_lines_pixel_dimensions(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-new-normal",
        |_ctx, args| builtin_window_new_normal(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-new-pixel",
        |_ctx, args| builtin_window_new_pixel(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-new-total",
        |_ctx, args| builtin_window_new_total(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-old-body-pixel-height",
        |_ctx, args| builtin_window_old_body_pixel_height(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-old-body-pixel-width",
        |_ctx, args| builtin_window_old_body_pixel_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-old-pixel-height",
        |_ctx, args| builtin_window_old_pixel_height(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-old-pixel-width",
        |_ctx, args| builtin_window_old_pixel_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-right-divider-width",
        |_ctx, args| builtin_window_right_divider_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-scroll-bar-height",
        |_ctx, args| builtin_window_scroll_bar_height(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-scroll-bar-width",
        |_ctx, args| builtin_window_scroll_bar_width(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-available-p",
        |_ctx, args| builtin_treesit_available_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-compiled-query-p",
        |_ctx, args| builtin_treesit_compiled_query_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-induce-sparse-tree",
        |_ctx, args| builtin_treesit_induce_sparse_tree(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-language-abi-version",
        |_ctx, args| builtin_treesit_language_abi_version(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-language-available-p",
        |_ctx, args| builtin_treesit_language_available_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-library-abi-version",
        |_ctx, args| builtin_treesit_library_abi_version(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-check",
        |_ctx, args| builtin_treesit_node_check(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-child",
        |_ctx, args| builtin_treesit_node_child(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-child-by-field-name",
        |_ctx, args| builtin_treesit_node_child_by_field_name(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-child-count",
        |_ctx, args| builtin_treesit_node_child_count(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-descendant-for-range",
        |_ctx, args| builtin_treesit_node_descendant_for_range(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-end",
        |_ctx, args| builtin_treesit_node_end(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-eq",
        |_ctx, args| builtin_treesit_node_eq(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-field-name-for-child",
        |_ctx, args| builtin_treesit_node_field_name_for_child(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-first-child-for-pos",
        |_ctx, args| builtin_treesit_node_first_child_for_pos(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-match-p",
        |_ctx, args| builtin_treesit_node_match_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-next-sibling",
        |_ctx, args| builtin_treesit_node_next_sibling(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-p",
        |_ctx, args| builtin_treesit_node_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-parent",
        |_ctx, args| builtin_treesit_node_parent(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-parser",
        |_ctx, args| builtin_treesit_node_parser(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-prev-sibling",
        |_ctx, args| builtin_treesit_node_prev_sibling(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-start",
        |_ctx, args| builtin_treesit_node_start(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-string",
        |_ctx, args| builtin_treesit_node_string(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-node-type",
        |_ctx, args| builtin_treesit_node_type(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-add-notifier",
        |_ctx, args| builtin_treesit_parser_add_notifier(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-buffer",
        |_ctx, args| builtin_treesit_parser_buffer(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-create",
        |_ctx, args| builtin_treesit_parser_create(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-delete",
        |_ctx, args| builtin_treesit_parser_delete(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-included-ranges",
        |_ctx, args| builtin_treesit_parser_included_ranges(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-language",
        |_ctx, args| builtin_treesit_parser_language(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-list",
        |_ctx, args| builtin_treesit_parser_list(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-notifiers",
        |_ctx, args| builtin_treesit_parser_notifiers(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-p",
        |_ctx, args| builtin_treesit_parser_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-remove-notifier",
        |_ctx, args| builtin_treesit_parser_remove_notifier(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-root-node",
        |_ctx, args| builtin_treesit_parser_root_node(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-set-included-ranges",
        |_ctx, args| builtin_treesit_parser_set_included_ranges(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-tag",
        |_ctx, args| builtin_treesit_parser_tag(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-pattern-expand",
        |_ctx, args| builtin_treesit_pattern_expand(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-query-capture",
        |_ctx, args| builtin_treesit_query_capture(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-query-compile",
        |_ctx, args| builtin_treesit_query_compile(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-query-expand",
        |_ctx, args| builtin_treesit_query_expand(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-query-language",
        |_ctx, args| builtin_treesit_query_language(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-query-p",
        |_ctx, args| builtin_treesit_query_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-search-forward",
        |_ctx, args| builtin_treesit_search_forward(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-search-subtree",
        |_ctx, args| builtin_treesit_search_subtree(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-subtree-stat",
        |_ctx, args| builtin_treesit_subtree_stat(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-grammar-location",
        |_ctx, args| builtin_treesit_grammar_location(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-tracking-line-column-p",
        |_ctx, args| builtin_treesit_tracking_line_column_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-tracking-line-column-p",
        |_ctx, args| builtin_treesit_parser_tracking_line_column_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-query-eagerly-compiled-p",
        |_ctx, args| builtin_treesit_query_eagerly_compiled_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-query-source",
        |_ctx, args| builtin_treesit_query_source(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-embed-level",
        |_ctx, args| builtin_treesit_parser_embed_level(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-set-embed-level",
        |_ctx, args| builtin_treesit_parser_set_embed_level(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parse-string",
        |_ctx, args| builtin_treesit_parse_string(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit-parser-changed-regions",
        |_ctx, args| builtin_treesit_parser_changed_regions(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit--linecol-at",
        |_ctx, args| builtin_treesit_linecol_at(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit--linecol-cache-set",
        |_ctx, args| builtin_treesit_linecol_cache_set(args),
        0,
        None,
    );
    ctx.defsubr(
        "treesit--linecol-cache",
        |_ctx, args| builtin_treesit_linecol_cache(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-available-p",
        |_ctx, args| builtin_sqlite_available_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-close",
        |_ctx, args| builtin_sqlite_close(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-columns",
        |_ctx, args| builtin_sqlite_columns(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-commit",
        |_ctx, args| builtin_sqlite_commit(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-execute",
        |_ctx, args| builtin_sqlite_execute(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-execute-batch",
        |_ctx, args| builtin_sqlite_execute_batch(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-finalize",
        |_ctx, args| builtin_sqlite_finalize(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-load-extension",
        |_ctx, args| builtin_sqlite_load_extension(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-more-p",
        |_ctx, args| builtin_sqlite_more_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-next",
        |_ctx, args| builtin_sqlite_next(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-open",
        |_ctx, args| builtin_sqlite_open(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-pragma",
        |_ctx, args| builtin_sqlite_pragma(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-rollback",
        |_ctx, args| builtin_sqlite_rollback(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-select",
        |_ctx, args| builtin_sqlite_select(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-transaction",
        |_ctx, args| builtin_sqlite_transaction(args),
        0,
        None,
    );
    ctx.defsubr(
        "sqlite-version",
        |_ctx, args| builtin_sqlite_version(args),
        0,
        None,
    );
    ctx.defsubr("sqlitep", |_ctx, args| builtin_sqlitep(args), 0, None);
    ctx.defsubr("fillarray", |_ctx, args| builtin_fillarray(args), 0, None);
    ctx.defsubr(
        "define-hash-table-test",
        |_ctx, args| builtin_define_hash_table_test(args),
        0,
        None,
    );
    ctx.defsubr(
        "hash-table-test",
        |_ctx, args| super::hashtab::builtin_hash_table_test(args),
        0,
        None,
    );
    ctx.defsubr(
        "hash-table-size",
        |_ctx, args| super::hashtab::builtin_hash_table_size(args),
        0,
        None,
    );
    ctx.defsubr(
        "hash-table-rehash-size",
        |_ctx, args| super::hashtab::builtin_hash_table_rehash_size(args),
        0,
        None,
    );
    ctx.defsubr(
        "hash-table-rehash-threshold",
        |_ctx, args| super::hashtab::builtin_hash_table_rehash_threshold(args),
        0,
        None,
    );
    ctx.defsubr(
        "hash-table-weakness",
        |_ctx, args| super::hashtab::builtin_hash_table_weakness(args),
        0,
        None,
    );
    ctx.defsubr(
        "copy-hash-table",
        |_ctx, args| super::hashtab::builtin_copy_hash_table(args),
        0,
        None,
    );
    ctx.defsubr(
        "sxhash-eq",
        |_ctx, args| super::hashtab::builtin_sxhash_eq(args),
        0,
        None,
    );
    ctx.defsubr(
        "sxhash-eql",
        |_ctx, args| super::hashtab::builtin_sxhash_eql(args),
        0,
        None,
    );
    ctx.defsubr(
        "sxhash-equal",
        |_ctx, args| super::hashtab::builtin_sxhash_equal(args),
        0,
        None,
    );
    ctx.defsubr(
        "sxhash-equal-including-properties",
        |_ctx, args| super::hashtab::builtin_sxhash_equal_including_properties(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal--hash-table-buckets",
        |_ctx, args| super::hashtab::builtin_internal_hash_table_buckets(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal--hash-table-histogram",
        |_ctx, args| super::hashtab::builtin_internal_hash_table_histogram(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal--hash-table-index-size",
        |_ctx, args| super::hashtab::builtin_internal_hash_table_index_size(args),
        0,
        None,
    );
    ctx.defsubr(
        "debug-timer-check",
        |_ctx, args| builtin_debug_timer_check(args),
        0,
        None,
    );
    ctx.defsubr(
        "dbus-close-inhibitor-lock",
        |_ctx, args| builtin_dbus_close_inhibitor_lock(args),
        0,
        None,
    );
    ctx.defsubr(
        "dbus-make-inhibitor-lock",
        |_ctx, args| builtin_dbus_make_inhibitor_lock(args),
        0,
        None,
    );
    ctx.defsubr(
        "dbus-registered-inhibitor-locks",
        |_ctx, args| builtin_dbus_registered_inhibitor_locks(args),
        0,
        None,
    );
    ctx.defsubr(
        "lcms2-available-p",
        |_ctx, args| builtin_lcms2_available_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "lcms-cie-de2000",
        |_ctx, args| builtin_lcms_cie_de2000(args),
        0,
        None,
    );
    ctx.defsubr(
        "lcms-xyz->jch",
        |_ctx, args| builtin_lcms_xyz_to_jch(args),
        0,
        None,
    );
    ctx.defsubr(
        "lcms-jch->xyz",
        |_ctx, args| builtin_lcms_jch_to_xyz(args),
        0,
        None,
    );
    ctx.defsubr(
        "lcms-jch->jab",
        |_ctx, args| builtin_lcms_jch_to_jab(args),
        0,
        None,
    );
    ctx.defsubr(
        "lcms-jab->jch",
        |_ctx, args| builtin_lcms_jab_to_jch(args),
        0,
        None,
    );
    ctx.defsubr(
        "lcms-cam02-ucs",
        |_ctx, args| builtin_lcms_cam02_ucs(args),
        0,
        None,
    );
    ctx.defsubr(
        "lcms-temp->white-point",
        |_ctx, args| builtin_lcms_temp_to_white_point(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-frame-geometry",
        |_ctx, args| builtin_neomacs_frame_geometry(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-frame-edges",
        |_ctx, args| builtin_neomacs_frame_edges(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-mouse-absolute-pixel-position",
        |_ctx, args| builtin_neomacs_mouse_absolute_pixel_position(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-set-mouse-absolute-pixel-position",
        |_ctx, args| builtin_neomacs_set_mouse_absolute_pixel_position(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-display-monitor-attributes-list",
        |_ctx, args| builtin_neomacs_display_monitor_attributes_list(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-scroll-bar-foreground",
        |_ctx, args| builtin_x_scroll_bar_foreground(args),
        0,
        None,
    );
    ctx.defsubr(
        "x-scroll-bar-background",
        |_ctx, args| builtin_x_scroll_bar_background(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-clipboard-set",
        |_ctx, args| builtin_neomacs_clipboard_set(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-clipboard-get",
        |_ctx, args| builtin_neomacs_clipboard_get(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-primary-selection-set",
        |_ctx, args| builtin_neomacs_primary_selection_set(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-primary-selection-get",
        |_ctx, args| builtin_neomacs_primary_selection_get(args),
        0,
        None,
    );
    ctx.defsubr(
        "neomacs-core-backend",
        |_ctx, args| builtin_neomacs_core_backend(args),
        0,
        None,
    );
    ctx.defsubr(
        "buffer-local-toplevel-value",
        |_ctx, args| builtin_buffer_local_toplevel_value(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-buffer-local-toplevel-value",
        |_ctx, args| builtin_set_buffer_local_toplevel_value(args),
        0,
        None,
    );
    ctx.defsubr(
        "debugger-trap",
        |_ctx, args| builtin_debugger_trap(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-delete-indirect-variable",
        |_ctx, args| builtin_internal_delete_indirect_variable(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-decode-string-utf-8",
        |_ctx, args| builtin_internal_decode_string_utf_8(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal-encode-string-utf-8",
        |_ctx, args| builtin_internal_encode_string_utf_8(args),
        0,
        None,
    );
    ctx.defsubr(
        "overlay-tree",
        |_ctx, args| builtin_overlay_tree(args),
        0,
        None,
    );
    ctx.defsubr(
        "thread-buffer-disposition",
        |_ctx, args| builtin_thread_buffer_disposition(args),
        0,
        None,
    );
    ctx.defsubr(
        "thread-set-buffer-disposition",
        |_ctx, args| builtin_thread_set_buffer_disposition(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-discard-buffer-from-window",
        |_ctx, args| builtin_window_discard_buffer_from_window(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-cursor-info",
        |_ctx, args| builtin_window_cursor_info(args),
        0,
        None,
    );
    ctx.defsubr(
        "combine-windows",
        |_ctx, args| builtin_combine_windows(args),
        0,
        None,
    );
    ctx.defsubr(
        "uncombine-window",
        |_ctx, args| builtin_uncombine_window(args),
        0,
        None,
    );
    ctx.defsubr(
        "frame-windows-min-size",
        |_ctx, args| builtin_frame_windows_min_size(args),
        0,
        None,
    );
    ctx.defsubr(
        "remember-mouse-glyph",
        |_ctx, args| builtin_remember_mouse_glyph(args),
        0,
        None,
    );
    ctx.defsubr(
        "lookup-image",
        |_ctx, args| builtin_lookup_image(args),
        0,
        None,
    );
    ctx.defsubr(
        "imagemagick-types",
        |_ctx, args| builtin_imagemagick_types(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-drive-otf",
        |_ctx, args| builtin_font_drive_otf(args),
        0,
        None,
    );
    ctx.defsubr(
        "font-otf-alternates",
        |_ctx, args| builtin_font_otf_alternates(args),
        0,
        None,
    );
    ctx.defsubr("obarrayp", |_ctx, args| builtin_obarrayp(args), 0, None);
    ctx.defsubr("ntake", |_ctx, args| builtin_ntake(args), 0, None);
    ctx.defsubr(
        "default-file-modes",
        |_ctx, args| super::fileio::builtin_default_file_modes(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-default-file-modes",
        |_ctx, args| super::fileio::builtin_set_default_file_modes(args),
        0,
        None,
    );
    ctx.defsubr(
        "cancel-kbd-macro-events",
        |_ctx, args| builtin_cancel_kbd_macro_events(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-configuration-p",
        |_ctx, args| builtin_window_configuration_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-configuration-frame",
        |_ctx, args| builtin_window_configuration_frame(args),
        0,
        None,
    );
    ctx.defsubr(
        "window-configuration-equal-p",
        |_ctx, args| builtin_window_configuration_equal_p(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-input-meta-mode",
        |_ctx, args| super::reader::builtin_set_input_meta_mode(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-output-flow-control",
        |_ctx, args| super::reader::builtin_set_output_flow_control(args),
        0,
        None,
    );
    ctx.defsubr(
        "set-quit-char",
        |_ctx, args| super::reader::builtin_set_quit_char(args),
        0,
        None,
    );
    ctx.defsubr(
        "top-level",
        |_ctx, args| super::minibuffer::builtin_top_level(args),
        0,
        None,
    );
    ctx.defsubr(
        "documentation-stringp",
        |_ctx, args| builtin_documentation_stringp(args),
        0,
        None,
    );
    ctx.defsubr(
        "internal--define-uninitialized-variable",
        builtin_internal_define_uninitialized_variable_eval,
        0,
        None,
    );
    ctx.defsubr(
        "compose-region-internal",
        super::composite::builtin_compose_region_internal_eval,
        0,
        None,
    );
    ctx.defsubr(
        "window-text-pixel-size",
        super::xdisp::builtin_window_text_pixel_size_eval,
        0,
        None,
    );
    ctx.defsubr(
        "pos-visible-in-window-p",
        super::xdisp::builtin_pos_visible_in_window_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "frame--face-hash-table",
        super::xfaces::builtin_frame_face_hash_table_eval,
        0,
        None,
    );
    ctx.defsubr(
        "delete-directory-internal",
        super::fileio::builtin_delete_directory_internal_eval,
        0,
        None,
    );
    ctx.defsubr(
        "make-directory-internal",
        super::fileio::builtin_make_directory_internal_eval,
        0,
        None,
    );
    ctx.defsubr(
        "directory-files-and-attributes",
        super::dired::builtin_directory_files_and_attributes_eval,
        0,
        None,
    );
    ctx.defsubr(
        "find-file-name-handler",
        super::fileio::builtin_find_file_name_handler_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-name-all-completions",
        super::dired::builtin_file_name_all_completions_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-accessible-directory-p",
        super::fileio::builtin_file_accessible_directory_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-name-case-insensitive-p",
        super::fileio::builtin_file_name_case_insensitive_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "file-newer-than-file-p",
        super::fileio::builtin_file_newer_than_file_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "verify-visited-file-modtime",
        super::fileio::builtin_verify_visited_file_modtime,
        0,
        None,
    );
    ctx.defsubr(
        "internal-default-interrupt-process",
        super::process::builtin_internal_default_interrupt_process,
        0,
        None,
    );
    ctx.defsubr(
        "internal-default-process-filter",
        super::process::builtin_internal_default_process_filter,
        0,
        None,
    );
    ctx.defsubr(
        "internal-default-process-sentinel",
        super::process::builtin_internal_default_process_sentinel,
        0,
        None,
    );
    ctx.defsubr(
        "internal-default-signal-process",
        super::process::builtin_internal_default_signal_process,
        0,
        None,
    );
    ctx.defsubr(
        "network-lookup-address-info",
        super::process::builtin_network_lookup_address_info,
        0,
        None,
    );
    ctx.defsubr(
        "set-network-process-option",
        super::process::builtin_set_network_process_option,
        0,
        None,
    );
    ctx.defsubr(
        "process-query-on-exit-flag",
        super::process::builtin_process_query_on_exit_flag,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-query-on-exit-flag",
        super::process::builtin_set_process_query_on_exit_flag,
        0,
        None,
    );
    ctx.defsubr(
        "process-inherit-coding-system-flag",
        super::process::builtin_process_inherit_coding_system_flag,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-coding-system",
        super::process::builtin_set_process_coding_system,
        0,
        None,
    );
    ctx.defsubr(
        "set-process-datagram-address",
        super::process::builtin_set_process_datagram_address,
        0,
        None,
    );
    ctx.defsubr(
        "remove-list-of-text-properties",
        super::textprop::builtin_remove_list_of_text_properties,
        0,
        None,
    );
    ctx.defsubr(
        "get-char-property-and-overlay",
        super::textprop::builtin_get_char_property_and_overlay,
        0,
        None,
    );
    ctx.defsubr(
        "next-single-property-change",
        super::textprop::builtin_next_single_property_change,
        0,
        None,
    );
    ctx.defsubr(
        "previous-single-property-change",
        super::textprop::builtin_previous_single_property_change,
        0,
        None,
    );
    ctx.defsubr(
        "line-beginning-position",
        super::navigation::builtin_line_beginning_position,
        0,
        None,
    );
    ctx.defsubr(
        "make-variable-buffer-local",
        super::custom::builtin_make_variable_buffer_local,
        0,
        None,
    );
    ctx.defsubr(
        "active-minibuffer-window",
        super::window_cmds::builtin_active_minibuffer_window_eval,
        0,
        None,
    );
    ctx.defsubr(
        "minibuffer-selected-window",
        super::window_cmds::builtin_minibuffer_selected_window,
        0,
        None,
    );
    ctx.defsubr(
        "window-mode-line-height",
        super::window_cmds::builtin_window_mode_line_height,
        0,
        None,
    );
    ctx.defsubr(
        "window-header-line-height",
        super::window_cmds::builtin_window_header_line_height,
        0,
        None,
    );
    ctx.defsubr(
        "window-tab-line-height",
        super::window_cmds::builtin_window_tab_line_height,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-display-table",
        super::window_cmds::builtin_set_window_display_table,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-cursor-type",
        super::window_cmds::builtin_set_window_cursor_type,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-scroll-bars",
        super::window_cmds::builtin_set_window_scroll_bars,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-next-buffers",
        super::window_cmds::builtin_set_window_next_buffers,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-prev-buffers",
        super::window_cmds::builtin_set_window_prev_buffers,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-dedicated-p",
        super::window_cmds::builtin_set_window_dedicated_p,
        0,
        None,
    );
    ctx.defsubr(
        "delete-window-internal",
        super::window_cmds::builtin_delete_window_internal,
        0,
        None,
    );
    ctx.defsubr(
        "delete-other-windows-internal",
        super::window_cmds::builtin_delete_other_windows_internal,
        0,
        None,
    );
    ctx.defsubr(
        "window-combination-limit",
        super::window_cmds::builtin_window_combination_limit,
        0,
        None,
    );
    ctx.defsubr(
        "set-window-combination-limit",
        super::window_cmds::builtin_set_window_combination_limit,
        0,
        None,
    );
    ctx.defsubr(
        "window-resize-apply-total",
        super::window_cmds::builtin_window_resize_apply_total,
        0,
        None,
    );
    ctx.defsubr(
        "other-window-for-scrolling",
        super::window_cmds::builtin_other_window_for_scrolling,
        0,
        None,
    );
    ctx.defsubr(
        "select-frame-set-input-focus",
        super::window_cmds::builtin_select_frame_set_input_focus,
        0,
        None,
    );
    ctx.defsubr(
        "modify-frame-parameters",
        super::window_cmds::builtin_modify_frame_parameters,
        0,
        None,
    );
    ctx.defsubr(
        "frame-selected-window",
        super::window_cmds::builtin_frame_selected_window,
        0,
        None,
    );
    ctx.defsubr(
        "frame-old-selected-window",
        super::window_cmds::builtin_frame_old_selected_window,
        0,
        None,
    );
    ctx.defsubr(
        "set-frame-selected-window",
        super::window_cmds::builtin_set_frame_selected_window,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-pixel-width",
        super::display::builtin_x_display_pixel_width_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-pixel-height",
        super::display::builtin_x_display_pixel_height_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-server-max-request-size",
        super::display::builtin_x_server_max_request_size_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-grayscale-p",
        super::display::builtin_x_display_grayscale_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-backing-store",
        super::display::builtin_x_display_backing_store_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-color-cells",
        super::display::builtin_x_display_color_cells_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-save-under",
        super::display::builtin_x_display_save_under_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-set-last-user-time",
        super::display::builtin_x_display_set_last_user_time_eval,
        0,
        None,
    );
    ctx.defsubr(
        "x-display-visual-class",
        super::display::builtin_x_display_visual_class_eval,
        0,
        None,
    );
    ctx.defsubr(
        "minor-mode-key-binding",
        super::interactive::builtin_minor_mode_key_binding,
        0,
        None,
    );
    ctx.defsubr(
        "this-command-keys-vector",
        super::interactive::builtin_this_command_keys_vector,
        0,
        None,
    );
    ctx.defsubr(
        "this-single-command-keys",
        super::interactive::builtin_this_single_command_keys,
        0,
        None,
    );
    ctx.defsubr(
        "this-single-command-raw-keys",
        super::interactive::builtin_this_single_command_raw_keys,
        0,
        None,
    );
    ctx.defsubr(
        "clear-this-command-keys",
        super::interactive::builtin_clear_this_command_keys,
        0,
        None,
    );
    ctx.defsubr(
        "waiting-for-user-input-p",
        super::reader::builtin_waiting_for_user_input_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "minibuffer-prompt",
        super::minibuffer::builtin_minibuffer_prompt_eval,
        0,
        None,
    );
    ctx.defsubr(
        "minibuffer-prompt-end",
        super::minibuffer::builtin_minibuffer_prompt_end_eval,
        0,
        None,
    );
    ctx.defsubr(
        "innermost-minibuffer-p",
        super::minibuffer::builtin_innermost_minibuffer_p_eval,
        0,
        None,
    );
    ctx.defsubr(
        "backtrace--frames-from-thread",
        super::misc::builtin_backtrace_frames_from_thread,
        0,
        None,
    );
    ctx.defsubr(
        "abort-minibuffers",
        super::minibuffer::builtin_abort_minibuffers_eval,
        0,
        None,
    );
    ctx.defsubr(
        "set-marker-insertion-type",
        super::marker::builtin_set_marker_insertion_type_eval,
        0,
        None,
    );
    ctx.defsubr(
        "set-standard-case-table",
        super::casetab::builtin_set_standard_case_table_eval,
        0,
        None,
    );
    ctx.defsubr(
        "get-unused-category",
        super::category::builtin_get_unused_category_eval,
        0,
        None,
    );
    ctx.defsubr(
        "standard-category-table",
        super::category::builtin_standard_category_table_eval,
        0,
        None,
    );
    ctx.defsubr(
        "upcase-initials-region",
        super::casefiddle::builtin_upcase_initials_region,
        0,
        None,
    );
    ctx.defsubr(
        "buffer-substring-no-properties",
        super::editfns::builtin_buffer_substring_no_properties,
        0,
        None,
    );

    // Pure builtins from builtins_extra (previously in old match dispatch).
    // These don't need &mut Context, so we wrap them.
    macro_rules! defsubr_pure {
        ($ctx:expr, $name:expr, $func:expr) => {
            $ctx.defsubr($name, |_eval, args| $func(args), 0, None);
        };
    }
    defsubr_pure!(ctx, "take", super::builtins_extra::builtin_take);
    defsubr_pure!(
        ctx,
        "assoc-string",
        super::builtins_extra::builtin_assoc_string
    );
    defsubr_pure!(
        ctx,
        "string-search",
        super::builtins_extra::builtin_string_search
    );
    defsubr_pure!(
        ctx,
        "bare-symbol",
        super::builtins_extra::builtin_bare_symbol
    );
    defsubr_pure!(
        ctx,
        "bare-symbol-p",
        super::builtins_extra::builtin_bare_symbol_p
    );
    defsubr_pure!(ctx, "byteorder", super::builtins_extra::builtin_byteorder);
    defsubr_pure!(
        ctx,
        "car-less-than-car",
        super::builtins_extra::builtin_car_less_than_car
    );
    defsubr_pure!(
        ctx,
        "proper-list-p",
        super::builtins_extra::builtin_proper_list_p
    );
    defsubr_pure!(ctx, "subrp", super::builtins_extra::builtin_subrp);
    defsubr_pure!(
        ctx,
        "byte-code-function-p",
        super::builtins_extra::builtin_byte_code_function_p
    );
    defsubr_pure!(ctx, "closurep", super::builtins_extra::builtin_closurep);
    defsubr_pure!(ctx, "natnump", super::builtins_extra::builtin_natnump);
    defsubr_pure!(ctx, "fixnump", super::builtins_extra::builtin_fixnump);
    defsubr_pure!(ctx, "bignump", super::builtins_extra::builtin_bignump);
    defsubr_pure!(
        ctx,
        "user-login-name",
        super::builtins_extra::builtin_user_login_name
    );
    defsubr_pure!(
        ctx,
        "user-real-login-name",
        super::builtins_extra::builtin_user_real_login_name
    );
    defsubr_pure!(
        ctx,
        "user-full-name",
        super::builtins_extra::builtin_user_full_name
    );
    defsubr_pure!(
        ctx,
        "system-name",
        super::builtins_extra::builtin_system_name
    );
    defsubr_pure!(ctx, "emacs-pid", super::builtins_extra::builtin_emacs_pid);
    defsubr_pure!(
        ctx,
        "memory-use-counts",
        super::builtins_extra::builtin_memory_use_counts
    );

    // Register ALL legacy dispatch builtins as Subr in the obarray.
    // These are builtins still in the old match-based dispatch that
    // havent been migrated to defsubr yet. Without obarray function
    // cells, the bytecode VM cant find them (void-function).
    for name in [
        "%",
        "*",
        "+",
        "-",
        "/",
        "/=",
        "<",
        "<=",
        "=",
        ">",
        ">=",
        "1+",
        "1-",
        "abs",
        "append",
        "ash",
        "assoc-string",
        "assq",
        "bare-symbol",
        "bare-symbol-p",
        "base64-decode-string",
        "base64-encode-string",
        "base64url-encode-string",
        "bidi-find-overridden-directionality",
        "bidi-resolved-levels",
        "bignump",
        "bool-vector",
        "bool-vector-count-consecutive",
        "bool-vector-count-population",
        "bool-vector-exclusive-or",
        "bool-vector-intersection",
        "bool-vector-not",
        "bool-vector-p",
        "bool-vector-set-difference",
        "bool-vector-subsetp",
        "bool-vector-union",
        "buffer-local-toplevel-value",
        "byte-code",
        "byte-code-function-p",
        "byteorder",
        "capitalize",
        "car",
        "car-less-than-car",
        "car-safe",
        "case-table-p",
        "category-docstring",
        "category-set-mnemonics",
        "category-table",
        "category-table-p",
        "ccl-execute",
        "ccl-execute-on-string",
        "ccl-program-p",
        "cdr",
        "cdr-safe",
        "char-charset",
        "char-or-string-p",
        "char-resolve-modifiers",
        "charset-after",
        "charset-id-internal",
        "charsetp",
        "charset-plist",
        "charset-priority-list",
        "char-table-extra-slot",
        "char-table-p",
        "char-table-parent",
        "char-table-range",
        "char-table-subtype",
        "char-width",
        "check-coding-system",
        "check-coding-systems-region",
        "clear-charset-maps",
        "clear-composition-cache",
        "clear-font-cache",
        "clear-image-cache",
        "clear-string",
        "close-font",
        "closurep",
        "cl-type-of",
        "coding-system-p",
        "color-distance",
        "color-gray-p",
        "color-supported-p",
        "color-values-from-color-spec",
        "combine-after-change-execute",
        "combine-windows",
        "command-error-default-function",
        "command-modes",
        "commandp",
        "compare-strings",
        "comp--compile-ctxt-to-file0",
        "comp-el-to-eln-filename",
        "comp-el-to-eln-rel-filename",
        "comp--init-ctxt",
        "comp--install-trampoline",
        "comp--late-register-subr",
        "comp-libgccjit-version",
        "comp-native-compiler-options-effective-p",
        "comp-native-driver-options-effective-p",
        "compose-string-internal",
        "composition-get-gstring",
        "composition-sort-rules",
        "comp--register-lambda",
        "comp--register-subr",
        "comp--release-ctxt",
        "comp--subr-signature",
        "cons",
        "copy-alist",
        "copy-category-table",
        "copy-hash-table",
        "copy-sequence",
        "copysign",
        "copy-syntax-table",
        "current-bidi-paragraph-direction",
        "current-case-table",
        "current-cpu-time",
        "current-idle-time",
        "current-message",
        "current-time",
        "current-time-string",
        "current-time-zone",
        "daemon-initialized",
        "daemonp",
        "dbus-close-inhibitor-lock",
        "dbus-get-unique-name",
        "dbus--init-bus",
        "dbus-make-inhibitor-lock",
        "dbus-message-internal",
        "dbus-registered-inhibitor-locks",
        "debugger-trap",
        "debug-timer-check",
        "declare-equiv-charset",
        "decode-big5-char",
        "decode-char",
        "decode-coding-region",
        "decode-coding-string",
        "decode-sjis-char",
        "decode-time",
        "defconst-1",
        "define-category",
        "define-charset-alias",
        "define-charset-internal",
        "define-coding-system-alias",
        "define-coding-system-internal",
        "define-fringe-bitmap",
        "define-hash-table-test",
        "defvar-1",
        "delete-terminal",
        "describe-buffer-bindings",
        "describe-vector",
        "destroy-fringe-bitmap",
        "ding",
        "directory-file-name",
        "directory-name-p",
        "display-color-cells",
        "display--line-is-continued-p",
        "display--update-for-mouse-movement",
        "do-auto-save",
        "documentation-property",
        "dump-emacs-portable",
        "dump-emacs-portable--sort-predicate",
        "dump-emacs-portable--sort-predicate-copied",
        "emacs-pid",
        "emacs-repository-get-branch",
        "emacs-repository-get-dirty",
        "emacs-repository-get-version",
        "encode-big5-char",
        "encode-char",
        "encode-coding-region",
        "encode-coding-string",
        "encode-sjis-char",
        "encode-time",
        "equal-including-properties",
        "event-convert-list",
        "external-debugging-output",
        "face-attribute-relative-p",
        "face-attributes-as-vector",
        "fceiling",
        "ffloor",
        "file-attributes-lessp",
        "file-name-absolute-p",
        "file-name-as-directory",
        "file-name-completion",
        "file-name-concat",
        "file-name-directory",
        "file-name-nondirectory",
        "fillarray",
        "find-charset-region",
        "find-charset-string",
        "find-composition-internal",
        "find-font",
        "find-operation-coding-system",
        "fixnump",
        "float-time",
        "flush-standard-output",
        "font-at",
        "font-drive-otf",
        "font-face-attributes",
        "font-family-list",
        "font-get",
        "font-get-glyphs",
        "font-get-system-font",
        "font-get-system-normal-font",
        "font-has-char-p",
        "font-info",
        "font-match-p",
        "font-otf-alternates",
        "fontp",
        "font-put",
        "fontset-font",
        "fontset-info",
        "fontset-list",
        "fontset-list-all",
        "font-shape-gstring",
        "font-spec",
        "font-variation-glyphs",
        "font-xlfd-name",
        "force-mode-line-update",
        "force-window-update",
        "format-mode-line",
        "format-time-string",
        "frame-after-make-frame",
        "frame-ancestor-p",
        "frame-bottom-divider-width",
        "frame-child-frame-border-width",
        "frame-focus",
        "frame-font-cache",
        "frame-fringe-width",
        "frame-id",
        "frame-internal-border-width",
        "frame-or-buffer-changed-p",
        "frame-parent",
        "frame-pointer-visible-p",
        "frame-right-divider-width",
        "frame-root-frame",
        "frame-scale-factor",
        "frame-scroll-bar-height",
        "frame-scroll-bar-width",
        "frame--set-was-invisible",
        "frame-windows-min-size",
        "frame-window-state-change",
        "frame--z-order-lessp",
        "frexp",
        "fringe-bitmaps-at-pos",
        "fround",
        "ftruncate",
        "func-arity",
        "function-equal",
        "gap-position",
        "gap-size",
        "garbage-collect-heapsize",
        "garbage-collect-maybe",
        "get-internal-run-time",
        "get-load-suffixes",
        "get-truename-buffer",
        "get-unicode-property-internal",
        "get-unused-iso-final-char",
        "gnutls-asynchronous-parameters",
        "gnutls-available-p",
        "gnutls-boot",
        "gnutls-bye",
        "gnutls-ciphers",
        "gnutls-deinit",
        "gnutls-digests",
        "gnutls-error-fatalp",
        "gnutls-errorp",
        "gnutls-error-string",
        "gnutls-format-certificate",
        "gnutls-get-initstage",
        "gnutls-hash-digest",
        "gnutls-hash-mac",
        "gnutls-macs",
        "gnutls-peer-status",
        "gnutls-peer-status-warning-describe",
        "gnutls-symmetric-decrypt",
        "gnutls-symmetric-encrypt",
        "gpm-mouse-start",
        "gpm-mouse-stop",
        "group-gid",
        "group-name",
        "group-real-gid",
        "handler-bind-1",
        "handle-save-session",
        "handle-switch-frame",
        "hash-table-rehash-size",
        "hash-table-rehash-threshold",
        "hash-table-size",
        "hash-table-test",
        "hash-table-weakness",
        "help--describe-vector",
        "identity",
        "image-cache-size",
        "image-flush",
        "imagemagick-types",
        "image-mask-p",
        "image-metadata",
        "imagep",
        "image-size",
        "image-transforms-p",
        "init-image-library",
        "inotify-add-watch",
        "inotify-allocated-p",
        "inotify-rm-watch",
        "inotify-valid-p",
        "inotify-watch-list",
        "integer-or-marker-p",
        "interactive-form",
        "internal-char-font",
        "internal-complete-buffer",
        "internal-copy-lisp-face",
        "internal-decode-string-utf-8",
        "internal-delete-indirect-variable",
        "internal-describe-syntax-value",
        "internal-encode-string-utf-8",
        "internal-event-symbol-parse-modifiers",
        "internal-face-x-get-resource",
        "internal-get-lisp-face-attribute",
        "internal-handle-focus-in",
        "internal--hash-table-buckets",
        "internal--hash-table-histogram",
        "internal--hash-table-index-size",
        "internal--labeled-narrow-to-region",
        "internal--labeled-widen",
        "internal-lisp-face-attribute-values",
        "internal-lisp-face-empty-p",
        "internal-lisp-face-equal-p",
        "internal-lisp-face-p",
        "internal-make-lisp-face",
        "internal-make-var-non-special",
        "internal-merge-in-global-face",
        "internal--obarray-buckets",
        "internal-set-alternative-font-family-alist",
        "internal-set-alternative-font-registry-alist",
        "internal--set-buffer-modified-tick",
        "internal-set-font-selection-order",
        "internal-set-lisp-face-attribute",
        "internal-set-lisp-face-attribute-from-resource",
        "internal-stack-stats",
        "internal-subr-documentation",
        "internal--track-mouse",
        "interpreted-function-p",
        "invisible-p",
        "invocation-directory",
        "invocation-name",
        "iso-charset",
        "json-parse-string",
        "json-serialize",
        "key-description",
        "keymap--get-keyelt",
        "keymap-prompt",
        "kill-emacs",
        "lcms2-available-p",
        "lcms-cam02-ucs",
        "lcms-cie-de2000",
        "lcms-jab->jch",
        "lcms-jch->jab",
        "lcms-jch->xyz",
        "lcms-temp->white-point",
        "lcms-xyz->jch",
        "ldexp",
        "length",
        "length<",
        "length=",
        "length>",
        "libxml-available-p",
        "libxml-parse-html-region",
        "libxml-parse-xml-region",
        "line-number-display-width",
        "line-pixel-height",
        "list",
        "list-fonts",
        "load-average",
        "locale-info",
        "local-variable-if-set-p",
        "locate-file-internal",
        "lock-buffer",
        "lock-file",
        "logand",
        "logb",
        "logcount",
        "logior",
        "lognot",
        "logxor",
        "long-line-optimizations-p",
        "looking-at",
        "lookup-image",
        "lookup-image-map",
        "lossage-size",
        "lower-frame",
        "lread--substitute-object-in-subtree",
        "make-bool-vector",
        "make-byte-code",
        "make-category-set",
        "make-category-table",
        "make-char",
        "make-char-table",
        "make-closure",
        "make-finalizer",
        "make-frame-invisible",
        "make-indirect-buffer",
        "make-interpreted-closure",
        "make-list",
        "make-marker",
        "make-record",
        "make-symbol",
        "make-temp-file-internal",
        "make-temp-name",
        "make-terminal-frame",
        "malloc-info",
        "malloc-trim",
        "mapbacktrace",
        "map-charset-chars",
        "map-keymap",
        "map-keymap-internal",
        "marker-buffer",
        "marker-insertion-type",
        "marker-last-position",
        "markerp",
        "match-beginning",
        "match-data",
        "match-end",
        "matching-paren",
        "max",
        "max-char",
        "md5",
        "member",
        "memory-info",
        "memory-use-counts",
        "memq",
        "memql",
        "menu-bar-menu-at-x-y",
        "menu-or-popup-active-p",
        "merge-face-attribute",
        "message",
        "message-box",
        "message-or-box",
        "min",
        "mod",
        "module-function-p",
        "module-load",
        "mouse-pixel-position",
        "mouse-position",
        "mouse-position-in-root-frame",
        "move-point-visually",
        "move-to-window-line",
        "multibyte-char-to-unibyte",
        "multibyte-string-p",
        "native-comp-available-p",
        "native-comp-function-p",
        "native-comp-unit-file",
        "native-comp-unit-set-file",
        "native-elisp-load",
        "natnump",
        "neomacs-clipboard-get",
        "neomacs-clipboard-set",
        "neomacs-core-backend",
        "neomacs-display-monitor-attributes-list",
        "neomacs-frame-edges",
        "neomacs-frame-geometry",
        "neomacs-mouse-absolute-pixel-position",
        "neomacs-primary-selection-get",
        "neomacs-primary-selection-set",
        "neomacs-set-mouse-absolute-pixel-position",
        "new-fontset",
        "newline-cache-check",
        "next-frame",
        "next-read-file-uses-dialog-p",
        "ngettext",
        "nreverse",
        "nth",
        "nthcdr",
        "number-or-marker-p",
        "obarray-clear",
        "obarray-make",
        "object-intervals",
        "old-selected-frame",
        "open-dribble-file",
        "open-font",
        "open-termscript",
        "optimize-char-table",
        "overlay-lists",
        "overlay-recenter",
        "overlay-tree",
        "pdumper-stats",
        "play-sound-internal",
        "plist-get",
        "plist-put",
        "position-symbol",
        "posn-at-point",
        "posn-at-x-y",
        "prefix-numeric-value",
        "previous-frame",
        "prin1",
        "prin1-to-string",
        "princ",
        "print",
        "process-connection",
        "profiler-cpu-log",
        "profiler-cpu-running-p",
        "profiler-cpu-start",
        "profiler-cpu-stop",
        "profiler-memory-log",
        "profiler-memory-running-p",
        "profiler-memory-start",
        "profiler-memory-stop",
        "proper-list-p",
        "propertize",
        "put-unicode-property-internal",
        "query-font",
        "query-fontset",
        "raise-frame",
        "rassoc",
        "rassq",
        "read-coding-system",
        "read-non-nil-coding-system",
        "read-positioning-symbols",
        "recent-auto-save-p",
        "reconsider-frame-fonts",
        "record",
        "recordp",
        "re--describe-compiled",
        "redirect-debugging-output",
        "redirect-frame-focus",
        "redisplay",
        "redraw-display",
        "regexp-quote",
        "register-ccl-program",
        "register-code-conversion-map",
        "remember-mouse-glyph",
        "remove-pos-from-symbol",
        "resize-mini-window-internal",
        "restore-buffer-modified-p",
        "reverse",
        "safe-length",
        "secure-hash",
        "secure-hash-algorithms",
        "set-binary-mode",
        "set-buffer-auto-saved",
        "set-buffer-local-toplevel-value",
        "set-buffer-major-mode",
        "set-buffer-redisplay",
        "setcar",
        "set-case-table",
        "set-category-table",
        "setcdr",
        "set-charset-plist",
        "set-charset-priority",
        "set-char-table-extra-slot",
        "set-char-table-parent",
        "set-char-table-range",
        "set-coding-system-priority",
        "set-file-acl",
        "set-file-selinux-context",
        "set-fontset-font",
        "set-frame-size-and-position-pixelwise",
        "set-frame-window-state-change",
        "set-fringe-bitmap-face",
        "set-keyboard-coding-system-internal",
        "set-match-data",
        "set-minibuffer-window",
        "set-mouse-pixel-position",
        "set-mouse-position",
        "set-safe-terminal-coding-system-internal",
        "set-terminal-coding-system-internal",
        "set-text-conversion-style",
        "set--this-command-keys",
        "set-time-zone-rule",
        "set-window-new-normal",
        "set-window-new-pixel",
        "set-window-new-total",
        "signal",
        "single-key-description",
        "Snarf-documentation",
        "sort-charsets",
        "split-char",
        "sqlite-available-p",
        "sqlite-close",
        "sqlite-columns",
        "sqlite-commit",
        "sqlite-execute",
        "sqlite-execute-batch",
        "sqlite-finalize",
        "sqlite-load-extension",
        "sqlite-more-p",
        "sqlite-next",
        "sqlite-open",
        "sqlitep",
        "sqlite-pragma",
        "sqlite-rollback",
        "sqlite-select",
        "sqlite-transaction",
        "sqlite-version",
        "standard-case-table",
        "standard-syntax-table",
        "string-as-multibyte",
        "string-as-unibyte",
        "string-bytes",
        "string-collate-equalp",
        "string-collate-lessp",
        "string-distance",
        "string-make-multibyte",
        "string-make-unibyte",
        "string-match",
        "string-search",
        "string-to-multibyte",
        "string-to-syntax",
        "string-to-unibyte",
        "string-version-lessp",
        "subr-arity",
        "subr-name",
        "subr-native-comp-unit",
        "subr-native-lambda-list",
        "subrp",
        "subr-type",
        "substitute-in-file-name",
        "substring-no-properties",
        "suspend-emacs",
        "sxhash-eq",
        "sxhash-eql",
        "sxhash-equal",
        "sxhash-equal-including-properties",
        "symbol-name",
        "symbolp",
        "symbol-with-pos-p",
        "symbol-with-pos-pos",
        "syntax-class-to-char",
        "syntax-table-p",
        "system-groups",
        "system-name",
        "system-users",
        "tab-bar-height",
        "take",
        "terminal-list",
        "terpri",
        "text-char-description",
        "text-quoting-style",
        "thread--blocker",
        "thread-buffer-disposition",
        "thread-set-buffer-disposition",
        "time-add",
        "time-convert",
        "time-equal-p",
        "time-less-p",
        "time-subtract",
        "tool-bar-get-system-style",
        "tool-bar-height",
        "tool-bar-pixel-width",
        "translate-region-internal",
        "transpose-regions",
        "treesit-available-p",
        "treesit-compiled-query-p",
        "treesit-grammar-location",
        "treesit-induce-sparse-tree",
        "treesit-language-abi-version",
        "treesit-language-available-p",
        "treesit-library-abi-version",
        "treesit--linecol-at",
        "treesit--linecol-cache",
        "treesit--linecol-cache-set",
        "treesit-node-check",
        "treesit-node-child",
        "treesit-node-child-by-field-name",
        "treesit-node-child-count",
        "treesit-node-descendant-for-range",
        "treesit-node-end",
        "treesit-node-eq",
        "treesit-node-field-name-for-child",
        "treesit-node-first-child-for-pos",
        "treesit-node-match-p",
        "treesit-node-next-sibling",
        "treesit-node-p",
        "treesit-node-parent",
        "treesit-node-parser",
        "treesit-node-prev-sibling",
        "treesit-node-start",
        "treesit-node-string",
        "treesit-node-type",
        "treesit-parser-add-notifier",
        "treesit-parser-buffer",
        "treesit-parser-changed-regions",
        "treesit-parser-create",
        "treesit-parser-delete",
        "treesit-parser-embed-level",
        "treesit-parser-included-ranges",
        "treesit-parser-language",
        "treesit-parser-list",
        "treesit-parser-notifiers",
        "treesit-parser-p",
        "treesit-parser-remove-notifier",
        "treesit-parser-root-node",
        "treesit-parser-set-embed-level",
        "treesit-parser-set-included-ranges",
        "treesit-parser-tag",
        "treesit-parser-tracking-line-column-p",
        "treesit-parse-string",
        "treesit-pattern-expand",
        "treesit-query-capture",
        "treesit-query-compile",
        "treesit-query-eagerly-compiled-p",
        "treesit-query-expand",
        "treesit-query-language",
        "treesit-query-p",
        "treesit-query-source",
        "treesit-search-forward",
        "treesit-search-subtree",
        "treesit-subtree-stat",
        "treesit-tracking-line-column-p",
        "tty-display-pixel-height",
        "tty-display-pixel-width",
        "tty-frame-at",
        "tty-frame-edges",
        "tty-frame-geometry",
        "tty-frame-list-z-order",
        "tty-frame-restack",
        "tty--output-buffer-size",
        "tty--set-output-buffer-size",
        "tty-suppress-bold-inverse-default-colors",
        "uncombine-window",
        "undo-boundary",
        "unencodable-char-position",
        "unhandled-file-name-directory",
        "unibyte-char-to-multibyte",
        "unicode-property-table-internal",
        "unify-charset",
        "unix-sync",
        "unlock-buffer",
        "unlock-file",
        "upcase-initials",
        "user-full-name",
        "user-login-name",
        "user-ptrp",
        "user-real-login-name",
        "user-real-uid",
        "user-uid",
        "value<",
        "variable-binding-locus",
        "vector-or-char-table-p",
        "visited-file-modtime",
        "void-variable",
        "window-bottom-divider-width",
        "window-cursor-info",
        "window-discard-buffer-from-window",
        "window-left-child",
        "window-line-height",
        "window-lines-pixel-dimensions",
        "window-new-normal",
        "window-new-pixel",
        "window-new-total",
        "window-next-sibling",
        "window-normal-size",
        "window-old-body-pixel-height",
        "window-old-body-pixel-width",
        "window-old-pixel-height",
        "window-old-pixel-width",
        "window-parent",
        "window-pixel-left",
        "window-pixel-top",
        "window-prev-sibling",
        "window-resize-apply",
        "window-right-divider-width",
        "window-scroll-bar-height",
        "window-scroll-bar-width",
        "window-top-child",
        "write-char",
        "wrong-type-argument",
        "x-backspace-delete-keys-p",
        "x-begin-drag",
        "x-change-window-property",
        "x-close-connection",
        "x-delete-window-property",
        "x-disown-selection-internal",
        "x-display-list",
        "x-display-mm-height",
        "x-display-mm-width",
        "x-display-planes",
        "x-display-screens",
        "x-double-buffered-p",
        "x-export-frames",
        "x-family-fonts",
        "x-focus-frame",
        "x-frame-edges",
        "x-frame-geometry",
        "x-frame-list-z-order",
        "x-frame-restack",
        "x-get-atom-name",
        "x-get-local-selection",
        "x-get-modifier-masks",
        "x-get-resource",
        "x-get-selection-internal",
        "x-hide-tip",
        "x-internal-focus-input-context",
        "x-list-fonts",
        "x-load-color-file",
        "x-menu-bar-open-internal",
        "x-mouse-absolute-pixel-position",
        "x-open-connection",
        "x-own-selection-internal",
        "x-parse-geometry",
        "x-popup-dialog",
        "x-popup-menu",
        "x-register-dnd-atom",
        "x-scroll-bar-background",
        "x-scroll-bar-foreground",
        "x-selection-exists-p",
        "x-selection-owner-p",
        "x-send-client-message",
        "x-server-input-extension-version",
        "x-server-vendor",
        "x-server-version",
        "x-set-mouse-absolute-pixel-position",
        "x-show-tip",
        "x-synchronize",
        "x-translate-coordinates",
        "x-uses-old-gtk-dialog",
        "xw-color-defined-p",
        "xw-color-values",
        "xw-display-color-p",
        "x-window-property",
        "x-window-property-attributes",
        "x-wm-set-size-hint",
        "yes-or-no-p",
        "zlib-available-p",
        "zlib-decompress-region",
        // PureBuiltinId entries that were missing from the list above.
        // All builtins must be registered in the obarray so that
        // (fboundp 'name) returns t – needed by cl-preloaded.el etc.
        "aref",
        "arrayp",
        "aset",
        "atom",
        "bitmap-spec-p",
        "booleanp",
        "bufferp",
        "byte-to-string",
        "ceiling",
        "char-to-string",
        "char-uppercase-p",
        "characterp",
        "clear-buffer-auto-save-failure",
        "clear-face-cache",
        "clrhash",
        "concat",
        "consp",
        "downcase",
        "eq",
        "eql",
        "equal",
        "float",
        "floatp",
        "floor",
        "gethash",
        "hash-table-count",
        "hash-table-p",
        "ignore",
        "integer-or-null-p",
        "integerp",
        "keywordp",
        "list-of-strings-p",
        "listp",
        "make-hash-table",
        "make-vector",
        "nlistp",
        "not",
        "null",
        "number-to-string",
        "numberp",
        "puthash",
        "remhash",
        "round",
        "sequencep",
        "string-equal",
        "string-greaterp",
        "string-lessp",
        "string-or-null-p",
        "string-to-char",
        "string-to-number",
        "stringp",
        "substring",
        "truncate",
        "type-of",
        "unibyte-string",
        "upcase",
        "vconcat",
        "vector",
        "vectorp",
    ] {
        let id = super::intern::intern(name);
        if ctx.obarray.symbol_function(name).is_none() {
            ctx.obarray.set_symbol_function(name, Value::Subr(id));
        }
    }
}
