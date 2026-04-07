//! Additional built-in functions to improve Emacs Lisp compatibility.
//!
//! These builtins complement the core set in builtins.rs with:
//! - Advanced list operations (cl-lib compatible)
//! - Sequence operations (seq.el compatible)
//! - String utilities (subr-x compatible)
//! - Window/frame operations
//! - Buffer info queries
//! - Format enhancements
//! - Variable operations

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::string_escape::{storage_byte_to_char, storage_char_len, storage_char_to_byte};
use super::value::{Value, ValueKind, VecLikeType};
#[cfg(unix)]
use std::ffi::CStr;
use std::fs;

// ---------------------------------------------------------------------------
// Argument helpers
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

fn expect_string(val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::String => Ok(val.as_str().unwrap().to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *val],
        )),
    }
}

fn expect_int(val: &Value) -> Result<i64, Flow> {
    match val.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *val],
        )),
    }
}

fn symbol_like_name(value: &Value) -> Option<&str> {
    match value.kind() {
        ValueKind::Nil => Some("nil"),
        ValueKind::T => Some("t"),
        ValueKind::Symbol(id) => Some(resolve_sym(id)),
        _ => None,
    }
}

fn expect_number_or_marker_f64(value: &Value) -> Result<f64, Flow> {
    use crate::emacs_core::value::VecLikeType;
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n as f64),
        ValueKind::Float => Ok(value.xfloat()),
        ValueKind::Veclike(VecLikeType::Bignum) => Ok(value.as_bignum().unwrap().to_f64()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("number-or-marker-p"), *value],
        )),
    }
}

fn list_car_or_signal(value: &Value) -> Result<Value, Flow> {
    match value.kind() {
        ValueKind::Cons => Ok(value.cons_car()),
        ValueKind::Nil => Ok(Value::NIL),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *value],
        )),
    }
}

fn assoc_string_key_name(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::String => Ok(value.as_str().unwrap().to_owned()),
        _ => symbol_like_name(value)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *value],
                )
            }),
    }
}

fn assoc_string_entry_name(value: &Value) -> Option<String> {
    match value.kind() {
        ValueKind::String => Some(value.as_str().unwrap().to_owned()),
        _ => symbol_like_name(value).map(ToOwned::to_owned),
    }
}

fn assoc_string_equal(left: &str, right: &str, fold_case: bool) -> bool {
    if fold_case {
        left.chars()
            .flat_map(char::to_lowercase)
            .eq(right.chars().flat_map(char::to_lowercase))
    } else {
        left == right
    }
}

fn collect_sequence_strict(val: &Value) -> Result<Vec<Value>, Flow> {
    match val.kind() {
        ValueKind::Nil => Ok(Vec::new()),
        ValueKind::Cons => {
            let mut result = Vec::new();
            let mut cursor = *val;
            loop {
                match cursor.kind() {
                    ValueKind::Nil => return Ok(result),
                    ValueKind::Cons => {
                        let pair_car = cursor.cons_car();
                        let pair_cdr = cursor.cons_cdr();
                        result.push(pair_car);
                        cursor = pair_cdr;
                    }
                    tail => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("listp"), cursor],
                        ));
                    }
                }
            }
        }
        ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
            Ok(val.as_vector_data().unwrap().clone())
        }
        ValueKind::String => {
            let s = val.as_str().unwrap().to_owned();
            Ok(s.chars().map(|ch| Value::fixnum(ch as i64)).collect())
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sequencep"), *val],
        )),
    }
}

// ---------------------------------------------------------------------------
// Advanced list operations
// ---------------------------------------------------------------------------

pub(crate) fn remove_list_equal(args: Vec<Value>) -> EvalResult {
    expect_args("remove", &args, 2)?;
    let target = &args[0];
    let list_val = &args[1];

    let mut result = Vec::new();
    let mut cursor = *list_val;
    loop {
        match cursor.kind() {
            ValueKind::Nil => break,
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if !super::value::equal_value(&pair_car, target, 0) {
                    result.push(pair_car);
                }
                cursor = pair_cdr;
            }
            _ => break,
        }
    }
    Ok(Value::list(result))
}

/// `(take N LIST)` — first N elements.
pub(crate) fn builtin_take(args: Vec<Value>) -> EvalResult {
    expect_args("take", &args, 2)?;
    let n = expect_int(&args[0])?;
    if n <= 0 {
        return Ok(Value::NIL);
    }
    let list = &args[1];
    if !list.is_nil() && !list.is_cons() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *list],
        ));
    }

    let mut result = Vec::new();
    let mut cursor = *list;
    for _ in 0..(n as usize) {
        match cursor.kind() {
            ValueKind::Nil => break,
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                result.push(pair_car);
                cursor = pair_cdr;
            }
            tail => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), cursor],
                ));
            }
        }
    }
    Ok(Value::list(result))
}

// ---------------------------------------------------------------------------
// String utilities (subr-x compatible)
// ---------------------------------------------------------------------------

/// `(string-search NEEDLE HAYSTACK &optional START)`.
///
/// START is a character position (not byte offset).  The return value
/// is also a character position, matching GNU Emacs semantics.
pub(crate) fn builtin_string_search(args: Vec<Value>) -> EvalResult {
    expect_min_args("string-search", &args, 2)?;
    let needle = expect_string(&args[0])?;
    let haystack = expect_string(&args[1])?;
    let char_len = storage_char_len(&haystack);
    let start_char = if args.len() > 2 {
        let n = expect_int(&args[2])?;
        if n < 0 || n as usize > char_len {
            return Err(signal(
                "args-out-of-range",
                vec![args[2], Value::fixnum(0), Value::fixnum(char_len as i64)],
            ));
        }
        n as usize
    } else {
        0
    };

    // Convert the start character position to a byte offset for slicing.
    let start_byte = storage_char_to_byte(&haystack, start_char);
    let search_in = &haystack[start_byte..];
    match search_in.find(&needle) {
        Some(byte_pos) => {
            // Convert the absolute byte position back to a character position.
            let abs_byte = start_byte + byte_pos;
            let char_pos = storage_byte_to_char(&haystack, abs_byte);
            Ok(Value::fixnum(char_pos as i64))
        }
        None => Ok(Value::NIL),
    }
}

// ---------------------------------------------------------------------------
// Predicate additions
// ---------------------------------------------------------------------------

/// `(proper-list-p OBJ)` -> length if OBJ is a proper list, nil otherwise.
pub(crate) fn builtin_proper_list_p(args: Vec<Value>) -> EvalResult {
    expect_args("proper-list-p", &args, 1)?;
    match super::value::list_length(&args[0]) {
        Some(len) => Ok(Value::fixnum(len as i64)),
        None => Ok(Value::NIL),
    }
}

/// `(subrp OBJ)` -> t if OBJ is a built-in function.
pub(crate) fn builtin_subrp(args: Vec<Value>) -> EvalResult {
    expect_args("subrp", &args, 1)?;
    Ok(Value::bool_val(args[0].as_subr_id().is_some()))
}

/// `(bare-symbol SYMBOL-OR-SYMBOL-WITH-POS)` -> symbol.
pub(crate) fn builtin_bare_symbol(args: Vec<Value>) -> EvalResult {
    expect_args("bare-symbol", &args, 1)?;
    if symbol_like_name(&args[0]).is_some() {
        Ok(args[0])
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![
                Value::list(vec![
                    Value::symbol("symbolp"),
                    Value::symbol("symbol-with-pos-p"),
                ]),
                args[0],
            ],
        ))
    }
}

/// `(bare-symbol-p OBJECT)` -> t if symbol (including keyword/nil/t).
pub(crate) fn builtin_bare_symbol_p(args: Vec<Value>) -> EvalResult {
    expect_args("bare-symbol-p", &args, 1)?;
    Ok(Value::bool_val(symbol_like_name(&args[0]).is_some()))
}

/// `(byteorder)` -> `?l` on little-endian, `?B` on big-endian.
pub(crate) fn builtin_byteorder(args: Vec<Value>) -> EvalResult {
    expect_args("byteorder", &args, 0)?;
    let marker = if cfg!(target_endian = "little") {
        'l'
    } else {
        'B'
    };
    Ok(Value::fixnum(marker as i64))
}

/// `(assoc-string KEY ALIST &optional CASE-FOLD)` -> first matching cell.
pub(crate) fn builtin_assoc_string(args: Vec<Value>) -> EvalResult {
    expect_min_args("assoc-string", &args, 2)?;
    expect_max_args("assoc-string", &args, 3)?;
    let needle = assoc_string_key_name(&args[0])?;
    let fold_case = args.get(2).is_some_and(|v| v.is_truthy());

    let mut cursor = args[1];
    loop {
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                let entry = pair_car;
                cursor = pair_cdr;

                if !entry.is_cons() {
                    continue;
                };
                let entry_pair_car = entry.cons_car();
                let entry_pair_cdr = entry.cons_cdr();
                let Some(entry_key) = assoc_string_entry_name(&entry_pair_car) else {
                    continue;
                };
                if assoc_string_equal(&needle, &entry_key, fold_case) {
                    return Ok(entry);
                }
            }
            _ => return Ok(Value::NIL),
        }
    }
}

/// `(car-less-than-car A B)` -> t if `(car A) < (car B)`.
pub(crate) fn builtin_car_less_than_car(args: Vec<Value>) -> EvalResult {
    expect_args("car-less-than-car", &args, 2)?;
    let left = list_car_or_signal(&args[0])?;
    let right = list_car_or_signal(&args[1])?;
    Ok(Value::bool_val(
        expect_number_or_marker_f64(&left)? < expect_number_or_marker_f64(&right)?,
    ))
}

/// `(byte-code-function-p OBJ)` -> t if compiled.
pub(crate) fn builtin_byte_code_function_p(args: Vec<Value>) -> EvalResult {
    expect_args("byte-code-function-p", &args, 1)?;
    Ok(Value::bool_val(args[0].is_bytecode()))
}

/// `(compiled-function-p OBJ)` -> t if compiled function.
pub(crate) fn builtin_compiled_function_p(args: Vec<Value>) -> EvalResult {
    expect_args("compiled-function-p", &args, 1)?;
    Ok(Value::bool_val(args[0].is_bytecode()))
}

/// `(closurep OBJ)` -> t if closure.
pub(crate) fn builtin_closurep(args: Vec<Value>) -> EvalResult {
    expect_args("closurep", &args, 1)?;
    Ok(Value::bool_val(
        args[0].is_lambda() || args[0].is_bytecode(),
    ))
}

/// `(natnump OBJ)` -> t if natural number (>= 0).
pub(crate) fn builtin_natnump(args: Vec<Value>) -> EvalResult {
    expect_args("natnump", &args, 1)?;
    let is_nat = match args[0].kind() {
        ValueKind::Fixnum(n) => n >= 0,
        _ => false,
    };
    Ok(Value::bool_val(is_nat))
}

/// `(zerop OBJ)` -> t if zero.
pub(crate) fn builtin_zerop(args: Vec<Value>) -> EvalResult {
    expect_args("zerop", &args, 1)?;
    let is_zero = match args[0].kind() {
        ValueKind::Fixnum(0) => true,
        ValueKind::Float => args[0].xfloat() == 0.0,
        _ => false,
    };
    Ok(Value::bool_val(is_zero))
}

// ---------------------------------------------------------------------------
// Misc operations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PasswdEntry {
    login: String,
    uid: i64,
    gecos: String,
}

fn parse_passwd_entry(line: &str) -> Option<PasswdEntry> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let mut fields = trimmed.split(':');
    let login = fields.next()?.to_string();
    let _passwd = fields.next()?;
    let uid = fields.next()?.parse::<i64>().ok()?;
    let _gid = fields.next()?;
    let gecos = fields.next().unwrap_or("").to_string();
    Some(PasswdEntry { login, uid, gecos })
}

fn load_passwd_entries() -> Vec<PasswdEntry> {
    fs::read_to_string("/etc/passwd")
        .ok()
        .map(|content| content.lines().filter_map(parse_passwd_entry).collect())
        .unwrap_or_default()
}

fn login_name_from_env() -> Option<String> {
    std::env::var("LOGNAME")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .filter(|name| !name.is_empty())
}

fn current_uid() -> i64 {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(1000)
}

fn real_uid() -> i64 {
    std::process::Command::new("id")
        .args(["-ru"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or_else(current_uid)
}

fn lookup_login_by_uid(uid: i64) -> Option<String> {
    load_passwd_entries()
        .into_iter()
        .find(|entry| entry.uid == uid)
        .map(|entry| entry.login)
}

fn canonical_full_name(entry: &PasswdEntry) -> String {
    let first_gecos = entry.gecos.split(',').next().unwrap_or("").trim();
    if first_gecos.is_empty() {
        entry.login.clone()
    } else {
        first_gecos.to_string()
    }
}

fn lookup_full_name_by_uid(uid: i64) -> Option<String> {
    load_passwd_entries()
        .into_iter()
        .find(|entry| entry.uid == uid)
        .map(|entry| canonical_full_name(&entry))
}

fn lookup_full_name_by_login(login: &str) -> Option<String> {
    load_passwd_entries()
        .into_iter()
        .find(|entry| entry.login == login)
        .map(|entry| canonical_full_name(&entry))
}

fn expect_uid_arg(val: &Value) -> Result<i64, Flow> {
    match val.kind() {
        ValueKind::Fixnum(uid) if uid >= 0 => Ok(uid),
        _ => Err(signal(
            "error",
            vec![Value::string(
                "Not an in-range integer, integral float, or cons of integers",
            )],
        )),
    }
}

/// `(user-login-name &optional UID)` -> string or nil.
pub(crate) fn builtin_user_login_name(args: Vec<Value>) -> EvalResult {
    expect_max_args("user-login-name", &args, 1)?;
    if let Some(uid_arg) = args.first() {
        if uid_arg.is_nil() {
            let current = login_name_from_env()
                .or_else(|| lookup_login_by_uid(current_uid()))
                .unwrap_or_else(|| "unknown".to_string());
            return Ok(Value::string(current));
        }
        let uid = expect_uid_arg(uid_arg)?;
        return Ok(match lookup_login_by_uid(uid) {
            Some(name) => Value::string(name),
            None => Value::NIL,
        });
    }

    let current = login_name_from_env()
        .or_else(|| lookup_login_by_uid(current_uid()))
        .unwrap_or_else(|| "unknown".to_string());
    Ok(Value::string(current))
}

/// `(user-real-login-name)` -> string.
pub(crate) fn builtin_user_real_login_name(args: Vec<Value>) -> EvalResult {
    expect_args("user-real-login-name", &args, 0)?;
    let name = lookup_login_by_uid(real_uid())
        .or_else(login_name_from_env)
        .unwrap_or_else(|| "unknown".to_string());
    Ok(Value::string(name))
}

/// `(user-full-name &optional UID-OR-LOGIN)` -> string or nil.
pub(crate) fn builtin_user_full_name(args: Vec<Value>) -> EvalResult {
    expect_max_args("user-full-name", &args, 1)?;
    if let Some(target) = args.first() {
        if target.is_nil() {
            if let Ok(name) = std::env::var("NAME") {
                if !name.is_empty() {
                    return Ok(Value::string(name));
                }
            }
            let fallback = lookup_full_name_by_uid(current_uid())
                .or_else(|| {
                    login_name_from_env()
                        .as_deref()
                        .and_then(lookup_full_name_by_login)
                })
                .or_else(login_name_from_env)
                .unwrap_or_else(|| "unknown".to_string());
            return Ok(Value::string(fallback));
        }

        return Ok(match target.kind() {
            ValueKind::Fixnum(uid) => {
                if uid < 0 {
                    return Err(signal(
                        "error",
                        vec![Value::string(
                            "Not an in-range integer, integral float, or cons of integers",
                        )],
                    ));
                }
                lookup_full_name_by_uid(uid)
                    .map(Value::string)
                    .unwrap_or(Value::NIL)
            }
            ValueKind::String => {
                let login = target.as_str().unwrap().to_owned();
                lookup_full_name_by_login(&login)
                    .map(Value::string)
                    .unwrap_or(Value::NIL)
            }
            _ => {
                return Err(signal(
                    "error",
                    vec![Value::string(
                        "Not an in-range integer, integral float, or cons of integers",
                    )],
                ));
            }
        });
    }

    if let Ok(name) = std::env::var("NAME") {
        if !name.is_empty() {
            return Ok(Value::string(name));
        }
    }

    let fallback = lookup_full_name_by_uid(current_uid())
        .or_else(|| {
            login_name_from_env()
                .as_deref()
                .and_then(lookup_full_name_by_login)
        })
        .or_else(login_name_from_env)
        .unwrap_or_else(|| "unknown".to_string());
    Ok(Value::string(fallback))
}

/// `(system-name)` -> string.
///
/// `(fixnump OBJ)` — return t if OBJ is a fixnum (small integer).
///
/// Mirrors GNU `Ffixnump` (`src/data.c`). Now that bignums are real,
/// this is *not* the same as `integerp` — `fixnump` returns nil for a
/// bignum even though `integerp` returns t.
pub(crate) fn builtin_fixnump(args: Vec<Value>) -> EvalResult {
    expect_args("fixnump", &args, 1)?;
    Ok(Value::bool_val(args[0].is_fixnum()))
}

/// `(bignump OBJ)` — return t if OBJ is a bignum.
///
/// Mirrors GNU `Fbignump` (`src/data.c`) and the `BIGNUMP` predicate.
/// Now that NeoMacs allocates real bignum objects via
/// [`Value::make_integer`] / [`Value::bignum`], this checks the
/// underlying veclike type tag instead of always returning nil.
pub(crate) fn builtin_bignump(args: Vec<Value>) -> EvalResult {
    expect_args("bignump", &args, 1)?;
    Ok(Value::bool_val(args[0].is_bignum()))
}

/// GNU editfns.c:1283 — returns the host name via `gethostname(2)`,
/// replacing spaces and tabs with `-`.
pub(crate) fn builtin_system_name(args: Vec<Value>) -> EvalResult {
    expect_args("system-name", &args, 0)?;
    let name = hostname::get()
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "localhost".to_string());
    // GNU replaces spaces and tabs with dashes (sysdep.c:1646).
    let name: String = name
        .chars()
        .map(|c| if c == ' ' || c == '\t' { '-' } else { c })
        .collect();
    Ok(Value::string(name))
}

pub(crate) fn operating_system_release_value() -> Value {
    operating_system_release()
        .map(Value::string)
        .unwrap_or(Value::NIL)
}

pub(crate) fn system_configuration_value() -> Value {
    Value::string(
        option_env!("TARGET")
            .map(str::to_owned)
            .unwrap_or_else(fallback_system_configuration),
    )
}

pub(crate) fn system_configuration_options_value() -> Value {
    Value::string("")
}

pub(crate) fn system_configuration_features_value() -> Value {
    let mut features = vec!["PDUMPER".to_string(), "THREADS".to_string()];
    features.sort_unstable();
    features.dedup();
    Value::string(features.join(" "))
}

fn fallback_system_configuration() -> String {
    let arch = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        other => other,
    };
    format!("{arch}-{os}")
}

fn operating_system_release() -> Option<String> {
    #[cfg(unix)]
    {
        let mut utsname = std::mem::MaybeUninit::<libc::utsname>::uninit();
        // SAFETY: uname writes a fully initialized utsname struct on success.
        let release = unsafe {
            if libc::uname(utsname.as_mut_ptr()) != 0 {
                return None;
            }
            let utsname = utsname.assume_init();
            CStr::from_ptr(utsname.release.as_ptr())
                .to_string_lossy()
                .trim()
                .to_string()
        };
        if release.is_empty() {
            None
        } else {
            Some(release)
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// `(emacs-version)` -> string.
pub(crate) fn builtin_emacs_version(args: Vec<Value>) -> EvalResult {
    expect_max_args("emacs-version", &args, 1)?;
    if args.first().is_some_and(|arg| !arg.is_nil()) {
        return Ok(Value::NIL);
    }
    Ok(Value::string(
        "GNU Emacs 31.0.50 (build 1, x86_64-pc-linux-gnu) [NeoVM 0.1.0 (Neomacs)]\n Copyright (C) 2026 Free Software Foundation, Inc.",
    ))
}

/// `(emacs-pid)` -> integer.
pub(crate) fn builtin_emacs_pid(args: Vec<Value>) -> EvalResult {
    expect_args("emacs-pid", &args, 0)?;
    Ok(Value::fixnum(std::process::id() as i64))
}

fn gc_bucket(name: &str, counts: &[i64]) -> Value {
    let mut items = Vec::with_capacity(counts.len() + 1);
    items.push(Value::symbol(name));
    items.extend(counts.iter().copied().map(Value::fixnum));
    Value::list(items)
}

/// Build the GC stats list (shared by eval and vm garbage-collect paths).
pub(crate) fn builtin_garbage_collect_stats() -> EvalResult {
    let counts = Value::memory_use_counts_snapshot();
    let conses = counts[0].max(0);
    let floats = counts[1].max(0);
    let vector_cells = counts[2].max(0);
    let symbols = counts[3].max(0);
    let string_chars = counts[4].max(0);
    let intervals = counts[5].max(0);
    let strings = counts[6].max(0);

    Ok(Value::list(vec![
        gc_bucket("conses", &[16, conses, 0]),
        gc_bucket("symbols", &[48, symbols, 0]),
        gc_bucket("strings", &[32, strings, 0]),
        gc_bucket("string-bytes", &[1, string_chars]),
        gc_bucket("vectors", &[16, vector_cells]),
        gc_bucket("vector-slots", &[8, vector_cells, 0]),
        gc_bucket("floats", &[8, floats, 0]),
        gc_bucket("intervals", &[56, intervals, 0]),
        gc_bucket("buffers", &[992, 0]),
    ]))
}

/// `(memory-use-counts)` -> list of runtime allocation counters:
/// `(CONS FLOATS VECTOR-CELLS SYMBOLS STRING-CHARS INTERVALS STRINGS)`.
pub(crate) fn builtin_memory_use_counts(args: Vec<Value>) -> EvalResult {
    expect_args("memory-use-counts", &args, 0)?;
    let counts = Value::memory_use_counts_snapshot();
    Ok(Value::list(
        counts.iter().map(|count| Value::fixnum(*count)).collect(),
    ))
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "builtins_extra_test.rs"]
mod tests;
