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
pub(super) use super::builtin_registry;
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

    // Legacy dispatch — will shrink as builtins migrate to defsubr.
    match name {
        "macrop" => return Some(super::builtins::symbols::builtin_macrop_eval(eval, args)),
        // Symbol/obarray
        "obarrayp" => return Some(builtin_obarrayp(args)),
        // Hooks
        "run-hooks" => {
            let hook_names: Vec<String> = args
                .iter()
                .filter_map(|a| a.as_symbol_name().map(|s| s.to_string()))
                .collect();
            // Only log important hooks at info; the rest at debug to avoid
            // flooding the log with custom-define-hook during bootstrap.
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
            return Some(result);
        }
        // GC
        // Loading
        "load" => {
            let file_name = args.first().map(|a| format!("{}", a)).unwrap_or_default();
            tracing::info!(file = %file_name, "load called");
            let result = builtin_load(eval, args);
            tracing::info!(file = %file_name, ok = result.is_ok(), "load returned");
            return Some(result);
        }
        // Buffer operations
        // set-mark and mark are now in navigation module (below)
        "ntake" => return Some(builtin_ntake(args)),
        // Search / regex operations
        // charset (evaluator-dependent)
        // composite (evaluator-dependent)
        // xdisp (evaluator-dependent)

        // Font (evaluator-dependent — frame designator validation)

        // File I/O (evaluator-dependent)
        "default-file-modes" => return Some(super::fileio::builtin_default_file_modes(args)),
        "set-default-file-modes" => {
            return Some(super::fileio::builtin_set_default_file_modes(args));
        }
        // Keymap operations
        // Process operations (evaluator-dependent)
        "set-process-inherit-coding-system-flag" => {
            return Some(
                super::process::builtin_set_process_inherit_coding_system_flag(eval, args),
            );
        }
        // Timer operations (evaluator-dependent)
        // Variable watchers
        // Syntax table operations (evaluator-dependent)
        // Register operations (evaluator-dependent)
        // Keyboard macro operations (evaluator-dependent)
        "cancel-kbd-macro-events" => return Some(builtin_cancel_kbd_macro_events(args)),
        // Bookmark operations (evaluator-dependent)
        // Abbreviation operations (evaluator-dependent)
        // Text property operations (evaluator-dependent — buffer access)

        // Navigation / mark / region (evaluator-dependent — buffer access)
        // Custom system (evaluator-dependent)

        // Autoload (evaluator-dependent)

        // Kill ring / text editing (evaluator-dependent — buffer access)

        // Rectangle operations (evaluator-dependent — buffer access)
        // Window/frame operations (evaluator-dependent)
        "compute-motion" => {
            return Some(super::builtins::buffers::builtin_compute_motion(eval, args));
        }
        "window-configuration-p" => return Some(builtin_window_configuration_p(args)),
        "window-configuration-frame" => return Some(builtin_window_configuration_frame(args)),
        "window-configuration-equal-p" => return Some(builtin_window_configuration_equal_p(args)),
        "frame-parameter" => {
            tracing::debug!(param = ?args.get(1).map(|v| format!("{}", v)), "frame-parameter called");
            return Some(super::window_cmds::builtin_frame_parameter(eval, args));
        }
        "send-string-to-terminal" => {
            return Some(super::dispnew::pure::builtin_send_string_to_terminal_eval(
                eval, args,
            ));
        }
        "internal-show-cursor" => {
            return Some(super::dispnew::pure::builtin_internal_show_cursor_eval(
                eval, args,
            ));
        }
        "internal-show-cursor-p" => {
            return Some(super::dispnew::pure::builtin_internal_show_cursor_p_eval(
                eval, args,
            ));
        }
        "redraw-frame" => return Some(super::dispnew::pure::builtin_redraw_frame_eval(eval, args)),
        "display-supports-face-attributes-p" => {
            return Some(
                super::display::builtin_display_supports_face_attributes_p_eval(eval, args),
            );
        }
        "terminal-name" => {
            return Some(super::terminal::pure::builtin_terminal_name_eval(
                eval, args,
            ));
        }
        "terminal-live-p" => {
            return Some(super::terminal::pure::builtin_terminal_live_p_eval(
                eval, args,
            ));
        }
        "terminal-parameter" => {
            return Some(super::terminal::pure::builtin_terminal_parameter_eval(
                eval, args,
            ));
        }
        "terminal-parameters" => {
            return Some(super::terminal::pure::builtin_terminal_parameters_eval(
                eval, args,
            ));
        }
        "set-terminal-parameter" => {
            return Some(super::terminal::pure::builtin_set_terminal_parameter_eval(
                eval, args,
            ));
        }
        "tty-type" => return Some(super::terminal::pure::builtin_tty_type_eval(eval, args)),
        "tty-top-frame" => {
            return Some(super::terminal::pure::builtin_tty_top_frame_eval(
                eval, args,
            ));
        }
        "tty-display-color-p" => {
            return Some(super::terminal::pure::builtin_tty_display_color_p_eval(
                eval, args,
            ));
        }
        "tty-display-color-cells" => {
            return Some(super::terminal::pure::builtin_tty_display_color_cells_eval(
                eval, args,
            ));
        }
        "tty-no-underline" => {
            return Some(super::terminal::pure::builtin_tty_no_underline_eval(
                eval, args,
            ));
        }
        "controlling-tty-p" => {
            return Some(super::terminal::pure::builtin_controlling_tty_p_eval(
                eval, args,
            ));
        }
        "suspend-tty" => return Some(super::terminal::pure::builtin_suspend_tty_eval(eval, args)),
        "resume-tty" => return Some(super::terminal::pure::builtin_resume_tty_eval(eval, args)),
        "frame-terminal" => {
            return Some(super::terminal::pure::builtin_frame_terminal_eval(
                eval, args,
            ));
        }
        "x-display-monitor-attributes-list" => {
            return Some(
                super::display::builtin_x_display_monitor_attributes_list_eval(eval, args),
            );
        }

        // Interactive / command system (evaluator-dependent)
        // Error hierarchy (evaluator-dependent — reads obarray)

        // Reader/printer (evaluator-dependent)
        "message" => {
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
            return Some(builtin_message_eval(eval, args));
        }
        "set-input-meta-mode" => return Some(super::reader::builtin_set_input_meta_mode(args)),
        "set-output-flow-control" => {
            return Some(super::reader::builtin_set_output_flow_control(args));
        }
        "set-quit-char" => return Some(super::reader::builtin_set_quit_char(args)),
        "read-char" => {
            tracing::info!("read-char called (will block for input)");
            return Some(super::reader::builtin_read_char(eval, args));
        }
        "minibuffer-innermost-command-loop-p" => {
            return Some(
                super::minibuffer::builtin_minibuffer_innermost_command_loop_p_eval(eval, args),
            );
        }

        // Misc (evaluator-dependent)
        "top-level" => return Some(super::minibuffer::builtin_top_level(args)),
        "recursive-edit" => {
            tracing::info!("dispatch_builtin: recursive-edit called");
            return Some(super::minibuffer::builtin_recursive_edit_eval(eval, args));
        }

        // Threading (evaluator-dependent)

        // Undo system (evaluator-dependent)
        // Hash-table / obarray (evaluator-dependent)

        // Marker (evaluator-dependent)

        // Case table (evaluator-dependent)

        // Category (evaluator-dependent)

        // Char-table (evaluator-dependent — applies function)

        // Coding system (evaluator-dependent — uses coding_systems manager)
        "coding-system-aliases" => {
            return Some(super::coding::builtin_coding_system_aliases(
                &eval.coding_systems,
                args,
            ));
        }
        "coding-system-plist" => {
            return Some(super::coding::builtin_coding_system_plist(
                &eval.coding_systems,
                args,
            ));
        }
        "coding-system-put" => {
            return Some(super::coding::builtin_coding_system_put(
                &mut eval.coding_systems,
                args,
            ));
        }
        "coding-system-base" => {
            return Some(super::coding::builtin_coding_system_base(
                &eval.coding_systems,
                args,
            ));
        }
        "coding-system-eol-type" => {
            return Some(super::coding::builtin_coding_system_eol_type(
                &eval.coding_systems,
                args,
            ));
        }
        "detect-coding-string" => {
            return Some(super::coding::builtin_detect_coding_string(
                &eval.coding_systems,
                args,
            ));
        }
        "detect-coding-region" => {
            return Some(super::coding::builtin_detect_coding_region(
                &eval.coding_systems,
                args,
            ));
        }
        "keyboard-coding-system" => {
            return Some(super::coding::builtin_keyboard_coding_system(
                &eval.coding_systems,
                args,
            ));
        }
        "terminal-coding-system" => {
            return Some(super::coding::builtin_terminal_coding_system(
                &eval.coding_systems,
                args,
            ));
        }
        "coding-system-priority-list" => {
            return Some(super::coding::builtin_coding_system_priority_list(
                &eval.coding_systems,
                args,
            ));
        }
        "find-coding-systems-region-internal" => {
            return Some(
                super::coding::builtin_find_coding_systems_region_internal_eval(eval, args),
            );
        }

        // Documentation/help (evaluator-dependent)
        "documentation-stringp" => return Some(builtin_documentation_stringp(args)),

        // Indentation (evaluator-dependent)
        // Case/char (evaluator-dependent)

        // Search (evaluator-dependent)
        "posix-search-forward" => {
            // Reuse regex search engine for now; this replaces nil-stub behavior.
            return Some(builtin_re_search_forward(eval, args));
        }
        "posix-search-backward" => {
            // Reuse regex search engine for now; this replaces nil-stub behavior.
            return Some(builtin_re_search_backward(eval, args));
        }
        // Lread (evaluator-dependent)
        "read-event" => {
            tracing::info!("read-event called (will block for input)");
            return Some(super::lread::builtin_read_event(eval, args));
        }

        // Editfns (evaluator-dependent)

        _ => {}
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
        "default-file-modes" => super::fileio::builtin_default_file_modes(args),
        "set-default-file-modes" => super::fileio::builtin_set_default_file_modes(args),
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
        "redraw-frame" => super::dispnew::pure::builtin_redraw_frame(args),
        "redraw-display" => super::dispnew::pure::builtin_redraw_display(args),
        "open-termscript" => super::dispnew::pure::builtin_open_termscript(args),
        "ding" => super::dispnew::pure::builtin_ding(args),
        "send-string-to-terminal" => super::dispnew::pure::builtin_send_string_to_terminal(args),
        "internal-show-cursor" => super::dispnew::pure::builtin_internal_show_cursor(args),
        "internal-show-cursor-p" => super::dispnew::pure::builtin_internal_show_cursor_p(args),
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
        "terminal-name" => super::terminal::pure::builtin_terminal_name(args),
        "terminal-list" => super::terminal::pure::builtin_terminal_list(args),
        "frame-terminal" => super::terminal::pure::builtin_frame_terminal(args),
        "terminal-live-p" => super::terminal::pure::builtin_terminal_live_p(args),
        "terminal-parameter" => super::terminal::pure::builtin_terminal_parameter(args),
        "terminal-parameters" => super::terminal::pure::builtin_terminal_parameters(args),
        "set-terminal-parameter" => super::terminal::pure::builtin_set_terminal_parameter(args),
        "tty-type" => super::terminal::pure::builtin_tty_type(args),
        "tty-top-frame" => super::terminal::pure::builtin_tty_top_frame(args),
        "tty-display-color-p" => super::terminal::pure::builtin_tty_display_color_p(args),
        "tty-display-color-cells" => super::terminal::pure::builtin_tty_display_color_cells(args),
        "tty-no-underline" => super::terminal::pure::builtin_tty_no_underline(args),
        "controlling-tty-p" => super::terminal::pure::builtin_controlling_tty_p(args),
        "suspend-tty" => super::terminal::pure::builtin_suspend_tty(args),
        "resume-tty" => super::terminal::pure::builtin_resume_tty(args),
        "display-supports-face-attributes-p" => {
            super::display::builtin_display_supports_face_attributes_p(args)
        }
        "x-server-version" => super::display::builtin_x_server_version(args),
        "x-server-input-extension-version" => {
            super::display::builtin_x_server_input_extension_version(args)
        }
        "x-server-vendor" => super::display::builtin_x_server_vendor(args),
        "display-color-cells" => super::display::builtin_display_color_cells(args),
        "x-display-mm-height" => super::display::builtin_x_display_mm_height(args),
        "x-display-mm-width" => super::display::builtin_x_display_mm_width(args),
        "x-display-monitor-attributes-list" => {
            super::display::builtin_x_display_monitor_attributes_list(args)
        }
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
        "file-name-completion" => super::dired::builtin_file_name_completion(args),
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
        "minibuffer-innermost-command-loop-p" => return None,
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
        "ntake" => builtin_ntake(args),
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
        "locate-file-internal" => super::lread::builtin_locate_file_internal(args),
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

/// Dispatch to pure builtins that don't need evaluator access.
/// Used by the bytecode VM.
pub(crate) fn dispatch_builtin_pure(name: &str, args: Vec<Value>) -> Option<EvalResult> {
    match name {
        "functionp"
        | "format-message"
        | "error"
        | "copy-file"
        | "defvaralias"
        | "delete-file"
        | "indirect-variable"
        | "insert-and-inherit"
        | "insert-before-markers-and-inherit"
        | "insert-buffer-substring"
        | "kill-all-local-variables"
        | "make-directory"
        | "make-temp-file"
        | "macroexpand"
        | "message"
        | "message-box"
        | "message-or-box"
        | "princ"
        | "prin1"
        | "prin1-to-string"
        | "print"
        | "rename-file"
        | "replace-buffer-contents"
        | "set-buffer-multibyte"
        | "split-window-internal"
        | "setplist"
        | "terminal-live-p"
        | "terminal-name"
        | "terpri"
        | "undo-boundary"
        | "write-char"
        | "assoc"
        | "plist-member"
        | "window-list-1"
        | "window-bump-use-time"
        | "old-selected-window"
        | "frame-old-selected-window"
        | "window-left-child"
        | "window-next-sibling"
        | "window-normal-size"
        | "window-parent"
        | "window-pixel-left"
        | "window-pixel-top"
        | "window-prev-sibling"
        | "set-frame-selected-window"
        | "window-system"
        | "window-top-child"
        | "frame-edges"
        | "window-at" => return None,
        _ => {}
    }

    if let Ok(id) = name.parse::<PureBuiltinId>() {
        return Some(dispatch_builtin_id_pure(id, args));
    }

    Some(match name {
        // Arithmetic (typed subset is dispatched above)
        // Type predicates and equality (typed subset is dispatched above)
        "signal" => super::errors::builtin_signal(args),
        "integer-or-marker-p" => builtin_integer_or_marker_p(args),
        "number-or-marker-p" => builtin_number_or_marker_p(args),
        "vector-or-char-table-p" => builtin_vector_or_char_table_p(args),
        "markerp" => super::marker::builtin_markerp(args),
        "marker-buffer" => super::marker::builtin_marker_buffer(args),
        "marker-insertion-type" => super::marker::builtin_marker_insertion_type(args),
        "marker-position" => super::marker::builtin_marker_position(args),
        "make-marker" => super::marker::builtin_make_marker(args),
        "bool-vector-p" => super::chartable::builtin_bool_vector_p(args),
        "make-category-set" => super::category::builtin_make_category_set(args),
        "macrop" => super::subr_info::builtin_macrop(args),
        "function-equal" => builtin_function_equal(args),
        "module-function-p" => builtin_module_function_p(args),
        "user-ptrp" => builtin_user_ptrp(args),
        "symbol-with-pos-p" => builtin_symbol_with_pos_p(args),
        "symbol-with-pos-pos" => builtin_symbol_with_pos_pos(args),
        // Cons/List (typed subset is dispatched above)
        "length<" => builtin_length_lt(args),
        "length=" => builtin_length_eq(args),
        "length>" => builtin_length_gt(args),
        "substring-no-properties" => builtin_substring_no_properties(args),
        // String (typed subset is dispatched above)
        // Vector/hash/conversion/plist/symbol (typed subset is dispatched above)
        // Math
        "sqrt" => builtin_sqrt(args),
        "sin" => builtin_sin(args),
        "cos" => builtin_cos(args),
        "tan" => builtin_tan(args),
        "asin" => builtin_asin(args),
        "acos" => builtin_acos(args),
        "atan" => builtin_atan(args),
        "exp" => builtin_exp(args),
        "log" => builtin_log(args),
        "expt" => builtin_expt(args),
        "random" => builtin_random(args),
        "isnan" => builtin_isnan(args),
        // Extended string
        "make-string" => builtin_make_string(args),
        "string" => builtin_string(args),
        "string-width" => builtin_string_width(args),
        // Extended list
        "delete" => builtin_delete(args),
        "delq" => builtin_delq(args),
        "elt" => builtin_elt(args),
        "memql" => builtin_memql(args),
        "nconc" => builtin_nconc(args),
        // Output / misc
        "identity" => builtin_identity(args),
        "current-message" => builtin_current_message(args),
        "format" => super::builtins::strings::builtin_format(args),
        "ngettext" => builtin_ngettext(args),
        "secure-hash-algorithms" => builtin_secure_hash_algorithms(args),
        "prefix-numeric-value" => builtin_prefix_numeric_value(args),
        "propertize" => builtin_propertize(args),
        "documentation-stringp" => super::builtins::misc_pure::builtin_documentation_stringp(args),
        "bare-symbol" => super::builtins_extra::builtin_bare_symbol(args),
        "capitalize" => super::casefiddle::builtin_capitalize(args),
        "charsetp" => super::charset::builtin_charsetp(args),
        "charset-plist" => super::charset::builtin_charset_plist(args),
        "define-charset-internal" => super::charset::builtin_define_charset_internal(args),
        "define-charset-alias" => super::charset::builtin_define_charset_alias(args),
        "internal-lisp-face-p" => super::font::builtin_internal_lisp_face_p(args),
        "internal-make-lisp-face" => super::font::builtin_internal_make_lisp_face(args),
        "internal-set-lisp-face-attribute" => {
            super::font::builtin_internal_set_lisp_face_attribute(args)
        }
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
        "frame--z-order-lessp" => super::dispnew::pure::builtin_frame_z_order_lessp(args),
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
        "default-file-modes" => super::fileio::builtin_default_file_modes(args),
        "set-default-file-modes" => super::fileio::builtin_set_default_file_modes(args),
        "make-temp-name" => super::fileio::builtin_make_temp_name(args),
        "next-read-file-uses-dialog-p" => super::fileio::builtin_next_read_file_uses_dialog_p(args),
        "unhandled-file-name-directory" => {
            super::fileio::builtin_unhandled_file_name_directory(args)
        }
        "get-truename-buffer" => super::fileio::builtin_get_truename_buffer(args),
        // Keymap (pure)
        "single-key-description" => builtin_single_key_description(args),
        "key-description" => builtin_key_description(args),
        "event-convert-list" => builtin_event_convert_list(args),
        "text-char-description" => builtin_text_char_description(args),
        // Process (pure)
        "set-binary-mode" => super::process::builtin_set_binary_mode(args),
        // Editfns (pure)
        "group-name" => super::editfns::builtin_group_name(args),
        "group-gid" => super::editfns::builtin_group_gid(args),
        "group-real-gid" => super::editfns::builtin_group_real_gid(args),
        "load-average" => super::editfns::builtin_load_average(args),
        "logcount" => super::editfns::builtin_logcount(args),
        // Timer (pure)
        // Undo system (pure)
        // Keyboard macro (pure)
        // Character encoding (pure)
        "char-width" => crate::encoding::builtin_char_width(args),
        "string-bytes" => crate::encoding::builtin_string_bytes(args),
        "multibyte-string-p" => crate::encoding::builtin_multibyte_string_p(args),
        "encode-coding-string" => crate::encoding::builtin_encode_coding_string(args),
        "decode-coding-string" => crate::encoding::builtin_decode_coding_string(args),
        "char-or-string-p" => crate::encoding::builtin_char_or_string_p(args),
        "max-char" => crate::encoding::builtin_max_char(args),
        // Display/terminal (pure)
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
        "font-at" => builtin_font_at(args),
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
        "defconst-1" => return None,
        "define-coding-system-internal" => {
            return None; // dispatched via eval-aware path
        }
        "defvar-1" => return None,
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
        "minibuffer-innermost-command-loop-p" => return None,
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
        "ntake" => builtin_ntake(args),
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
        "redisplay" => builtin_redisplay(args),
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
        "xw-display-color-p" => builtin_xw_display_color_p(args),
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
        // atimer.c gap-fill
        "debug-timer-check" => builtin_debug_timer_check(args),

        // dbusbind.c gap-fill
        "dbus-close-inhibitor-lock" => builtin_dbus_close_inhibitor_lock(args),
        "dbus-make-inhibitor-lock" => builtin_dbus_make_inhibitor_lock(args),
        "dbus-registered-inhibitor-locks" => builtin_dbus_registered_inhibitor_locks(args),

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

        // eval.c gap-fill
        "buffer-local-toplevel-value" => builtin_buffer_local_toplevel_value(args),
        "set-buffer-local-toplevel-value" => builtin_set_buffer_local_toplevel_value(args),
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

/// Register all builtins via defsubr — function pointer dispatch.
///
/// This replaces the giant match-by-name block in dispatch_builtin.
/// Each registered builtin is called via a direct function pointer,
/// matching GNU Emacs's defsubr/funcall_subr architecture.
pub(crate) fn init_builtins(ctx: &mut super::eval::Context) {
    use super::eval::Context;
    use super::error::*;
    use super::value::*;
        ctx.defsubr("apply", builtin_apply, 0, None);
        ctx.defsubr("funcall", builtin_funcall, 0, None);
        ctx.defsubr("funcall-interactively", builtin_funcall_interactively, 0, None);
        ctx.defsubr("funcall-with-delayed-message", builtin_funcall_with_delayed_message, 0, None);
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
        ctx.defsubr("default-toplevel-value", builtin_default_toplevel_value, 0, None);
        ctx.defsubr("fboundp", builtin_fboundp, 0, None);
        ctx.defsubr("internal-make-var-non-special", builtin_internal_make_var_non_special_eval, 0, None);
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
        ctx.defsubr("getenv-internal", super::process::builtin_getenv_internal_eval, 0, None);
        ctx.defsubr("special-variable-p", builtin_special_variable_p, 0, None);
        ctx.defsubr("intern", builtin_intern_fn, 0, None);
        ctx.defsubr("intern-soft", builtin_intern_soft, 0, None);
        ctx.defsubr("run-hook-with-args", builtin_run_hook_with_args, 0, None);
        ctx.defsubr("run-hook-with-args-until-success", builtin_run_hook_with_args_until_success, 0, None);
        ctx.defsubr("run-hook-with-args-until-failure", builtin_run_hook_with_args_until_failure, 0, None);
        ctx.defsubr("run-hook-wrapped", builtin_run_hook_wrapped, 0, None);
        ctx.defsubr("run-window-configuration-change-hook", builtin_run_window_configuration_change_hook, 0, None);
        ctx.defsubr("run-window-scroll-functions", builtin_run_window_scroll_functions, 0, None);
        ctx.defsubr("featurep", builtin_featurep, 0, None);
        ctx.defsubr("garbage-collect", builtin_garbage_collect_eval, 0, None);
        ctx.defsubr("neovm-precompile-file", builtin_neovm_precompile_file, 0, None);
        ctx.defsubr("eval", builtin_eval, 0, None);
        ctx.defsubr("get-buffer-create", builtin_get_buffer_create, 0, None);
        ctx.defsubr("get-buffer", builtin_get_buffer, 0, None);
        ctx.defsubr("make-indirect-buffer", builtin_make_indirect_buffer, 0, None);
        ctx.defsubr("find-buffer", builtin_find_buffer, 0, None);
        ctx.defsubr("buffer-live-p", builtin_buffer_live_p, 0, None);
        ctx.defsubr("barf-if-buffer-read-only", builtin_barf_if_buffer_read_only, 0, None);
        ctx.defsubr("bury-buffer-internal", builtin_bury_buffer_internal, 0, None);
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
        ctx.defsubr("buffer-line-statistics", builtin_buffer_line_statistics, 0, None);
        ctx.defsubr("buffer-text-pixel-size", builtin_buffer_text_pixel_size, 0, None);
        ctx.defsubr("base64-encode-region", super::fns::builtin_base64_encode_region_eval, 0, None);
        ctx.defsubr("base64-decode-region", super::fns::builtin_base64_decode_region_eval, 0, None);
        ctx.defsubr("base64url-encode-region", super::fns::builtin_base64url_encode_region_eval, 0, None);
        ctx.defsubr("md5", super::fns::builtin_md5_eval, 0, None);
        ctx.defsubr("secure-hash", super::fns::builtin_secure_hash_eval, 0, None);
        ctx.defsubr("buffer-hash", super::fns::builtin_buffer_hash_eval, 0, None);
        ctx.defsubr("buffer-substring", builtin_buffer_substring, 0, None);
        ctx.defsubr("compare-buffer-substrings", builtin_compare_buffer_substrings, 0, None);
        ctx.defsubr("point", builtin_point, 0, None);
        ctx.defsubr("point-min", builtin_point_min, 0, None);
        ctx.defsubr("point-max", builtin_point_max, 0, None);
        ctx.defsubr("goto-char", builtin_goto_char, 0, None);
        ctx.defsubr("field-beginning", builtin_field_beginning, 0, None);
        ctx.defsubr("field-end", builtin_field_end, 0, None);
        ctx.defsubr("field-string", builtin_field_string, 0, None);
        ctx.defsubr("field-string-no-properties", builtin_field_string_no_properties, 0, None);
        ctx.defsubr("constrain-to-field", builtin_constrain_to_field, 0, None);
        ctx.defsubr("insert", builtin_insert, 0, None);
        ctx.defsubr("insert-and-inherit", builtin_insert_and_inherit, 0, None);
        ctx.defsubr("insert-before-markers-and-inherit", builtin_insert_before_markers_and_inherit, 0, None);
        ctx.defsubr("insert-buffer-substring", builtin_insert_buffer_substring, 0, None);
        ctx.defsubr("insert-char", builtin_insert_char, 0, None);
        ctx.defsubr("insert-byte", builtin_insert_byte, 0, None);
        ctx.defsubr("replace-region-contents", builtin_replace_region_contents_eval, 0, None);
        ctx.defsubr("set-buffer-multibyte", builtin_set_buffer_multibyte_eval, 0, None);
        ctx.defsubr("kill-all-local-variables", builtin_kill_all_local_variables, 0, None);
        ctx.defsubr("buffer-swap-text", builtin_buffer_swap_text, 0, None);
        ctx.defsubr("delete-region", builtin_delete_region, 0, None);
        ctx.defsubr("delete-and-extract-region", builtin_delete_and_extract_region, 0, None);
        ctx.defsubr("subst-char-in-region", builtin_subst_char_in_region, 0, None);
        ctx.defsubr("delete-field", builtin_delete_field, 0, None);
        ctx.defsubr("delete-all-overlays", builtin_delete_all_overlays, 0, None);
        ctx.defsubr("erase-buffer", builtin_erase_buffer, 0, None);
        ctx.defsubr("buffer-enable-undo", builtin_buffer_enable_undo, 0, None);
        ctx.defsubr("buffer-size", builtin_buffer_size, 0, None);
        ctx.defsubr("narrow-to-region", builtin_narrow_to_region, 0, None);
        ctx.defsubr("widen", builtin_widen, 0, None);
        ctx.defsubr("internal--labeled-narrow-to-region", builtin_internal_labeled_narrow_to_region_eval, 0, None);
        ctx.defsubr("internal--labeled-widen", builtin_internal_labeled_widen_eval, 0, None);
        ctx.defsubr("buffer-modified-p", builtin_buffer_modified_p, 0, None);
        ctx.defsubr("set-buffer-modified-p", builtin_set_buffer_modified_p, 0, None);
        ctx.defsubr("buffer-modified-tick", builtin_buffer_modified_tick, 0, None);
        ctx.defsubr("buffer-chars-modified-tick", builtin_buffer_chars_modified_tick, 0, None);
        ctx.defsubr("buffer-list", builtin_buffer_list, 0, None);
        ctx.defsubr("other-buffer", builtin_other_buffer, 0, None);
        ctx.defsubr("generate-new-buffer-name", builtin_generate_new_buffer_name, 0, None);
        ctx.defsubr("char-after", builtin_char_after, 0, None);
        ctx.defsubr("char-before", builtin_char_before, 0, None);
        ctx.defsubr("byte-to-position", builtin_byte_to_position, 0, None);
        ctx.defsubr("position-bytes", builtin_position_bytes, 0, None);
        ctx.defsubr("get-byte", builtin_get_byte, 0, None);
        ctx.defsubr("buffer-local-value", builtin_buffer_local_value, 0, None);
        ctx.defsubr("local-variable-if-set-p", builtin_local_variable_if_set_p_eval, 0, None);
        ctx.defsubr("variable-binding-locus", builtin_variable_binding_locus_eval, 0, None);
        ctx.defsubr("interactive-form", builtin_interactive_form_eval, 0, None);
        ctx.defsubr("command-modes", super::interactive::builtin_command_modes_eval, 0, None);
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
        ctx.defsubr("match-data--translate", builtin_match_data_translate_eval, 0, None);
        ctx.defsubr("set-match-data", builtin_set_match_data_eval, 0, None);
        ctx.defsubr("replace-match", builtin_replace_match, 0, None);
        ctx.defsubr("find-charset-region", super::charset::builtin_find_charset_region_eval, 0, None);
        ctx.defsubr("charset-after", super::charset::builtin_charset_after_eval, 0, None);
        ctx.defsubr("format-mode-line", super::xdisp::builtin_format_mode_line_eval, 0, None);
        ctx.defsubr("window-line-height", super::xdisp::builtin_window_line_height_eval, 0, None);
        ctx.defsubr("posn-at-point", super::xdisp::builtin_posn_at_point_eval, 0, None);
        ctx.defsubr("posn-at-x-y", super::xdisp::builtin_posn_at_x_y_eval, 0, None);
        ctx.defsubr("coordinates-in-window-p", builtin_coordinates_in_window_p, 0, None);
        ctx.defsubr("tool-bar-height", super::xdisp::builtin_tool_bar_height_eval, 0, None);
        ctx.defsubr("tab-bar-height", super::xdisp::builtin_tab_bar_height_eval, 0, None);
        ctx.defsubr("list-fonts", super::font::builtin_list_fonts_eval, 0, None);
        ctx.defsubr("find-font", super::font::builtin_find_font_eval, 0, None);
        ctx.defsubr("font-family-list", super::font::builtin_font_family_list_eval, 0, None);
        ctx.defsubr("font-info", super::font::builtin_font_info_eval, 0, None);
        ctx.defsubr("new-fontset", builtin_new_fontset_eval, 0, None);
        ctx.defsubr("set-fontset-font", builtin_set_fontset_font_eval, 0, None);
        ctx.defsubr("insert-file-contents", super::fileio::builtin_insert_file_contents, 0, None);
        ctx.defsubr("write-region", super::fileio::builtin_write_region, 0, None);
        ctx.defsubr("file-name-completion", super::dired::builtin_file_name_completion_eval, 0, None);
        ctx.defsubr("set-visited-file-modtime", super::fileio::builtin_set_visited_file_modtime, 0, None);
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
        ctx.defsubr("current-minor-mode-maps", builtin_current_minor_mode_maps, 0, None);
        ctx.defsubr("keymap-parent", builtin_keymap_parent, 0, None);
        ctx.defsubr("set-keymap-parent", builtin_set_keymap_parent, 0, None);
        ctx.defsubr("keymapp", builtin_keymapp, 0, None);
        ctx.defsubr("accessible-keymaps", builtin_accessible_keymaps, 0, None);
        ctx.defsubr("map-keymap", builtin_map_keymap, 0, None);
        ctx.defsubr("map-keymap-internal", builtin_map_keymap_internal, 0, None);
        ctx.defsubr("print--preprocess", super::process::builtin_print_preprocess, 0, None);
        ctx.defsubr("format-network-address", super::process::builtin_format_network_address, 0, None);
        ctx.defsubr("network-interface-list", super::process::builtin_network_interface_list, 0, None);
        ctx.defsubr("network-interface-info", super::process::builtin_network_interface_info, 0, None);
        ctx.defsubr("signal-names", super::process::builtin_signal_names, 0, None);
        ctx.defsubr("accept-process-output", super::process::builtin_accept_process_output, 0, None);
        ctx.defsubr("list-system-processes", super::process::builtin_list_system_processes, 0, None);
        ctx.defsubr("num-processors", super::process::builtin_num_processors, 0, None);
        ctx.defsubr("make-process", super::process::builtin_make_process, 0, None);
        ctx.defsubr("make-network-process", super::process::builtin_make_network_process, 0, None);
        ctx.defsubr("make-pipe-process", super::process::builtin_make_pipe_process, 0, None);
        ctx.defsubr("gnutls-boot", super::process::builtin_gnutls_boot, 0, None);
        ctx.defsubr("make-serial-process", super::process::builtin_make_serial_process, 0, None);
        ctx.defsubr("serial-process-configure", super::process::builtin_serial_process_configure, 0, None);
        ctx.defsubr("call-process", super::process::builtin_call_process, 0, None);
        ctx.defsubr("call-process-region", super::process::builtin_call_process_region, 0, None);
        ctx.defsubr("continue-process", super::process::builtin_continue_process, 0, None);
        ctx.defsubr("delete-process", super::process::builtin_delete_process, 0, None);
        ctx.defsubr("interrupt-process", super::process::builtin_interrupt_process, 0, None);
        ctx.defsubr("kill-process", super::process::builtin_kill_process, 0, None);
        ctx.defsubr("quit-process", super::process::builtin_quit_process, 0, None);
        ctx.defsubr("signal-process", super::process::builtin_signal_process, 0, None);
        ctx.defsubr("stop-process", super::process::builtin_stop_process, 0, None);
        ctx.defsubr("get-process", super::process::builtin_get_process, 0, None);
        ctx.defsubr("get-buffer-process", super::process::builtin_get_buffer_process, 0, None);
        ctx.defsubr("process-attributes", super::process::builtin_process_attributes, 0, None);
        ctx.defsubr("processp", super::process::builtin_processp, 0, None);
        ctx.defsubr("process-id", super::process::builtin_process_id, 0, None);
        ctx.defsubr("process-command", super::process::builtin_process_command, 0, None);
        ctx.defsubr("process-contact", super::process::builtin_process_contact, 0, None);
        ctx.defsubr("process-filter", super::process::builtin_process_filter, 0, None);
        ctx.defsubr("set-process-filter", super::process::builtin_set_process_filter, 0, None);
        ctx.defsubr("process-sentinel", super::process::builtin_process_sentinel, 0, None);
        ctx.defsubr("set-process-sentinel", super::process::builtin_set_process_sentinel, 0, None);
        ctx.defsubr("process-coding-system", super::process::builtin_process_coding_system, 0, None);
        ctx.defsubr("process-datagram-address", super::process::builtin_process_datagram_address, 0, None);
        ctx.defsubr("set-process-buffer", super::process::builtin_set_process_buffer, 0, None);
        ctx.defsubr("set-process-thread", super::process::builtin_set_process_thread, 0, None);
        ctx.defsubr("set-process-window-size", super::process::builtin_set_process_window_size, 0, None);
        ctx.defsubr("process-tty-name", super::process::builtin_process_tty_name, 0, None);
        ctx.defsubr("process-plist", super::process::builtin_process_plist, 0, None);
        ctx.defsubr("set-process-plist", super::process::builtin_set_process_plist, 0, None);
        ctx.defsubr("process-mark", super::process::builtin_process_mark, 0, None);
        ctx.defsubr("process-type", super::process::builtin_process_type, 0, None);
        ctx.defsubr("process-thread", super::process::builtin_process_thread, 0, None);
        ctx.defsubr("process-running-child-p", super::process::builtin_process_running_child_p, 0, None);
        ctx.defsubr("process-send-region", super::process::builtin_process_send_region, 0, None);
        ctx.defsubr("process-send-eof", super::process::builtin_process_send_eof, 0, None);
        ctx.defsubr("process-send-string", super::process::builtin_process_send_string, 0, None);
        ctx.defsubr("process-status", super::process::builtin_process_status, 0, None);
        ctx.defsubr("process-exit-status", super::process::builtin_process_exit_status, 0, None);
        ctx.defsubr("process-list", super::process::builtin_process_list, 0, None);
        ctx.defsubr("process-name", super::process::builtin_process_name, 0, None);
        ctx.defsubr("process-buffer", super::process::builtin_process_buffer, 0, None);
        ctx.defsubr("sleep-for", super::timer::builtin_sleep_for, 0, None);
        ctx.defsubr("add-variable-watcher", super::advice::builtin_add_variable_watcher, 0, None);
        ctx.defsubr("remove-variable-watcher", super::advice::builtin_remove_variable_watcher, 0, None);
        ctx.defsubr("get-variable-watchers", super::advice::builtin_get_variable_watchers, 0, None);
        ctx.defsubr("modify-syntax-entry", super::syntax::builtin_modify_syntax_entry, 0, None);
        ctx.defsubr("syntax-table", super::syntax::builtin_syntax_table, 0, None);
        ctx.defsubr("set-syntax-table", super::syntax::builtin_set_syntax_table, 0, None);
        ctx.defsubr("char-syntax", super::syntax::builtin_char_syntax, 0, None);
        ctx.defsubr("matching-paren", super::syntax::builtin_matching_paren_eval, 0, None);
        ctx.defsubr("forward-comment", super::syntax::builtin_forward_comment, 0, None);
        ctx.defsubr("backward-prefix-chars", super::syntax::builtin_backward_prefix_chars, 0, None);
        ctx.defsubr("forward-word", super::syntax::builtin_forward_word, 0, None);
        ctx.defsubr("scan-lists", super::syntax::builtin_scan_lists, 0, None);
        ctx.defsubr("scan-sexps", super::syntax::builtin_scan_sexps, 0, None);
        ctx.defsubr("parse-partial-sexp", super::syntax::builtin_parse_partial_sexp, 0, None);
        ctx.defsubr("skip-syntax-forward", super::syntax::builtin_skip_syntax_forward, 0, None);
        ctx.defsubr("skip-syntax-backward", super::syntax::builtin_skip_syntax_backward, 0, None);
        ctx.defsubr("start-kbd-macro", super::kmacro::builtin_start_kbd_macro, 0, None);
        ctx.defsubr("end-kbd-macro", super::kmacro::builtin_end_kbd_macro, 0, None);
        ctx.defsubr("call-last-kbd-macro", super::kmacro::builtin_call_last_kbd_macro, 0, None);
        ctx.defsubr("execute-kbd-macro", super::kmacro::builtin_execute_kbd_macro, 0, None);
        ctx.defsubr("store-kbd-macro-event", super::kmacro::builtin_store_kbd_macro_event, 0, None);
        ctx.defsubr("put-text-property", super::textprop::builtin_put_text_property, 0, None);
        ctx.defsubr("get-text-property", super::textprop::builtin_get_text_property, 0, None);
        ctx.defsubr("get-char-property", super::textprop::builtin_get_char_property, 0, None);
        ctx.defsubr("get-pos-property", builtin_get_pos_property, 0, None);
        ctx.defsubr("add-face-text-property", super::textprop::builtin_add_face_text_property, 0, None);
        ctx.defsubr("add-text-properties", super::textprop::builtin_add_text_properties, 0, None);
        ctx.defsubr("set-text-properties", super::textprop::builtin_set_text_properties, 0, None);
        ctx.defsubr("remove-text-properties", super::textprop::builtin_remove_text_properties, 0, None);
        ctx.defsubr("text-properties-at", super::textprop::builtin_text_properties_at, 0, None);
        ctx.defsubr("get-display-property", super::textprop::builtin_get_display_property, 0, None);
        ctx.defsubr("next-single-char-property-change", builtin_next_single_char_property_change, 0, None);
        ctx.defsubr("previous-single-char-property-change", builtin_previous_single_char_property_change, 0, None);
        ctx.defsubr("next-property-change", super::textprop::builtin_next_property_change, 0, None);
        ctx.defsubr("next-char-property-change", builtin_next_char_property_change, 0, None);
        ctx.defsubr("previous-property-change", builtin_previous_property_change, 0, None);
        ctx.defsubr("previous-char-property-change", builtin_previous_char_property_change, 0, None);
        ctx.defsubr("text-property-any", super::textprop::builtin_text_property_any, 0, None);
        ctx.defsubr("text-property-not-all", super::textprop::builtin_text_property_not_all, 0, None);
        ctx.defsubr("next-overlay-change", super::textprop::builtin_next_overlay_change, 0, None);
        ctx.defsubr("previous-overlay-change", super::textprop::builtin_previous_overlay_change, 0, None);
        ctx.defsubr("make-overlay", super::textprop::builtin_make_overlay, 0, None);
        ctx.defsubr("delete-overlay", super::textprop::builtin_delete_overlay, 0, None);
        ctx.defsubr("overlay-put", super::textprop::builtin_overlay_put, 0, None);
        ctx.defsubr("overlay-get", super::textprop::builtin_overlay_get, 0, None);
        ctx.defsubr("overlays-at", super::textprop::builtin_overlays_at, 0, None);
        ctx.defsubr("overlays-in", super::textprop::builtin_overlays_in, 0, None);
        ctx.defsubr("move-overlay", super::textprop::builtin_move_overlay, 0, None);
        ctx.defsubr("overlay-start", super::textprop::builtin_overlay_start, 0, None);
        ctx.defsubr("overlay-end", super::textprop::builtin_overlay_end, 0, None);
        ctx.defsubr("overlay-buffer", super::textprop::builtin_overlay_buffer, 0, None);
        ctx.defsubr("overlay-properties", super::textprop::builtin_overlay_properties, 0, None);
        ctx.defsubr("overlayp", super::textprop::builtin_overlayp, 0, None);
        ctx.defsubr("bobp", super::navigation::builtin_bobp, 0, None);
        ctx.defsubr("eobp", super::navigation::builtin_eobp, 0, None);
        ctx.defsubr("bolp", super::navigation::builtin_bolp, 0, None);
        ctx.defsubr("eolp", super::navigation::builtin_eolp, 0, None);
        ctx.defsubr("pos-bol", builtin_pos_bol, 0, None);
        ctx.defsubr("line-end-position", super::navigation::builtin_line_end_position, 0, None);
        ctx.defsubr("pos-eol", builtin_pos_eol, 0, None);
        ctx.defsubr("line-number-at-pos", super::navigation::builtin_line_number_at_pos, 0, None);
        ctx.defsubr("forward-line", super::navigation::builtin_forward_line, 0, None);
        ctx.defsubr("beginning-of-line", super::navigation::builtin_beginning_of_line, 0, None);
        ctx.defsubr("end-of-line", super::navigation::builtin_end_of_line, 0, None);
        ctx.defsubr("forward-char", super::navigation::builtin_forward_char, 0, None);
        ctx.defsubr("backward-char", super::navigation::builtin_backward_char, 0, None);
        ctx.defsubr("skip-chars-forward", super::navigation::builtin_skip_chars_forward, 0, None);
        ctx.defsubr("skip-chars-backward", super::navigation::builtin_skip_chars_backward, 0, None);
        ctx.defsubr("mark-marker", super::marker::builtin_mark_marker, 0, None);
        ctx.defsubr("region-beginning", super::navigation::builtin_region_beginning, 0, None);
        ctx.defsubr("region-end", super::navigation::builtin_region_end, 0, None);
        ctx.defsubr("transient-mark-mode", super::navigation::builtin_transient_mark_mode, 0, None);
        ctx.defsubr("make-local-variable", super::custom::builtin_make_local_variable, 0, None);
        ctx.defsubr("local-variable-p", super::custom::builtin_local_variable_p, 0, None);
        ctx.defsubr("buffer-local-variables", super::custom::builtin_buffer_local_variables, 0, None);
        ctx.defsubr("kill-local-variable", super::custom::builtin_kill_local_variable, 0, None);
        ctx.defsubr("default-value", super::custom::builtin_default_value, 0, None);
        ctx.defsubr("set-default", super::custom::builtin_set_default, 0, None);
        ctx.defsubr("set-default-toplevel-value", builtin_set_default_toplevel_value, 0, None);
        ctx.defsubr("autoload", super::autoload::builtin_autoload, 0, None);
        ctx.defsubr("autoload-do-load", super::autoload::builtin_autoload_do_load, 0, None);
        ctx.defsubr("symbol-file", super::autoload::builtin_symbol_file_eval, 0, None);
        ctx.defsubr("downcase-region", super::casefiddle::builtin_downcase_region, 0, None);
        ctx.defsubr("upcase-region", super::casefiddle::builtin_upcase_region, 0, None);
        ctx.defsubr("capitalize-region", super::casefiddle::builtin_capitalize_region, 0, None);
        ctx.defsubr("downcase-word", super::casefiddle::builtin_downcase_word, 0, None);
        ctx.defsubr("upcase-word", super::casefiddle::builtin_upcase_word, 0, None);
        ctx.defsubr("capitalize-word", super::casefiddle::builtin_capitalize_word, 0, None);
        ctx.defsubr("indent-to", super::indent::builtin_indent_to_eval, 0, None);
        ctx.defsubr("selected-window", super::window_cmds::builtin_selected_window, 0, None);
        ctx.defsubr("old-selected-window", super::window_cmds::builtin_old_selected_window, 0, None);
        ctx.defsubr("minibuffer-window", super::window_cmds::builtin_minibuffer_window, 0, None);
        ctx.defsubr("window-parameter", super::window_cmds::builtin_window_parameter, 0, None);
        ctx.defsubr("set-window-parameter", super::window_cmds::builtin_set_window_parameter, 0, None);
        ctx.defsubr("window-parameters", super::window_cmds::builtin_window_parameters, 0, None);
        ctx.defsubr("window-parent", super::window_cmds::builtin_window_parent, 0, None);
        ctx.defsubr("window-top-child", super::window_cmds::builtin_window_top_child, 0, None);
        ctx.defsubr("window-left-child", super::window_cmds::builtin_window_left_child, 0, None);
        ctx.defsubr("window-next-sibling", super::window_cmds::builtin_window_next_sibling, 0, None);
        ctx.defsubr("window-prev-sibling", super::window_cmds::builtin_window_prev_sibling, 0, None);
        ctx.defsubr("window-normal-size", super::window_cmds::builtin_window_normal_size, 0, None);
        ctx.defsubr("window-display-table", super::window_cmds::builtin_window_display_table, 0, None);
        ctx.defsubr("window-cursor-type", super::window_cmds::builtin_window_cursor_type, 0, None);
        ctx.defsubr("window-buffer", super::window_cmds::builtin_window_buffer, 0, None);
        ctx.defsubr("window-start", super::window_cmds::builtin_window_start, 0, None);
        ctx.defsubr("window-end", super::window_cmds::builtin_window_end, 0, None);
        ctx.defsubr("window-point", super::window_cmds::builtin_window_point, 0, None);
        ctx.defsubr("window-use-time", super::window_cmds::builtin_window_use_time, 0, None);
        ctx.defsubr("window-bump-use-time", super::window_cmds::builtin_window_bump_use_time, 0, None);
        ctx.defsubr("window-old-point", super::window_cmds::builtin_window_old_point, 0, None);
        ctx.defsubr("window-old-buffer", super::window_cmds::builtin_window_old_buffer, 0, None);
        ctx.defsubr("window-prev-buffers", super::window_cmds::builtin_window_prev_buffers, 0, None);
        ctx.defsubr("window-next-buffers", super::window_cmds::builtin_window_next_buffers, 0, None);
        ctx.defsubr("window-left-column", super::window_cmds::builtin_window_left_column, 0, None);
        ctx.defsubr("window-top-line", super::window_cmds::builtin_window_top_line, 0, None);
        ctx.defsubr("window-pixel-left", super::window_cmds::builtin_window_pixel_left, 0, None);
        ctx.defsubr("window-pixel-top", super::window_cmds::builtin_window_pixel_top, 0, None);
        ctx.defsubr("window-hscroll", super::window_cmds::builtin_window_hscroll, 0, None);
        ctx.defsubr("window-vscroll", super::window_cmds::builtin_window_vscroll, 0, None);
        ctx.defsubr("window-margins", super::window_cmds::builtin_window_margins, 0, None);
        ctx.defsubr("window-fringes", super::window_cmds::builtin_window_fringes, 0, None);
        ctx.defsubr("window-scroll-bars", super::window_cmds::builtin_window_scroll_bars, 0, None);
        ctx.defsubr("window-pixel-height", super::window_cmds::builtin_window_pixel_height, 0, None);
        ctx.defsubr("window-pixel-width", super::window_cmds::builtin_window_pixel_width, 0, None);
        ctx.defsubr("window-body-height", super::window_cmds::builtin_window_body_height, 0, None);
        ctx.defsubr("window-body-width", super::window_cmds::builtin_window_body_width, 0, None);
        ctx.defsubr("window-text-height", super::window_cmds::builtin_window_text_height, 0, None);
        ctx.defsubr("window-text-width", super::window_cmds::builtin_window_text_width, 0, None);
        ctx.defsubr("window-total-height", super::window_cmds::builtin_window_total_height, 0, None);
        ctx.defsubr("window-total-width", super::window_cmds::builtin_window_total_width, 0, None);
        ctx.defsubr("window-list", super::window_cmds::builtin_window_list, 0, None);
        ctx.defsubr("window-list-1", super::window_cmds::builtin_window_list_1, 0, None);
        ctx.defsubr("get-buffer-window", super::window_cmds::builtin_get_buffer_window, 0, None);
        ctx.defsubr("window-dedicated-p", super::window_cmds::builtin_window_dedicated_p, 0, None);
        ctx.defsubr("window-minibuffer-p", super::window_cmds::builtin_window_minibuffer_p, 0, None);
        ctx.defsubr("window-at", super::window_cmds::builtin_window_at, 0, None);
        ctx.defsubr("window-live-p", super::window_cmds::builtin_window_live_p, 0, None);
        ctx.defsubr("set-window-start", super::window_cmds::builtin_set_window_start, 0, None);
        ctx.defsubr("set-window-hscroll", super::window_cmds::builtin_set_window_hscroll, 0, None);
        ctx.defsubr("set-window-margins", super::window_cmds::builtin_set_window_margins, 0, None);
        ctx.defsubr("set-window-fringes", super::window_cmds::builtin_set_window_fringes, 0, None);
        ctx.defsubr("set-window-vscroll", super::window_cmds::builtin_set_window_vscroll, 0, None);
        ctx.defsubr("set-window-point", super::window_cmds::builtin_set_window_point, 0, None);
        ctx.defsubr("split-window-internal", builtin_split_window_internal, 0, None);
        ctx.defsubr("delete-window", super::window_cmds::builtin_delete_window, 0, None);
        ctx.defsubr("delete-other-windows", super::window_cmds::builtin_delete_other_windows, 0, None);
        ctx.defsubr("select-window", super::window_cmds::builtin_select_window, 0, None);
        ctx.defsubr("scroll-up", super::window_cmds::builtin_scroll_up, 0, None);
        ctx.defsubr("scroll-down", super::window_cmds::builtin_scroll_down, 0, None);
        ctx.defsubr("scroll-left", super::window_cmds::builtin_scroll_left, 0, None);
        ctx.defsubr("scroll-right", super::window_cmds::builtin_scroll_right, 0, None);
        ctx.defsubr("window-resize-apply", super::window_cmds::builtin_window_resize_apply, 0, None);
        ctx.defsubr("recenter", super::window_cmds::builtin_recenter, 0, None);
        ctx.defsubr("vertical-motion", builtin_vertical_motion, 0, None);
        ctx.defsubr("next-window", super::window_cmds::builtin_next_window, 0, None);
        ctx.defsubr("previous-window", super::window_cmds::builtin_previous_window, 0, None);
        ctx.defsubr("set-window-buffer", super::window_cmds::builtin_set_window_buffer, 0, None);
        ctx.defsubr("current-window-configuration", builtin_current_window_configuration, 0, None);
        ctx.defsubr("set-window-configuration", builtin_set_window_configuration, 0, None);
        ctx.defsubr("old-selected-frame", builtin_old_selected_frame_eval, 0, None);
        ctx.defsubr("selected-frame", super::window_cmds::builtin_selected_frame, 0, None);
        ctx.defsubr("mouse-pixel-position", builtin_mouse_pixel_position_eval, 0, None);
        ctx.defsubr("mouse-position", builtin_mouse_position_eval, 0, None);
        ctx.defsubr("next-frame", builtin_next_frame_eval, 0, None);
        ctx.defsubr("previous-frame", builtin_previous_frame_eval, 0, None);
        ctx.defsubr("select-frame", super::window_cmds::builtin_select_frame, 0, None);
        ctx.defsubr("last-nonminibuffer-frame", super::window_cmds::builtin_selected_frame, 0, None);
        ctx.defsubr("visible-frame-list", super::window_cmds::builtin_visible_frame_list, 0, None);
        ctx.defsubr("frame-list", super::window_cmds::builtin_frame_list, 0, None);
        ctx.defsubr("x-create-frame", super::window_cmds::builtin_x_create_frame, 0, None);
        ctx.defsubr("make-frame-visible", super::window_cmds::builtin_make_frame_visible, 0, None);
        ctx.defsubr("make-frame", super::window_cmds::builtin_make_frame, 0, None);
        ctx.defsubr("iconify-frame", super::window_cmds::builtin_iconify_frame, 0, None);
        ctx.defsubr("delete-frame", super::window_cmds::builtin_delete_frame, 0, None);
        ctx.defsubr("frame-char-height", super::window_cmds::builtin_frame_char_height, 0, None);
        ctx.defsubr("frame-char-width", super::window_cmds::builtin_frame_char_width, 0, None);
        ctx.defsubr("frame-native-height", super::window_cmds::builtin_frame_native_height, 0, None);
        ctx.defsubr("frame-native-width", super::window_cmds::builtin_frame_native_width, 0, None);
        ctx.defsubr("frame-text-cols", super::window_cmds::builtin_frame_text_cols, 0, None);
        ctx.defsubr("frame-text-height", super::window_cmds::builtin_frame_text_height, 0, None);
        ctx.defsubr("frame-text-lines", super::window_cmds::builtin_frame_text_lines, 0, None);
        ctx.defsubr("frame-text-width", super::window_cmds::builtin_frame_text_width, 0, None);
        ctx.defsubr("frame-total-cols", super::window_cmds::builtin_frame_total_cols, 0, None);
        ctx.defsubr("frame-total-lines", super::window_cmds::builtin_frame_total_lines, 0, None);
        ctx.defsubr("frame-position", super::window_cmds::builtin_frame_position, 0, None);
        ctx.defsubr("frame-parameters", super::window_cmds::builtin_frame_parameters, 0, None);
        ctx.defsubr("set-frame-height", super::window_cmds::builtin_set_frame_height, 0, None);
        ctx.defsubr("set-frame-width", super::window_cmds::builtin_set_frame_width, 0, None);
        ctx.defsubr("set-frame-size", super::window_cmds::builtin_set_frame_size, 0, None);
        ctx.defsubr("set-frame-position", super::window_cmds::builtin_set_frame_position, 0, None);
        ctx.defsubr("frame-visible-p", super::window_cmds::builtin_frame_visible_p, 0, None);
        ctx.defsubr("frame-live-p", super::window_cmds::builtin_frame_live_p, 0, None);
        ctx.defsubr("frame-first-window", super::window_cmds::builtin_frame_first_window, 0, None);
        ctx.defsubr("frame-root-window", super::window_cmds::builtin_frame_root_window, 0, None);
        ctx.defsubr("windowp", super::window_cmds::builtin_windowp, 0, None);
        ctx.defsubr("window-valid-p", super::window_cmds::builtin_window_valid_p, 0, None);
        ctx.defsubr("window-height", super::window_cmds::builtin_window_height, 0, None);
        ctx.defsubr("window-width", super::window_cmds::builtin_window_width, 0, None);
        ctx.defsubr("framep", super::window_cmds::builtin_framep, 0, None);
        ctx.defsubr("window-frame", super::window_cmds::builtin_window_frame, 0, None);
        ctx.defsubr("frame-id", builtin_frame_id_eval, 0, None);
        ctx.defsubr("frame-root-frame", builtin_frame_root_frame_eval, 0, None);
        ctx.defsubr("x-open-connection", super::display::builtin_x_open_connection_eval, 0, None);
        ctx.defsubr("x-get-resource", super::display::builtin_x_get_resource_eval, 0, None);
        ctx.defsubr("x-list-fonts", super::display::builtin_x_list_fonts_eval, 0, None);
        ctx.defsubr("window-system", super::display::builtin_window_system_eval, 0, None);
        ctx.defsubr("current-idle-time", builtin_current_idle_time_eval, 0, None);
        ctx.defsubr("x-server-version", super::display::builtin_x_server_version_eval, 0, None);
        ctx.defsubr("x-server-input-extension-version", super::display::builtin_x_server_input_extension_version_eval, 0, None);
        ctx.defsubr("x-server-vendor", super::display::builtin_x_server_vendor_eval, 0, None);
        ctx.defsubr("display-color-cells", super::display::builtin_display_color_cells_eval, 0, None);
        ctx.defsubr("x-display-mm-height", super::display::builtin_x_display_mm_height_eval, 0, None);
        ctx.defsubr("x-display-mm-width", super::display::builtin_x_display_mm_width_eval, 0, None);
        ctx.defsubr("x-display-planes", super::display::builtin_x_display_planes_eval, 0, None);
        ctx.defsubr("x-display-screens", super::display::builtin_x_display_screens_eval, 0, None);
        ctx.defsubr("x-close-connection", super::display::builtin_x_close_connection_eval, 0, None);
        ctx.defsubr("call-interactively", super::interactive::builtin_call_interactively, 0, None);
        ctx.defsubr("commandp", super::interactive::builtin_commandp_interactive, 0, None);
        ctx.defsubr("command-remapping", super::interactive::builtin_command_remapping, 0, None);
        ctx.defsubr("self-insert-command", super::interactive::builtin_self_insert_command, 0, None);
        ctx.defsubr("key-binding", super::interactive::builtin_key_binding, 0, None);
        ctx.defsubr("where-is-internal", super::interactive::builtin_where_is_internal, 0, None);
        ctx.defsubr("this-command-keys", super::interactive::builtin_this_command_keys, 0, None);
        ctx.defsubr("format", builtin_format_eval, 0, None);
        ctx.defsubr("format-message", builtin_format_message_eval, 0, None);
        ctx.defsubr("message-box", builtin_message_box_eval, 0, None);
        ctx.defsubr("message-or-box", builtin_message_or_box_eval, 0, None);
        ctx.defsubr("current-message", builtin_current_message_eval, 0, None);
        ctx.defsubr("read-from-string", super::reader::builtin_read_from_string, 0, None);
        ctx.defsubr("read", super::reader::builtin_read, 0, None);
        ctx.defsubr("read-from-minibuffer", super::reader::builtin_read_from_minibuffer, 0, None);
        ctx.defsubr("read-string", super::reader::builtin_read_string, 0, None);
        ctx.defsubr("completing-read", super::reader::builtin_completing_read, 0, None);
        ctx.defsubr("read-buffer", super::minibuffer::builtin_read_buffer, 0, None);
        ctx.defsubr("read-command", super::minibuffer::builtin_read_command, 0, None);
        ctx.defsubr("read-variable", super::minibuffer::builtin_read_variable, 0, None);
        ctx.defsubr("try-completion", super::minibuffer::builtin_try_completion_eval, 0, None);
        ctx.defsubr("all-completions", super::minibuffer::builtin_all_completions_eval, 0, None);
        ctx.defsubr("test-completion", super::minibuffer::builtin_test_completion_eval, 0, None);
        ctx.defsubr("input-pending-p", super::reader::builtin_input_pending_p, 0, None);
        ctx.defsubr("discard-input", super::reader::builtin_discard_input, 0, None);
        ctx.defsubr("current-input-mode", super::reader::builtin_current_input_mode, 0, None);
        ctx.defsubr("set-input-mode", super::reader::builtin_set_input_mode, 0, None);
        ctx.defsubr("set-input-interrupt-mode", super::reader::builtin_set_input_interrupt_mode, 0, None);
        ctx.defsubr("read-key-sequence", super::reader::builtin_read_key_sequence, 0, None);
        ctx.defsubr("read-key-sequence-vector", super::reader::builtin_read_key_sequence_vector, 0, None);
        ctx.defsubr("recent-keys", builtin_recent_keys, 0, None);
        ctx.defsubr("minibufferp", super::minibuffer::builtin_minibufferp_eval, 0, None);
        ctx.defsubr("minibuffer-contents", super::minibuffer::builtin_minibuffer_contents, 0, None);
        ctx.defsubr("minibuffer-contents-no-properties", super::minibuffer::builtin_minibuffer_contents_no_properties, 0, None);
        ctx.defsubr("minibuffer-depth", super::minibuffer::builtin_minibuffer_depth_eval, 0, None);
        ctx.defsubr("princ", builtin_princ_eval, 0, None);
        ctx.defsubr("prin1", builtin_prin1_eval, 0, None);
        ctx.defsubr("prin1-to-string", builtin_prin1_to_string_eval, 0, None);
        ctx.defsubr("print", builtin_print_eval, 0, None);
        ctx.defsubr("terpri", builtin_terpri_eval, 0, None);
        ctx.defsubr("write-char", builtin_write_char_eval, 0, None);
        ctx.defsubr("backtrace--locals", super::misc::builtin_backtrace_locals, 0, None);
        ctx.defsubr("backtrace-debug", super::misc::builtin_backtrace_debug, 0, None);
        ctx.defsubr("backtrace-eval", super::misc::builtin_backtrace_eval, 0, None);
        ctx.defsubr("backtrace-frame--internal", super::misc::builtin_backtrace_frame_internal, 0, None);
        ctx.defsubr("recursion-depth", super::misc::builtin_recursion_depth, 0, None);
        ctx.defsubr("kill-emacs", builtin_kill_emacs_eval, 0, None);
        ctx.defsubr("exit-recursive-edit", super::minibuffer::builtin_exit_recursive_edit, 0, None);
        ctx.defsubr("abort-recursive-edit", super::minibuffer::builtin_abort_recursive_edit, 0, None);
        ctx.defsubr("make-thread", super::threads::builtin_make_thread, 0, None);
        ctx.defsubr("thread-join", super::threads::builtin_thread_join, 0, None);
        ctx.defsubr("thread-yield", super::threads::builtin_thread_yield, 0, None);
        ctx.defsubr("thread-name", super::threads::builtin_thread_name, 0, None);
        ctx.defsubr("thread-live-p", super::threads::builtin_thread_live_p, 0, None);
        ctx.defsubr("threadp", super::threads::builtin_threadp, 0, None);
        ctx.defsubr("thread-signal", super::threads::builtin_thread_signal, 0, None);
        ctx.defsubr("current-thread", super::threads::builtin_current_thread, 0, None);
        ctx.defsubr("all-threads", super::threads::builtin_all_threads, 0, None);
        ctx.defsubr("thread-last-error", super::threads::builtin_thread_last_error, 0, None);
        ctx.defsubr("make-mutex", super::threads::builtin_make_mutex, 0, None);
        ctx.defsubr("mutex-name", super::threads::builtin_mutex_name, 0, None);
        ctx.defsubr("mutex-lock", super::threads::builtin_mutex_lock, 0, None);
        ctx.defsubr("mutex-unlock", super::threads::builtin_mutex_unlock, 0, None);
        ctx.defsubr("mutexp", super::threads::builtin_mutexp, 0, None);
        ctx.defsubr("make-condition-variable", super::threads::builtin_make_condition_variable, 0, None);
        ctx.defsubr("condition-variable-p", super::threads::builtin_condition_variable_p, 0, None);
        ctx.defsubr("condition-name", super::threads::builtin_condition_name, 0, None);
        ctx.defsubr("condition-mutex", super::threads::builtin_condition_mutex, 0, None);
        ctx.defsubr("condition-wait", super::threads::builtin_condition_wait, 0, None);
        ctx.defsubr("condition-notify", super::threads::builtin_condition_notify, 0, None);
        ctx.defsubr("undo-boundary", super::undo::builtin_undo_boundary_eval, 0, None);
        ctx.defsubr("maphash", super::hashtab::builtin_maphash, 0, None);
        ctx.defsubr("mapatoms", super::hashtab::builtin_mapatoms, 0, None);
        ctx.defsubr("unintern", super::hashtab::builtin_unintern, 0, None);
        ctx.defsubr("set-marker", super::marker::builtin_set_marker, 0, None);
        ctx.defsubr("move-marker", super::marker::builtin_move_marker, 0, None);
        ctx.defsubr("marker-position", super::marker::builtin_marker_position_eval, 0, None);
        ctx.defsubr("marker-buffer", super::marker::builtin_marker_buffer_eval, 0, None);
        ctx.defsubr("copy-marker", super::marker::builtin_copy_marker_eval, 0, None);
        ctx.defsubr("point-marker", super::marker::builtin_point_marker, 0, None);
        ctx.defsubr("point-min-marker", super::marker::builtin_point_min_marker, 0, None);
        ctx.defsubr("point-max-marker", super::marker::builtin_point_max_marker, 0, None);
        ctx.defsubr("current-case-table", super::casetab::builtin_current_case_table_eval, 0, None);
        ctx.defsubr("standard-case-table", super::casetab::builtin_standard_case_table_eval, 0, None);
        ctx.defsubr("set-case-table", super::casetab::builtin_set_case_table_eval, 0, None);
        ctx.defsubr("define-category", super::category::builtin_define_category_eval, 0, None);
        ctx.defsubr("category-docstring", super::category::builtin_category_docstring_eval, 0, None);
        ctx.defsubr("modify-category-entry", super::category::builtin_modify_category_entry, 0, None);
        ctx.defsubr("char-category-set", super::category::builtin_char_category_set, 0, None);
        ctx.defsubr("category-table", super::category::builtin_category_table_eval, 0, None);
        ctx.defsubr("set-category-table", super::category::builtin_set_category_table_eval, 0, None);
        ctx.defsubr("map-char-table", super::chartable::builtin_map_char_table, 0, None);
        ctx.defsubr("assoc", builtin_assoc_eval, 0, None);
        ctx.defsubr("plist-member", builtin_plist_member, 0, None);
        ctx.defsubr("json-parse-buffer", super::json::builtin_json_parse_buffer, 0, None);
        ctx.defsubr("json-insert", super::json::builtin_json_insert, 0, None);
        ctx.defsubr("documentation", super::doc::builtin_documentation, 0, None);
        ctx.defsubr("documentation-property", super::doc::builtin_documentation_property_eval, 0, None);
        ctx.defsubr("current-indentation", super::indent::builtin_current_indentation_eval, 0, None);
        ctx.defsubr("current-column", super::indent::builtin_current_column_eval, 0, None);
        ctx.defsubr("move-to-column", super::indent::builtin_move_to_column_eval, 0, None);
        ctx.defsubr("eval-buffer", super::lread::builtin_eval_buffer, 0, None);
        ctx.defsubr("eval-region", super::lread::builtin_eval_region, 0, None);
        ctx.defsubr("read-char-exclusive", super::lread::builtin_read_char_exclusive, 0, None);
        ctx.defsubr("insert-before-markers", super::editfns::builtin_insert_before_markers, 0, None);
        ctx.defsubr("delete-char", super::editfns::builtin_delete_char, 0, None);
        ctx.defsubr("following-char", super::editfns::builtin_following_char, 0, None);
        ctx.defsubr("preceding-char", super::editfns::builtin_preceding_char, 0, None);
        ctx.defsubr("font-at", super::font::builtin_font_at_eval, 0, None);
        ctx.defsubr("face-font", super::font::builtin_face_font_eval, 0, None);
        ctx.defsubr("access-file", super::fileio::builtin_access_file_eval, 0, None);
        ctx.defsubr("expand-file-name", super::fileio::builtin_expand_file_name_eval, 0, None);
        ctx.defsubr("delete-file-internal", super::fileio::builtin_delete_file_internal_eval, 0, None);
        ctx.defsubr("rename-file", super::fileio::builtin_rename_file_eval, 0, None);
        ctx.defsubr("copy-file", super::fileio::builtin_copy_file_eval, 0, None);
        ctx.defsubr("add-name-to-file", super::fileio::builtin_add_name_to_file_eval, 0, None);
        ctx.defsubr("make-symbolic-link", super::fileio::builtin_make_symbolic_link_eval, 0, None);
        ctx.defsubr("directory-files", super::fileio::builtin_directory_files_eval, 0, None);
        ctx.defsubr("file-attributes", super::dired::builtin_file_attributes_eval, 0, None);
        ctx.defsubr("file-exists-p", super::fileio::builtin_file_exists_p_eval, 0, None);
        ctx.defsubr("file-readable-p", super::fileio::builtin_file_readable_p_eval, 0, None);
        ctx.defsubr("file-writable-p", super::fileio::builtin_file_writable_p_eval, 0, None);
        ctx.defsubr("file-acl", super::fileio::builtin_file_acl_eval, 0, None);
        ctx.defsubr("file-executable-p", super::fileio::builtin_file_executable_p_eval, 0, None);
        ctx.defsubr("file-locked-p", super::fileio::builtin_file_locked_p_eval, 0, None);
        ctx.defsubr("file-selinux-context", super::fileio::builtin_file_selinux_context_eval, 0, None);
        ctx.defsubr("file-system-info", super::fileio::builtin_file_system_info_eval, 0, None);
        ctx.defsubr("file-directory-p", super::fileio::builtin_file_directory_p_eval, 0, None);
        ctx.defsubr("file-regular-p", super::fileio::builtin_file_regular_p_eval, 0, None);
        ctx.defsubr("file-symlink-p", super::fileio::builtin_file_symlink_p_eval, 0, None);
        ctx.defsubr("file-modes", super::fileio::builtin_file_modes_eval, 0, None);
        ctx.defsubr("set-file-modes", super::fileio::builtin_set_file_modes_eval, 0, None);
        ctx.defsubr("set-file-times", super::fileio::builtin_set_file_times_eval, 0, None);
        ctx.defsubr("error-message-string", super::errors::builtin_error_message_string, 0, None);
        ctx.defsubr("char-equal", builtin_char_equal, 0, None);
        ctx.defsubr("internal--define-uninitialized-variable", builtin_internal_define_uninitialized_variable_eval, 0, None);
        ctx.defsubr("compose-region-internal", super::composite::builtin_compose_region_internal_eval, 0, None);
        ctx.defsubr("window-text-pixel-size", super::xdisp::builtin_window_text_pixel_size_eval, 0, None);
        ctx.defsubr("pos-visible-in-window-p", super::xdisp::builtin_pos_visible_in_window_p_eval, 0, None);
        ctx.defsubr("frame--face-hash-table", super::xfaces::builtin_frame_face_hash_table_eval, 0, None);
        ctx.defsubr("delete-directory-internal", super::fileio::builtin_delete_directory_internal_eval, 0, None);
        ctx.defsubr("make-directory-internal", super::fileio::builtin_make_directory_internal_eval, 0, None);
        ctx.defsubr("directory-files-and-attributes", super::dired::builtin_directory_files_and_attributes_eval, 0, None);
        ctx.defsubr("find-file-name-handler", super::fileio::builtin_find_file_name_handler_eval, 0, None);
        ctx.defsubr("file-name-all-completions", super::dired::builtin_file_name_all_completions_eval, 0, None);
        ctx.defsubr("file-accessible-directory-p", super::fileio::builtin_file_accessible_directory_p_eval, 0, None);
        ctx.defsubr("file-name-case-insensitive-p", super::fileio::builtin_file_name_case_insensitive_p_eval, 0, None);
        ctx.defsubr("file-newer-than-file-p", super::fileio::builtin_file_newer_than_file_p_eval, 0, None);
        ctx.defsubr("verify-visited-file-modtime", super::fileio::builtin_verify_visited_file_modtime, 0, None);
        ctx.defsubr("internal-default-interrupt-process", super::process::builtin_internal_default_interrupt_process, 0, None);
        ctx.defsubr("internal-default-process-filter", super::process::builtin_internal_default_process_filter, 0, None);
        ctx.defsubr("internal-default-process-sentinel", super::process::builtin_internal_default_process_sentinel, 0, None);
        ctx.defsubr("internal-default-signal-process", super::process::builtin_internal_default_signal_process, 0, None);
        ctx.defsubr("network-lookup-address-info", super::process::builtin_network_lookup_address_info, 0, None);
        ctx.defsubr("set-network-process-option", super::process::builtin_set_network_process_option, 0, None);
        ctx.defsubr("process-query-on-exit-flag", super::process::builtin_process_query_on_exit_flag, 0, None);
        ctx.defsubr("set-process-query-on-exit-flag", super::process::builtin_set_process_query_on_exit_flag, 0, None);
        ctx.defsubr("process-inherit-coding-system-flag", super::process::builtin_process_inherit_coding_system_flag, 0, None);
        ctx.defsubr("set-process-coding-system", super::process::builtin_set_process_coding_system, 0, None);
        ctx.defsubr("set-process-datagram-address", super::process::builtin_set_process_datagram_address, 0, None);
        ctx.defsubr("remove-list-of-text-properties", super::textprop::builtin_remove_list_of_text_properties, 0, None);
        ctx.defsubr("get-char-property-and-overlay", super::textprop::builtin_get_char_property_and_overlay, 0, None);
        ctx.defsubr("next-single-property-change", super::textprop::builtin_next_single_property_change, 0, None);
        ctx.defsubr("previous-single-property-change", super::textprop::builtin_previous_single_property_change, 0, None);
        ctx.defsubr("line-beginning-position", super::navigation::builtin_line_beginning_position, 0, None);
        ctx.defsubr("make-variable-buffer-local", super::custom::builtin_make_variable_buffer_local, 0, None);
        ctx.defsubr("active-minibuffer-window", super::window_cmds::builtin_active_minibuffer_window_eval, 0, None);
        ctx.defsubr("minibuffer-selected-window", super::window_cmds::builtin_minibuffer_selected_window, 0, None);
        ctx.defsubr("window-mode-line-height", super::window_cmds::builtin_window_mode_line_height, 0, None);
        ctx.defsubr("window-header-line-height", super::window_cmds::builtin_window_header_line_height, 0, None);
        ctx.defsubr("window-tab-line-height", super::window_cmds::builtin_window_tab_line_height, 0, None);
        ctx.defsubr("set-window-display-table", super::window_cmds::builtin_set_window_display_table, 0, None);
        ctx.defsubr("set-window-cursor-type", super::window_cmds::builtin_set_window_cursor_type, 0, None);
        ctx.defsubr("set-window-scroll-bars", super::window_cmds::builtin_set_window_scroll_bars, 0, None);
        ctx.defsubr("set-window-next-buffers", super::window_cmds::builtin_set_window_next_buffers, 0, None);
        ctx.defsubr("set-window-prev-buffers", super::window_cmds::builtin_set_window_prev_buffers, 0, None);
        ctx.defsubr("set-window-dedicated-p", super::window_cmds::builtin_set_window_dedicated_p, 0, None);
        ctx.defsubr("delete-window-internal", super::window_cmds::builtin_delete_window_internal, 0, None);
        ctx.defsubr("delete-other-windows-internal", super::window_cmds::builtin_delete_other_windows_internal, 0, None);
        ctx.defsubr("window-combination-limit", super::window_cmds::builtin_window_combination_limit, 0, None);
        ctx.defsubr("set-window-combination-limit", super::window_cmds::builtin_set_window_combination_limit, 0, None);
        ctx.defsubr("window-resize-apply-total", super::window_cmds::builtin_window_resize_apply_total, 0, None);
        ctx.defsubr("other-window-for-scrolling", super::window_cmds::builtin_other_window_for_scrolling, 0, None);
        ctx.defsubr("select-frame-set-input-focus", super::window_cmds::builtin_select_frame_set_input_focus, 0, None);
        ctx.defsubr("modify-frame-parameters", super::window_cmds::builtin_modify_frame_parameters, 0, None);
        ctx.defsubr("frame-selected-window", super::window_cmds::builtin_frame_selected_window, 0, None);
        ctx.defsubr("frame-old-selected-window", super::window_cmds::builtin_frame_old_selected_window, 0, None);
        ctx.defsubr("set-frame-selected-window", super::window_cmds::builtin_set_frame_selected_window, 0, None);
        ctx.defsubr("x-display-pixel-width", super::display::builtin_x_display_pixel_width_eval, 0, None);
        ctx.defsubr("x-display-pixel-height", super::display::builtin_x_display_pixel_height_eval, 0, None);
        ctx.defsubr("x-server-max-request-size", super::display::builtin_x_server_max_request_size_eval, 0, None);
        ctx.defsubr("x-display-grayscale-p", super::display::builtin_x_display_grayscale_p_eval, 0, None);
        ctx.defsubr("x-display-backing-store", super::display::builtin_x_display_backing_store_eval, 0, None);
        ctx.defsubr("x-display-color-cells", super::display::builtin_x_display_color_cells_eval, 0, None);
        ctx.defsubr("x-display-save-under", super::display::builtin_x_display_save_under_eval, 0, None);
        ctx.defsubr("x-display-set-last-user-time", super::display::builtin_x_display_set_last_user_time_eval, 0, None);
        ctx.defsubr("x-display-visual-class", super::display::builtin_x_display_visual_class_eval, 0, None);
        ctx.defsubr("minor-mode-key-binding", super::interactive::builtin_minor_mode_key_binding, 0, None);
        ctx.defsubr("this-command-keys-vector", super::interactive::builtin_this_command_keys_vector, 0, None);
        ctx.defsubr("this-single-command-keys", super::interactive::builtin_this_single_command_keys, 0, None);
        ctx.defsubr("this-single-command-raw-keys", super::interactive::builtin_this_single_command_raw_keys, 0, None);
        ctx.defsubr("clear-this-command-keys", super::interactive::builtin_clear_this_command_keys, 0, None);
        ctx.defsubr("waiting-for-user-input-p", super::reader::builtin_waiting_for_user_input_p_eval, 0, None);
        ctx.defsubr("minibuffer-prompt", super::minibuffer::builtin_minibuffer_prompt_eval, 0, None);
        ctx.defsubr("minibuffer-prompt-end", super::minibuffer::builtin_minibuffer_prompt_end_eval, 0, None);
        ctx.defsubr("innermost-minibuffer-p", super::minibuffer::builtin_innermost_minibuffer_p_eval, 0, None);
        ctx.defsubr("backtrace--frames-from-thread", super::misc::builtin_backtrace_frames_from_thread, 0, None);
        ctx.defsubr("abort-minibuffers", super::minibuffer::builtin_abort_minibuffers_eval, 0, None);
        ctx.defsubr("set-marker-insertion-type", super::marker::builtin_set_marker_insertion_type_eval, 0, None);
        ctx.defsubr("set-standard-case-table", super::casetab::builtin_set_standard_case_table_eval, 0, None);
        ctx.defsubr("get-unused-category", super::category::builtin_get_unused_category_eval, 0, None);
        ctx.defsubr("standard-category-table", super::category::builtin_standard_category_table_eval, 0, None);
        ctx.defsubr("upcase-initials-region", super::casefiddle::builtin_upcase_initials_region, 0, None);
        ctx.defsubr("buffer-substring-no-properties", super::editfns::builtin_buffer_substring_no_properties, 0, None);
}
