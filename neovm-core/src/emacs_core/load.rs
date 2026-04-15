//! File loading and module system (require/provide/load).

use super::builtins::collections::builtin_make_hash_table;
use super::error::{EvalError, Flow, map_flow, signal};
use super::intern::{intern, resolve_sym};
use super::keymap::{is_list_keymap, list_keymap_lookup_one};
use super::value::{HashKey, Value, ValueKind, list_to_vec};
use crate::heap_types::LispString;
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

thread_local! {
    static BOOTSTRAP_PREFER_LDEFS_BOOT: Cell<bool> = const { Cell::new(false) };
}

fn load_string_text(value: &Value) -> Option<String> {
    value.as_runtime_string_owned()
}

fn load_runtime_string(value: &LispString) -> String {
    super::builtins::runtime_string_from_lisp_string(value)
}

fn load_path_lisp_string(path: &Path) -> LispString {
    super::fileio::path_to_lisp_file_name(path)
}

fn load_path_buf(value: &LispString) -> PathBuf {
    super::fileio::lisp_file_name_to_path_buf(value)
}

fn load_path_value(path: &Path) -> Value {
    Value::heap_string(load_path_lisp_string(path))
}

struct BootstrapLdefsBootPreferenceGuard {
    previous: bool,
}

impl BootstrapLdefsBootPreferenceGuard {
    fn enable() -> Self {
        let previous = BOOTSTRAP_PREFER_LDEFS_BOOT.with(|cell| cell.replace(true));
        Self { previous }
    }
}

impl Drop for BootstrapLdefsBootPreferenceGuard {
    fn drop(&mut self) {
        BOOTSTRAP_PREFER_LDEFS_BOOT.with(|cell| cell.set(self.previous));
    }
}

fn bootstrap_prefers_ldefs_boot() -> bool {
    BOOTSTRAP_PREFER_LDEFS_BOOT.with(Cell::get)
}

/// Decode Emacs "extended UTF-8" bytes into a Rust String.
///
/// Emacs uses a superset of UTF-8 that allows code points above U+10FFFF
/// (used for internal charset characters, eight-bit raw bytes, etc.).
/// These are encoded using UTF-8-style 4/5/6-byte sequences that standard
/// UTF-8 rejects once the leading byte is above F4.
///
/// For `?<extended>` character literals, we replace the extended bytes
/// with `?\x<HEX>` escape syntax that the parser already supports.
/// All other extended byte sequences (outside `?` context) are replaced
/// with U+FFFD, matching lossy UTF-8 behaviour.
pub(crate) fn decode_emacs_utf8(bytes: &[u8]) -> String {
    fn push_extended_char_or_escape(out: &mut String, code: u32) {
        if out.ends_with('?') {
            // Replace the extended char with `\x<HEX>` escape so the
            // parser reads it as an integer code point.
            out.push_str(&format!("\\x{:X}", code));
        } else {
            // Outside character literal context, use replacement char.
            out.push('\u{FFFD}');
        }
    }

    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // ASCII byte — fast path.
        if b < 0x80 {
            out.push(b as char);
            i += 1;
            continue;
        }
        // Valid 2-byte UTF-8 (C2-DF).
        if b >= 0xC2 && b <= 0xDF && i + 1 < bytes.len() && (bytes[i + 1] & 0xC0) == 0x80 {
            if let Some(s) = std::str::from_utf8(&bytes[i..i + 2]).ok() {
                out.push_str(s);
                i += 2;
                continue;
            }
        }
        // Valid 3-byte UTF-8 (E0-EF).
        if b >= 0xE0
            && b <= 0xEF
            && i + 2 < bytes.len()
            && (bytes[i + 1] & 0xC0) == 0x80
            && (bytes[i + 2] & 0xC0) == 0x80
        {
            if let Some(s) = std::str::from_utf8(&bytes[i..i + 3]).ok() {
                out.push_str(s);
                i += 3;
                continue;
            }
        }
        // Valid standard 4-byte UTF-8 (F0-F4, code point <= 10FFFF).
        if b >= 0xF0
            && b <= 0xF4
            && i + 3 < bytes.len()
            && (bytes[i + 1] & 0xC0) == 0x80
            && (bytes[i + 2] & 0xC0) == 0x80
            && (bytes[i + 3] & 0xC0) == 0x80
        {
            if let Some(s) = std::str::from_utf8(&bytes[i..i + 4]).ok() {
                out.push_str(s);
                i += 4;
                continue;
            }
        }
        // Extended 4-byte (F5-F7): Emacs-internal code point > U+10FFFF.
        if b >= 0xF5
            && b <= 0xF7
            && i + 3 < bytes.len()
            && (bytes[i + 1] & 0xC0) == 0x80
            && (bytes[i + 2] & 0xC0) == 0x80
            && (bytes[i + 3] & 0xC0) == 0x80
        {
            let code = ((b as u32 & 0x07) << 18)
                | ((bytes[i + 1] as u32 & 0x3F) << 12)
                | ((bytes[i + 2] as u32 & 0x3F) << 6)
                | (bytes[i + 3] as u32 & 0x3F);
            push_extended_char_or_escape(&mut out, code);
            i += 4;
            continue;
        }
        // Extended 5-byte (F8-FB): still accepted by Emacs's internal UTF-8.
        if b >= 0xF8
            && b <= 0xFB
            && i + 4 < bytes.len()
            && (bytes[i + 1] & 0xC0) == 0x80
            && (bytes[i + 2] & 0xC0) == 0x80
            && (bytes[i + 3] & 0xC0) == 0x80
            && (bytes[i + 4] & 0xC0) == 0x80
        {
            let code = ((b as u32 & 0x03) << 24)
                | ((bytes[i + 1] as u32 & 0x3F) << 18)
                | ((bytes[i + 2] as u32 & 0x3F) << 12)
                | ((bytes[i + 3] as u32 & 0x3F) << 6)
                | (bytes[i + 4] as u32 & 0x3F);
            push_extended_char_or_escape(&mut out, code);
            i += 5;
            continue;
        }
        // Extended 6-byte (FC-FD): highest Emacs internal codes.
        if b >= 0xFC
            && b <= 0xFD
            && i + 5 < bytes.len()
            && (bytes[i + 1] & 0xC0) == 0x80
            && (bytes[i + 2] & 0xC0) == 0x80
            && (bytes[i + 3] & 0xC0) == 0x80
            && (bytes[i + 4] & 0xC0) == 0x80
            && (bytes[i + 5] & 0xC0) == 0x80
        {
            let code = ((b as u32 & 0x01) << 30)
                | ((bytes[i + 1] as u32 & 0x3F) << 24)
                | ((bytes[i + 2] as u32 & 0x3F) << 18)
                | ((bytes[i + 3] as u32 & 0x3F) << 12)
                | ((bytes[i + 4] as u32 & 0x3F) << 6)
                | (bytes[i + 5] as u32 & 0x3F);
            push_extended_char_or_escape(&mut out, code);
            i += 6;
            continue;
        }
        // Invalid byte — replacement character.
        out.push('\u{FFFD}');
        i += 1;
    }
    out
}

/// Format a Value for human-readable error messages, resolving SymIds and heap-backed values.
fn format_value_for_error(v: &Value) -> String {
    match v.kind() {
        ValueKind::Symbol(sid) => super::intern::resolve_sym(sid).to_string(),
        ValueKind::String => format!("\"{}\"", load_string_text(v).expect("checked string")),
        ValueKind::Fixnum(n) => format!("{}", n),
        ValueKind::Nil => "nil".to_string(),
        ValueKind::T => "t".to_string(),
        ValueKind::Cons => {
            let car = v.cons_car();
            let cdr = v.cons_cdr();
            let car_s = format_value_for_error(&car);
            let cdr_s = format_value_for_error(&cdr);
            if cdr == Value::NIL {
                format!("({})", car_s)
            } else {
                format!("({} . {})", car_s, cdr_s)
            }
        }
        other => format!("{:?}", v),
    }
}

fn format_eval_error_in_state(eval: &super::eval::Context, err: &EvalError) -> String {
    match err {
        EvalError::Signal {
            symbol,
            data,
            raw_data,
        } => {
            let payload = if let Some(raw) = raw_data {
                crate::emacs_core::error::print_value_in_state(eval, raw)
            } else if data.is_empty() {
                "nil".to_string()
            } else {
                crate::emacs_core::error::print_value_in_state(eval, &Value::list(data.clone()))
            };
            format!("({} {})", resolve_sym(*symbol), payload)
        }
        EvalError::UncaughtThrow { tag, value } => format!(
            "(throw {} {})",
            crate::emacs_core::error::print_value_in_state(eval, tag),
            crate::emacs_core::error::print_value_in_state(eval, value),
        ),
    }
}

const GENERATED_LOADDEFS_MARKER: &str = "Generated by the `loaddefs-generate' function.";
const TRANSIENT_RUNTIME_FEATURES: &[&str] = &[
    "cl-lib", "cl-macs", "cl-seq", "cl-extra", "gv", "icons", "pcase",
];

fn is_generated_loaddefs_source(source: &str) -> bool {
    source.contains(GENERATED_LOADDEFS_MARKER)
}

fn eval_generated_form_args(
    eval: &mut super::eval::Context,
    args: &[Value],
) -> Result<Vec<Value>, EvalError> {
    args.iter()
        .map(|value| eval_runtime_form(eval, *value))
        .collect()
}

fn eval_runtime_form(eval: &mut super::eval::Context, form: Value) -> Result<Value, EvalError> {
    eval.eval_sub(form).map_err(map_flow)
}

fn cached_form_requires_eager_replay(form: Value) -> bool {
    form.is_cons()
        && form
            .cons_car()
            .as_symbol_name()
            .is_some_and(|name| matches!(name, "eval-and-compile" | "eval-when-compile"))
}

fn generated_defalias(eval: &mut super::eval::Context, args: &[Value]) -> Result<Value, EvalError> {
    if !(2..=3).contains(&args.len()) {
        return Err(EvalError::Signal {
            symbol: intern("wrong-number-of-arguments"),
            data: vec![Value::symbol("defalias"), Value::fixnum(args.len() as i64)],
            raw_data: None,
        });
    }
    let values = eval_generated_form_args(eval, args)?;
    let result = eval
        .defalias_value(values[0], values[1])
        .map_err(map_flow)?;
    if let Some(doc) = values.get(2).copied().filter(|value| !value.is_nil()) {
        super::builtins::builtin_put(
            eval,
            vec![values[0], Value::symbol("function-documentation"), doc],
        )
        .map_err(map_flow)?;
    }
    Ok(result)
}

fn try_eval_generated_loaddefs_form(
    eval: &mut super::eval::Context,
    form: Value,
) -> Result<Option<Value>, EvalError> {
    let Some(items) = list_to_vec(&form) else {
        return Ok(None);
    };
    let Some(head_sym) = items.first().and_then(|v| v.as_symbol_name()) else {
        return Ok(None);
    };
    let tail = &items[1..];
    // Keep this table limited to core primitive replay.  GNU Lisp-owned
    // helpers from loaddefs (e.g. custom/obsolete metadata helpers) should
    // run through the already-loaded GNU Lisp runtime instead.
    match head_sym {
        "progn" => {
            let mut last = Value::NIL;
            for item in tail {
                last = eval_generated_loaddefs_form(eval, *item)?;
            }
            Ok(Some(last))
        }
        "autoload" => {
            let values = eval_generated_form_args(eval, tail)?;
            Ok(Some(
                super::autoload::builtin_autoload(eval, values).map_err(map_flow)?,
            ))
        }
        "put" | "function-put" => {
            let values = eval_generated_form_args(eval, tail)?;
            Ok(Some(
                super::builtins::builtin_put(eval, values).map_err(map_flow)?,
            ))
        }
        "defalias" => Ok(Some(generated_defalias(eval, tail)?)),
        "defvaralias" => {
            let values = eval_generated_form_args(eval, tail)?;
            Ok(Some(
                super::builtins::builtin_defvaralias(eval, values).map_err(map_flow)?,
            ))
        }
        _ => Ok(None),
    }
}

fn eval_generated_loaddefs_form(
    eval: &mut super::eval::Context,
    form: Value,
) -> Result<Value, EvalError> {
    if let Some(value) = try_eval_generated_loaddefs_form(eval, form)? {
        return Ok(value);
    }
    eval_runtime_form(eval, form)
}

fn has_load_suffix(name: &LispString) -> bool {
    let bytes = name.as_bytes();
    bytes.ends_with(b".el") || bytes.ends_with(b".elc")
}

fn append_load_suffix(base: &Path, suffix: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        let mut bytes = base.as_os_str().as_bytes().to_vec();
        bytes.extend_from_slice(suffix);
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
    }

    #[cfg(not(unix))]
    {
        let suffix = std::str::from_utf8(suffix).expect("ASCII suffix");
        PathBuf::from(format!("{}{}", base.to_string_lossy(), suffix))
    }
}

fn source_suffixed_path(base: &Path) -> PathBuf {
    append_load_suffix(base, b".el")
}

fn compiled_suffixed_path(base: &Path) -> PathBuf {
    append_load_suffix(base, b".elc")
}

fn unsupported_compiled_suffixed_paths(base: &Path) -> [PathBuf; 1] {
    [append_load_suffix(base, b".elc.gz")]
}

/// GNU Emacs always prefers .elc over .el (load-suffixes defaults to
/// (".elc" ".el")).  NeoVM matches this by default.
/// Set NEOVM_PREFER_EL=1 to prefer .el source (for debugging).
fn prefer_el_only() -> bool {
    std::env::var("NEOVM_PREFER_EL").is_ok()
}

fn candidate_mtime(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

fn pick_suffixed(base: &Path, prefer_newer: bool) -> Option<PathBuf> {
    let el = source_suffixed_path(base);
    let elc = compiled_suffixed_path(base);
    let skip_elc = prefer_el_only();

    if prefer_newer && !skip_elc {
        let mut candidates = Vec::new();
        if elc.exists() {
            candidates.push(elc.clone());
        }
        if el.exists() {
            candidates.push(el.clone());
        }
        return candidates
            .into_iter()
            .filter_map(|path| candidate_mtime(&path).map(|mtime| (mtime, path)))
            .max_by_key(|(mtime, _)| *mtime)
            .map(|(_, path)| path);
    }

    // GNU default: try .elc first, then .el
    if !skip_elc && elc.exists() {
        return Some(elc);
    }
    if el.exists() {
        return Some(el);
    }

    None
}

fn find_for_base(
    base: &Path,
    original_name: &LispString,
    no_suffix: bool,
    must_suffix: bool,
    prefer_newer: bool,
) -> Option<PathBuf> {
    if no_suffix || has_load_suffix(original_name) {
        if base.is_file() {
            return Some(base.to_path_buf());
        }
        return None;
    }

    if let Some(suffixed) = pick_suffixed(base, prefer_newer) {
        return Some(suffixed);
    }

    if !must_suffix && base.is_file() {
        return Some(base.to_path_buf());
    }

    // Surface unsupported compressed compiled artifacts explicitly.
    for compiled in unsupported_compiled_suffixed_paths(base) {
        if compiled.exists() {
            return Some(compiled);
        }
    }

    None
}

fn expand_tilde_path_buf(path: &LispString) -> PathBuf {
    #[cfg(unix)]
    {
        let bytes = path.as_bytes();
        if bytes == b"~" {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home);
            }
        } else if bytes.starts_with(b"~/") {
            if let Some(home) = std::env::var_os("HOME") {
                let mut expanded = PathBuf::from(home);
                expanded.push(std::ffi::OsString::from_vec(bytes[2..].to_vec()));
                return expanded;
            }
        }

        return load_path_buf(path);
    }

    #[cfg(not(unix))]
    {
        PathBuf::from(expand_tilde(&load_runtime_string(path)))
    }
}

/// Search for a file in the load path.
#[tracing::instrument(level = "debug", ret)]
pub fn find_file_in_load_path(name: &str, load_path: &[LispString]) -> Option<PathBuf> {
    find_file_in_load_path_with_flags(name, load_path, false, false, false)
}

/// Search for a file in load-path with `load` optional suffix flags.
///
/// Behavior follows Emacs:
/// - `no_suffix`: load only the exact filename.
/// - `must_suffix`: require a suffixed file when FILE has no suffix.
/// - `prefer_newer`: ignore suffix order and choose the newest suffixed file.
/// - default: search each load-path directory in order, trying `.elc` before
///   `.el`, then bare names when suffixless loading is allowed.
pub fn find_file_in_load_path_with_flags(
    name: &str,
    load_path: &[LispString],
    no_suffix: bool,
    must_suffix: bool,
    prefer_newer: bool,
) -> Option<PathBuf> {
    let name = LispString::from_utf8(name);
    find_lisp_file_in_load_path_with_flags(&name, load_path, no_suffix, must_suffix, prefer_newer)
        .map(|found| load_path_buf(&found))
}

fn find_lisp_file_in_load_path_with_flags(
    name: &LispString,
    load_path: &[LispString],
    no_suffix: bool,
    must_suffix: bool,
    prefer_newer: bool,
) -> Option<LispString> {
    let path = expand_tilde_path_buf(name);
    if path.is_absolute() {
        return find_for_base(&path, name, no_suffix, must_suffix, prefer_newer)
            .map(|found| load_path_lisp_string(&found));
    }

    // GNU keeps `ldefs-boot.el` as the curated bootstrap autoload surface and
    // only requires it to carry autoloads needed during source bootstrap.
    // When building the source bootstrap evaluator, prefer that curated file
    // over a potentially fresher runtime `loaddefs.el`, whose extra defcustom
    // surface can pull later Lisp (for example `electric` -> `nadvice`) before
    // early bootstrap files like `oclosure` have been loaded.
    if bootstrap_prefers_ldefs_boot()
        && !no_suffix
        && !must_suffix
        && matches!(name.as_bytes(), b"loaddefs" | b"loaddefs.el")
    {
        for dir in load_path {
            let bootstrap = load_path_buf(dir).join("ldefs-boot.el");
            if bootstrap.is_file() {
                return Some(load_path_lisp_string(&bootstrap));
            }
        }
    }

    // Emacs searches load-path directory-by-directory; suffix preference
    // is evaluated within each directory.
    for dir in load_path {
        let full = load_path_buf(dir).join(load_path_buf(name));
        if let Some(found) = find_for_base(&full, name, no_suffix, must_suffix, prefer_newer) {
            return Some(load_path_lisp_string(&found));
        }
    }

    None
}

/// Extract `load-path` from the evaluator's obarray as Lisp strings.
pub fn get_load_path(obarray: &super::symbol::Obarray) -> Vec<LispString> {
    let default_directory = obarray
        .symbol_value("default-directory")
        .and_then(|v| {
            v.is_string()
                .then(|| v.as_lisp_string().expect("checked string").clone())
        })
        .unwrap_or_else(|| LispString::from_unibyte(b".".to_vec()));

    let val = obarray
        .symbol_value("load-path")
        .cloned()
        .unwrap_or(Value::NIL);
    super::value::list_to_vec(&val)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| match v {
            v if v.is_nil() => Some(default_directory.clone()),
            _ if v.is_string() => v.as_lisp_string().cloned(),
            _ => None,
        })
        .collect()
}

pub(crate) enum LoadPlan {
    Return(Value),
    Load { found: LispString },
}

pub(crate) fn plan_load_in_state(
    obarray: &super::symbol::Obarray,
    file: Value,
    noerror: Option<Value>,
    nosuffix: Option<Value>,
    must_suffix: Option<Value>,
) -> Result<LoadPlan, Flow> {
    let file = match file.kind() {
        ValueKind::String => file.as_lisp_string().expect("checked string").clone(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), file],
            ));
        }
    };
    let noerror = noerror.is_some_and(|v| v.is_truthy());
    let nosuffix = nosuffix.is_some_and(|v| v.is_truthy());
    let must_suffix = must_suffix.is_some_and(|v| v.is_truthy());
    let prefer_newer = obarray
        .symbol_value("load-prefer-newer")
        .is_some_and(|v| v.is_truthy());

    let load_path = get_load_path(obarray);
    match find_lisp_file_in_load_path_with_flags(
        &file,
        &load_path,
        nosuffix,
        must_suffix,
        prefer_newer,
    ) {
        Some(found) => Ok(LoadPlan::Load { found }),
        None => {
            if noerror {
                Ok(LoadPlan::Return(Value::NIL))
            } else {
                Err(signal(
                    "file-missing",
                    vec![Value::string(format!(
                        "Cannot open load file: {}",
                        load_runtime_string(&file)
                    ))],
                ))
            }
        }
    }
}

pub(crate) fn builtin_load_in_vm_runtime(
    shared: &mut super::eval::Context,
    vm_gc_roots: &[Value],
    args: &[Value],
) -> Result<Value, Flow> {
    if args.is_empty() {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol("load"), Value::fixnum(0)],
        ));
    }

    match plan_load_in_state(
        &shared.obarray,
        args[0],
        args.get(1).copied(),
        args.get(3).copied(),
        args.get(4).copied(),
    )? {
        LoadPlan::Return(value) => Ok(value),
        LoadPlan::Load { found } => {
            let extra_roots = args.to_vec();
            let noerror = args.get(1).is_some_and(|v| v.is_truthy());
            let nomessage = args.get(2).is_some_and(|v| v.is_truthy());
            let path = load_path_buf(&found);
            shared.with_extra_gc_roots(vm_gc_roots, &extra_roots, move |eval| {
                load_file_with_found_flags(eval, &path, &found, noerror, nomessage).map_err(|e| {
                    match e {
                        EvalError::Signal {
                            symbol,
                            data,
                            raw_data,
                        } => Flow::Signal(crate::emacs_core::error::SignalData {
                            symbol,
                            data,
                            raw_data,
                            suppress_signal_hook: false,
                            selected_resume: None,
                            search_complete: false,
                        }),
                        EvalError::UncaughtThrow { tag, value } => {
                            crate::emacs_core::error::signal("no-catch", vec![tag, value])
                        }
                    }
                })
            })
        }
    }
}

pub(crate) const BOOTSTRAP_LOAD_PATH_SUBDIRS: &[&str] = &[
    "",
    "calendar",
    "emacs-lisp",
    "mail",
    "progmodes",
    "language",
    "international",
    "textmodes",
    "vc",
    "leim",
];

fn strip_utf8_bom(source: &str) -> &str {
    source.strip_prefix('\u{feff}').unwrap_or(source)
}

fn strip_reader_prefix(source: &str) -> (&str, bool) {
    let without_bom = strip_utf8_bom(source);
    if !without_bom.starts_with("#!") {
        return (without_bom, false);
    }

    match without_bom.find('\n') {
        Some(index) => (&without_bom[index + 1..], false),
        None => ("", true),
    }
}

fn lexical_binding_enabled_in_file_local_cookie_line(line: &str) -> bool {
    matches!(
        lexical_binding_cookie_in_file_local_cookie_line(line),
        LexicalBindingCookie::Lexical
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LexicalBindingCookie {
    None,
    Dynamic,
    Lexical,
}

fn lexical_binding_cookie_in_file_local_cookie_line(line: &str) -> LexicalBindingCookie {
    let Some(start) = line.find("-*-") else {
        return LexicalBindingCookie::None;
    };
    let rest = &line[start + 3..];
    let Some(end_rel) = rest.find("-*-") else {
        return LexicalBindingCookie::None;
    };
    let cookie = &rest[..end_rel];

    for entry in cookie.split(';') {
        let Some((name, value)) = entry.split_once(':') else {
            continue;
        };
        if name.trim() == "lexical-binding" {
            return if value.trim() == "t" {
                LexicalBindingCookie::Lexical
            } else {
                LexicalBindingCookie::Dynamic
            };
        }
    }
    LexicalBindingCookie::None
}

pub(crate) fn lexical_binding_cookie_for_source(source: &str) -> LexicalBindingCookie {
    let mut lines = strip_utf8_bom(source).lines();
    let first_line = lines.next();
    if let Some(cookie) = first_line.map(lexical_binding_cookie_in_file_local_cookie_line)
        && cookie != LexicalBindingCookie::None
    {
        return cookie;
    }

    if first_line.is_some_and(|line| line.starts_with("#!")) {
        return lines
            .next()
            .map(lexical_binding_cookie_in_file_local_cookie_line)
            .unwrap_or(LexicalBindingCookie::None);
    }

    LexicalBindingCookie::None
}

pub(crate) fn lexical_binding_enabled_for_source(source: &str) -> bool {
    matches!(
        lexical_binding_cookie_for_source(source),
        LexicalBindingCookie::Lexical
    )
}

fn default_toplevel_lexical_binding(eval: &super::eval::Context) -> bool {
    crate::emacs_core::eval::default_toplevel_value_in_state(
        &eval.obarray,
        eval.specpdl.as_slice(),
        intern("lexical-binding"),
    )
    .is_some_and(|value| value.is_truthy())
}

fn lexical_binding_from_cookie(
    eval: &mut super::eval::Context,
    cookie: LexicalBindingCookie,
    from: Option<Value>,
) -> Result<bool, EvalError> {
    match cookie {
        LexicalBindingCookie::Lexical => Ok(true),
        LexicalBindingCookie::Dynamic => Ok(false),
        LexicalBindingCookie::None => {
            let default = default_toplevel_lexical_binding(eval);
            let Some(from) = from else {
                return Ok(default);
            };
            let hook = eval
                .visible_variable_value_or_nil("internal--get-default-lexical-binding-function");
            if hook.is_nil() {
                return Ok(default);
            }

            let result = eval.with_gc_scope(|ctx| {
                ctx.root(hook);
                ctx.root(from);
                ctx.apply(hook, vec![from]).map_err(map_flow)
            });
            result.map(|value| value.is_truthy())
        }
    }
}

pub(crate) fn source_lexical_binding_for_load(
    eval: &mut super::eval::Context,
    source: &str,
    from: Option<Value>,
) -> Result<bool, EvalError> {
    lexical_binding_from_cookie(eval, lexical_binding_cookie_for_source(source), from)
}

fn is_unsupported_compiled_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    // .elc is now supported; only block compressed .elc.gz
    name.ends_with(".elc.gz")
}

/// Check if eager macro expansion is available.
/// Requires both `internal-macroexpand-for-load` and the pcase backquote
/// macroexpander (`--pcase-macroexpander`) to be defined, since
/// `macroexpand-all` uses pcase backquote patterns internally.
///
/// This mirrors GNU `loadup.el`, which loads `macroexp`, then loads `pcase`
/// under `(let ((macroexp--pending-eager-loads '(skip))) ...)`, then reloads
/// `macroexp` once the pcase backquote macroexpander exists.
#[tracing::instrument(level = "debug", skip(eval))]
pub(crate) fn get_eager_macroexpand_fn(eval: &super::eval::Context) -> Option<Value> {
    // Respect the Elisp `macroexp--pending-eager-loads` variable.
    // When it starts with `skip`, eager expansion is suppressed (mirrors
    // the check in `internal-macroexpand-for-load` in macroexp.el).
    if let Some(val) = eval.obarray().symbol_value("macroexp--pending-eager-loads") {
        if val.is_cons() {
            if val.cons_car().is_symbol_named("skip") {
                return None;
            }
        }
    }
    // Guard: pcase ` macroexpander must be available
    eval.obarray().symbol_function("`--pcase-macroexpander")?;
    let f = eval
        .obarray()
        .symbol_function("internal-macroexpand-for-load")
        .cloned()?;
    // Guard: if the function cell was set to nil (e.g. via fset), treat as unavailable
    if f.is_nil() {
        return None;
    }
    Some(f)
}

/// Port of real Emacs's `readevalloop_eager_expand_eval` from lread.c.
///
/// Algorithm (matching real Emacs lread.c lines 2013-2032):
/// 1. `val = macroexpand(val, nil)` — one-level expand, mutating `val`
/// 2. If `val` is `(progn ...)`, recurse into each subform
/// 3. Otherwise `eval(macroexpand(val, t))` — full expand the ALREADY
///    one-level-expanded `val`, then eval
///
/// This ensures all macros (including `pcase` inside function bodies) are
/// expanded at load time, preventing combinatorial re-expansion at runtime.
///
/// **Cycle/failure recovery**: NeoVM loads .el source files, not .elc
/// compiled files. This means eager expansion encounters circular require
/// chains (e.g. cl-lib ↔ cl-generic ↔ seq) that real Emacs avoids because
/// .elc files don't need eager expansion. When expansion fails (cycle
/// detection, missing macros, etc.), we fall back to evaluating the form
/// without eager expansion — matching the behavior of loading .elc files.
#[tracing::instrument(level = "debug", skip(eval, form_value, macroexpand_fn, sink))]
pub(crate) fn eager_expand_toplevel_forms(
    eval: &mut super::eval::Context,
    form_value: Value,
    macroexpand_fn: Value,
    sink: &mut impl FnMut(&mut super::eval::Context, Value, Value, bool) -> Result<Value, EvalError>,
) -> Result<Value, EvalError> {
    eager_expand_toplevel_forms_with_extra_roots(
        eval,
        form_value,
        macroexpand_fn,
        &mut |_ctx| {},
        sink,
    )
}

pub(crate) fn eager_expand_toplevel_forms_with_extra_roots(
    eval: &mut super::eval::Context,
    form_value: Value,
    macroexpand_fn: Value,
    extra_roots: &mut impl FnMut(&mut super::eval::Context),
    sink: &mut impl FnMut(&mut super::eval::Context, Value, Value, bool) -> Result<Value, EvalError>,
) -> Result<Value, EvalError> {
    let original_form = form_value;
    let mutation_epoch_before = eval.macro_expansion_mutation_epoch();
    // Step 1: one-level expand — val = (internal-macroexpand-for-load val nil)
    // Note: real Emacs mutates `val` here; we shadow it.
    let step1_start = std::time::Instant::now();
    let val = eval.with_gc_scope(|ctx| {
        extra_roots(ctx);
        ctx.root(form_value);
        ctx.root(macroexpand_fn);
        // `internal-macroexpand-for-load` is an internal loader helper.
        // Its failures are handled here and its frames are not part of the
        // user-facing loaded form surface, so avoid paying full backtrace
        // bookkeeping on every eager expansion call.
        ctx.apply(macroexpand_fn, vec![form_value, Value::NIL]).ok()
    });
    eval.note_eager_macro_perf_step1(step1_start.elapsed());
    let val = match val {
        Some(v) => v,
        None => {
            // Eager expansion failed (cycle detection, missing macro, etc.).
            // Fall back to evaluating the original form without expansion.
            // This matches .elc behavior where forms are already compiled.
            tracing::debug!("eager_expand step1 failed, falling back to plain eval");
            return eval.with_gc_scope(|ctx| {
                extra_roots(ctx);
                ctx.root(form_value);
                sink(ctx, original_form, form_value, false)
            });
        }
    };

    // Step 2: if result is (progn ...), recurse into subforms.
    // Root `val` during iteration: the recursive `eager_expand_eval`
    // call triggers evaluation + GC, which could free val's cons cells.
    if val.is_cons() {
        let car = val.cons_car();
        let cdr = val.cons_cdr();
        if car.is_symbol_named("progn") {
            return eval.with_gc_scope(|ctx| {
                extra_roots(ctx);
                ctx.root(val);
                let mut result = Value::NIL;
                let mut tail = cdr;
                while tail.is_cons() {
                    let sub_form = tail.cons_car();
                    tail = tail.cons_cdr();
                    result = eager_expand_toplevel_forms_with_extra_roots(
                        ctx,
                        sub_form,
                        macroexpand_fn,
                        extra_roots,
                        sink,
                    )?;
                }
                Ok(result)
            });
        }
    }

    // Step 3+4: deep expand then eval —
    // GNU lread.c:2030: val = eval_sub(calln(Qmacroexpand, val, Qt));
    // where Qmacroexpand = Qinternal_macroexpand_for_load (set at line 2184).
    // Calling internal-macroexpand-for-load(val, t) with full-p=t triggers
    // macroexpand--all-toplevel (deep/recursive expansion via macroexpand-all).
    eval.with_gc_scope(|ctx| {
        extra_roots(ctx);
        ctx.root(val);
        ctx.root(macroexpand_fn);
        ctx.root(original_form);
        let t3 = std::time::Instant::now();
        // Call internal-macroexpand-for-load(val, t) — full-p=t means deep expand
        let expanded = match ctx.apply(macroexpand_fn, vec![val, Value::T]) {
            Ok(v) => v,
            Err(e) => {
                // Full expansion failed; use the one-level-expanded form.
                let form_str = super::print::print_value(&val);
                let form_preview: String = form_str.chars().take(200).collect();
                tracing::debug!("eager_expand step3 failed: {e:?} form={form_preview}");
                val
            }
        };
        let d3 = t3.elapsed();
        ctx.note_eager_macro_perf_step3(d3);
        if ctx.macro_perf_enabled() && d3.as_millis() > 200 {
            let head = if val.is_cons() {
                val.cons_car().as_symbol_name().unwrap_or("<non-symbol>")
            } else {
                "<atom>"
            };
            let form_str = super::print::print_value(&val);
            let form_preview: String = form_str.chars().take(200).collect();
            tracing::warn!(
                "eager_expand step3 (full-expand) took {d3:.2?} head={head} form={form_preview}"
            );
        }
        let requires_eager_replay = ctx.macro_expansion_mutation_epoch() != mutation_epoch_before
            || cached_form_requires_eager_replay(original_form)
            || cached_form_requires_eager_replay(val)
            || cached_form_requires_eager_replay(expanded);
        ctx.root(expanded);
        sink(ctx, original_form, expanded, requires_eager_replay)
    })
}

#[tracing::instrument(level = "debug", skip(eval, form_value, macroexpand_fn))]
pub(crate) fn eager_expand_eval(
    eval: &mut super::eval::Context,
    form_value: Value,
    macroexpand_fn: Value,
) -> Result<Value, EvalError> {
    eager_expand_toplevel_forms(
        eval,
        form_value,
        macroexpand_fn,
        &mut |ctx, _original, expanded, _requires_eager_replay| {
            ctx.with_gc_scope(|ctx| {
                ctx.root(expanded);
                let t4 = std::time::Instant::now();
                let value = ctx.eval_value(&expanded).map_err(map_flow)?;
                let d4 = t4.elapsed();
                ctx.note_eager_macro_perf_step4(d4);
                if ctx.macro_perf_enabled() && d4.as_millis() > 200 {
                    tracing::warn!("eager_expand step4 (eval) took {d4:.2?}");
                }
                Ok(value)
            })
        },
    )
}

/// Shared context save/restore for file loading.
///
/// Saves and restores: lexical-binding, lexenv, load-file-name,
/// load-true-file-name, current-load-list, temp roots.
/// Sets lexical-binding from the file cookie and load-bound filename metadata.
/// The `body` closure runs with the new context and its result is returned
/// after context restoration.
fn with_load_context<F>(
    eval: &mut super::eval::Context,
    found: &LispString,
    lexical_binding: bool,
    body: F,
) -> Result<Value, EvalError>
where
    F: FnOnce(&mut super::eval::Context) -> Result<Value, EvalError>,
{
    let old_lexical = eval.lexical_binding();
    let old_lexenv = eval.lexenv;
    let old_load_file = eval.obarray().symbol_value("load-file-name").cloned();
    let old_load_true_file = eval.obarray().symbol_value("load-true-file-name").cloned();
    let old_current_load_list = eval.obarray().symbol_value("current-load-list").cloned();
    let old_reader_load_file = super::value_reader::get_reader_load_file_name_public();

    eval.with_gc_scope(|ctx| {
        ctx.root(old_lexenv);
        if let Some(ref v) = old_load_file {
            ctx.root(*v);
        }
        if let Some(ref v) = old_load_true_file {
            ctx.root(*v);
        }
        if let Some(ref v) = old_current_load_list {
            ctx.root(*v);
        }
        if let Some(v) = old_reader_load_file {
            ctx.root(v);
        }

        if lexical_binding {
            ctx.set_lexical_binding(true);
            ctx.lexenv = Value::list(vec![Value::T]);
        }

        let load_file_value = Value::heap_string(found.clone());
        ctx.root(load_file_value);
        let load_true_file_value = load_file_value;
        let current_load_list = Value::cons(load_file_value, Value::NIL);
        ctx.root(current_load_list);
        ctx.set_variable("load-file-name", load_file_value);
        ctx.set_variable("load-true-file-name", load_true_file_value);
        ctx.set_variable("current-load-list", current_load_list);
        // Set the reader's #$ thread-local so value_reader produces the
        // actual file path string (matching GNU lread.c Vload_file_name).
        super::value_reader::set_reader_load_file_name(Some(load_file_value));

        let result = body(ctx);

        // Restore reader load-file-name
        super::value_reader::set_reader_load_file_name(old_reader_load_file);

        ctx.set_lexical_binding(old_lexical);
        ctx.lexenv = old_lexenv;
        if let Some(old) = old_load_file {
            ctx.set_variable("load-file-name", old);
        } else {
            ctx.set_variable("load-file-name", Value::NIL);
        }
        if let Some(old) = old_load_true_file {
            ctx.set_variable("load-true-file-name", old);
        } else {
            ctx.set_variable("load-true-file-name", Value::NIL);
        }
        if let Some(old) = old_current_load_list {
            ctx.set_variable("current-load-list", old);
        } else {
            ctx.set_variable("current-load-list", Value::NIL);
        }

        result
    })
}

/// GNU-style streaming read-eval loop using the Value-native reader.
///
/// Reads one form at a time from `content`, optionally macro-expands it via
/// `macroexpand_fn`, evaluates it, then advances to the next form. No
/// parse-all-first, no compilation cache, no macro expansion cache.
///
/// This matches the structure of `readevalloop` in GNU Emacs `lread.c`.
fn streaming_readevalloop(
    eval: &mut super::eval::Context,
    path: &Path,
    hist_file_name: &LispString,
    content: &str,
    source_multibyte: bool,
    macroexpand_fn: Option<Value>,
) -> Result<Value, EvalError> {
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let mut pos = 0;
    let mut form_idx = 0;

    loop {
        let read_result =
            super::value_reader::read_one_with_source_multibyte(content, source_multibyte, pos)
                .map_err(|e| {
                    // GNU `Fload` (`src/lread.c`) signals `end-of-file`
                    // when the reader hits EOF mid-form (e.g. a single-line
                    // shebang that exhausts the file with no trailing
                    // newline before any forms). Mirror that here.
                    if e.message.contains("end of input") || e.message.contains("unterminated") {
                        return EvalError::Signal {
                            symbol: intern("end-of-file"),
                            data: vec![],
                            raw_data: None,
                        };
                    }
                    EvalError::Signal {
                        symbol: intern("error"),
                        data: vec![Value::string(format!(
                            "Read error in {}: {} at position {}",
                            path.display(),
                            e.message,
                            e.position
                        ))],
                        raw_data: None,
                    }
                })?;

        let Some((form, next_pos)) = read_result else {
            break; // EOF
        };

        // Log a preview of the form source text.
        let form_start = pos;
        pos = next_pos;

        if tracing::enabled!(tracing::Level::DEBUG) {
            let preview: String = content[form_start..next_pos].chars().take(80).collect();
            tracing::debug!("{} FORM[{}/streaming]: {}", file_name, form_idx, preview,);
        }

        // Root the form value so it survives any GC triggered during
        // macro-expansion or evaluation.
        let saved_temp_roots = eval.save_temp_roots();
        eval.push_temp_root(form);

        let eval_result = if let Some(mexp) = macroexpand_fn {
            // GNU-style eager expand: one level, recurse for progn,
            // full expand + eval.
            streaming_readevalloop_eager_expand_eval(eval, form, mexp)
        } else {
            eval.eval_sub(form).map_err(map_flow)
        };

        eval.restore_temp_roots(saved_temp_roots);

        // Report errors with human-readable detail.
        if let Err(ref e) = eval_result {
            let err_detail = match e {
                EvalError::Signal {
                    symbol,
                    data,
                    raw_data,
                } => {
                    let sym_name = super::intern::resolve_sym(*symbol);
                    let payload = if let Some(raw) = raw_data {
                        format_value_for_error(raw)
                    } else if data.is_empty() {
                        "nil".to_string()
                    } else {
                        let data_strs: Vec<String> =
                            data.iter().map(|v| format_value_for_error(v)).collect();
                        format!("({})", data_strs.join(" "))
                    };
                    format!("({} {})", sym_name, payload)
                }
                other => format!("{:?}", other),
            };
            let preview: String = content[form_start..next_pos].chars().take(120).collect();
            tracing::error!(
                "  !! {} FORM[{}] FAILED: {} => {}",
                file_name,
                form_idx,
                preview,
                err_detail,
            );
            // Dump Lisp backtrace (like GNU's debug-early-backtrace)
            if !eval.runtime_backtrace.is_empty() {
                tracing::error!("  Lisp backtrace:");
                for (j, frame) in eval.runtime_backtrace.iter().rev().enumerate() {
                    let func_name = super::print::print_value(&frame.function);
                    let args_str = frame
                        .args()
                        .iter()
                        .take(4)
                        .map(|a| {
                            let s = super::print::print_value(a);
                            if s.len() > 40 {
                                format!("{}...", &s[..37])
                            } else {
                                s
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    let ellipsis = if frame.args_len() > 4 { " ..." } else { "" };
                    tracing::error!("    {j}: ({func_name} {args_str}{ellipsis})");
                    if j >= 20 {
                        tracing::error!(
                            "    ... ({} more frames)",
                            eval.runtime_backtrace.len() - j - 1
                        );
                        break;
                    }
                }
            }
        }
        eval_result?;

        // GNU keeps the current top-level form protected across the
        // post-form GC in readevalloop. Exact GC needs the same root here:
        // freshly installed closures/macros can still share structure with the
        // just-evaluated source form.
        eval.gc_safe_point_exact_with_extra_roots(&[form]);
        form_idx += 1;
    }

    record_load_history(eval, hist_file_name);
    Ok(Value::T)
}

/// GNU-style eager macro expansion during streaming load.
///
/// Matches `readevalloop_eager_expand_eval` in lread.c:
/// 1. One-level macroexpand via `internal-macroexpand-for-load(form, nil)`
/// 2. If result is `(progn ...)`, iterate subforms (recurse for each)
/// 3. Otherwise, full macroexpand via `internal-macroexpand-for-load(form, t)`
///    then eval the result.
fn streaming_readevalloop_eager_expand_eval(
    eval: &mut super::eval::Context,
    form: Value,
    macroexpand: Value,
) -> Result<Value, EvalError> {
    // Step 1: one-level expand (full_p = nil)
    let step1_start = std::time::Instant::now();
    let expanded = match eval.apply(macroexpand, vec![form, Value::NIL]) {
        Ok(v) => v,
        Err(_) => {
            // Expansion failed (cycle detection, missing macro, etc.).
            // Fall back to evaluating the original form without expansion,
            // matching .elc behavior.
            eval.note_eager_macro_perf_step1(step1_start.elapsed());
            tracing::debug!("streaming eager_expand step1 failed, falling back to plain eval");
            return eval.eval_sub(form).map_err(map_flow);
        }
    };
    eval.note_eager_macro_perf_step1(step1_start.elapsed());

    // Root the expanded form so it survives GC during progn iteration.
    let saved_temp_roots = eval.save_temp_roots();
    eval.push_temp_root(expanded);

    let result = streaming_readevalloop_eager_expand_eval_inner(eval, expanded, macroexpand);

    eval.restore_temp_roots(saved_temp_roots);
    result
}

/// Inner helper for eager expansion: handles progn unwinding and full expansion.
fn streaming_readevalloop_eager_expand_eval_inner(
    eval: &mut super::eval::Context,
    expanded: Value,
    macroexpand: Value,
) -> Result<Value, EvalError> {
    // Step 2: if (progn ...), iterate subforms
    if expanded.is_cons() && expanded.cons_car().is_symbol_named("progn") {
        let mut cursor = expanded.cons_cdr();
        let mut last_val = Value::NIL;
        while cursor.is_cons() {
            let subform = cursor.cons_car();
            cursor = cursor.cons_cdr();
            // Root cursor across recursive expansion+eval (it's a cons tail
            // that could be collected if we don't protect it).
            let saved = eval.save_temp_roots();
            eval.push_temp_root(cursor);
            last_val = streaming_readevalloop_eager_expand_eval(eval, subform, macroexpand)?;
            eval.restore_temp_roots(saved);
        }
        return Ok(last_val);
    }

    // Step 3: full expand (full_p = t), then eval
    let step3_start = std::time::Instant::now();
    let fully_expanded = match eval.apply(macroexpand, vec![expanded, Value::T]) {
        Ok(v) => v,
        Err(_) => {
            // Full expansion failed; use the one-level-expanded form.
            tracing::debug!("streaming eager_expand step3 failed, using one-level expansion");
            expanded
        }
    };
    eval.note_eager_macro_perf_step3(step3_start.elapsed());

    let saved = eval.save_temp_roots();
    eval.push_temp_root(fully_expanded);
    let step4_start = std::time::Instant::now();
    let result = eval.eval_sub(fully_expanded).map_err(map_flow);
    eval.note_eager_macro_perf_step4(step4_start.elapsed());
    eval.restore_temp_roots(saved);
    result
}

/// Load and evaluate a file. Returns the last result.
#[tracing::instrument(level = "info", skip(eval), err(Debug))]
pub fn load_file(eval: &mut super::eval::Context, path: &Path) -> Result<Value, EvalError> {
    load_file_with_flags(eval, path, false, false)
}

/// Load and evaluate a file with the caller-visible `load` flags.
#[tracing::instrument(level = "info", skip(eval), err(Debug))]
pub fn load_file_with_flags(
    eval: &mut super::eval::Context,
    path: &Path,
    noerror: bool,
    nomessage: bool,
) -> Result<Value, EvalError> {
    // Expand tilde in case the path comes from Elisp with ~ prefix
    let expanded = expand_tilde(&path.to_string_lossy());
    let path = std::path::Path::new(&expanded);
    let found = load_path_lisp_string(path);
    load_file_with_found_flags(eval, path, &found, noerror, nomessage)
}

pub(crate) fn load_file_with_found_flags(
    eval: &mut super::eval::Context,
    path: &Path,
    found: &LispString,
    noerror: bool,
    nomessage: bool,
) -> Result<Value, EvalError> {
    if is_unsupported_compiled_path(path) {
        return Err(EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "Loading compressed compiled Elisp artifacts (.elc.gz) is unsupported in neomacs: {}",
                path.display()
            ))],
            raw_data: None,
        });
    }

    // GNU Emacs only signals `Recursive load` once the same found filename is
    // already present four times in `Vloads_in_progress`, i.e. on the fifth
    // attempt. Matching that behavior matters because Lisp depends on the
    // textual `found` identity, not on canonicalized host paths.
    let load_count = eval
        .loads_in_progress
        .iter()
        .filter(|p| *p == found)
        .count();
    if load_count > 3 {
        let found_value = Value::heap_string(found.clone());
        let in_progress = Value::list(
            eval.loads_in_progress
                .iter()
                .cloned()
                .map(Value::heap_string)
                .collect(),
        );
        return Err(EvalError::Signal {
            symbol: intern("error"),
            data: vec![
                Value::string("Recursive load"),
                Value::cons(found_value, in_progress),
            ],
            raw_data: None,
        });
    }
    eval.loads_in_progress.push(found.clone());

    // GNU Emacs lread.c: specbind(Qload_in_progress, Qt)
    // Set load-in-progress to t during file loading, restore afterward.
    let old_load_in_progress = eval
        .obarray()
        .symbol_value("load-in-progress")
        .cloned()
        .unwrap_or(Value::NIL);
    eval.set_variable("load-in-progress", Value::T);

    let result = stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        load_file_body(eval, path, found, noerror, nomessage)
    });

    eval.set_variable("load-in-progress", old_load_in_progress);
    eval.loads_in_progress.pop();
    result
}

fn load_file_body(
    eval: &mut super::eval::Context,
    path: &Path,
    found: &LispString,
    noerror: bool,
    nomessage: bool,
) -> Result<Value, EvalError> {
    let is_elc = path.extension().and_then(|e| e.to_str()) == Some("elc");

    if !is_elc
        && let load_source_file_function =
            eval.visible_variable_value_or_nil("load-source-file-function")
        && !load_source_file_function.is_nil()
    {
        let full_name = Value::heap_string(found.clone());
        let hist_file_name = full_name;
        return eval
            .apply(
                load_source_file_function,
                vec![
                    full_name,
                    hist_file_name,
                    Value::bool_val(noerror),
                    Value::bool_val(nomessage),
                ],
            )
            .map_err(crate::emacs_core::error::map_flow);
    }

    // Read raw bytes and decode (with Emacs-extended UTF-8 for .el,
    // or header-skipping for .elc).
    let raw_bytes = std::fs::read(path).map_err(|e| EvalError::Signal {
        symbol: intern("file-error"),
        data: vec![Value::string(format!(
            "Cannot read file: {}: {}",
            path.display(),
            e
        ))],
        raw_data: None,
    })?;

    // For .elc: skip the ;ELC magic header and detect lexical-binding from raw bytes.
    // For .el: decode Emacs-extended UTF-8.
    let (content, source_multibyte) = if is_elc {
        (skip_elc_header(&raw_bytes), false)
    } else {
        // GNU `Fload` (`src/lread.c`) lets the coding system swallow
        // a leading UTF-8 BOM (U+FEFF). NeoVM's reader does not, so
        // strip it here before the streaming reader sees the source —
        // otherwise the BOM is parsed as a one-character symbol and
        // signals `void-variable`.
        let decoded = decode_emacs_utf8(&raw_bytes);
        (
            match decoded.strip_prefix('\u{feff}') {
                Some(rest) => rest.to_string(),
                None => decoded,
            },
            true,
        )
    };

    // Detect lexical-binding.
    let lexical_binding = if is_elc {
        elc_has_lexical_binding(&raw_bytes)
    } else {
        source_lexical_binding_for_load(eval, &content, Some(Value::heap_string(found.clone())))?
    };

    // --- Shared context setup via with_load_context ---
    with_load_context(eval, found, lexical_binding, |eval| {
        // Both .el and .elc use the streaming Value reader.
        // .el files get eager macro expansion; .elc files are already compiled
        // so no expansion is needed (macroexpand_fn = None).  The reader
        // converts #[...] syntax to ByteCode values directly (like GNU Emacs).
        let macroexpand_fn = if is_elc {
            None
        } else {
            get_eager_macroexpand_fn(eval)
        };

        streaming_readevalloop(
            eval,
            path,
            found,
            &content,
            source_multibyte,
            macroexpand_fn,
        )
    })
}

pub(crate) fn eval_decoded_source_file_in_context(
    eval: &mut super::eval::Context,
    path: &Path,
    content: &str,
    source_multibyte: bool,
) -> Result<Value, EvalError> {
    // Use the streaming Value-reader path (no Expr intermediate).
    let macroexpand_fn = get_eager_macroexpand_fn(eval);
    let found = load_path_lisp_string(path);
    streaming_readevalloop(
        eval,
        path,
        &found,
        content,
        source_multibyte,
        macroexpand_fn,
    )
}

/// Skip the `;ELC` magic header in a byte-compiled Elisp file.
/// Returns the remaining content as a string.
fn skip_elc_header(raw_bytes: &[u8]) -> String {
    // .elc files start with ";ELC" magic bytes (0x3B 0x45 0x4C 0x43)
    // followed by version bytes (typically 0x1C 0x00 0x00 0x00 for Emacs 28+).
    // Then comment lines starting with ";;".
    //
    // We need to skip all bytes up to the first non-comment line.
    //
    // GNU Emacs `.elc` files mix ASCII source (defvar, defun, etc.) with
    // unibyte bytecode strings inside `#[...]` byte-code-function literals.
    // The bytecode strings contain raw bytes 0x00-0xFF where bytes >= 0x80
    // are NOT valid UTF-8 starts (e.g., 0xC0 0x87 = `constant 0; return`).
    //
    // We CANNOT use `decode_emacs_utf8` here because it replaces non-UTF-8
    // bytes with U+FFFD or escapes, corrupting the bytecode.  Instead, use
    // Latin-1 encoding: each raw byte 0-255 becomes the Unicode code point
    // with the same value, encoded as UTF-8 in the resulting Rust String.
    // This preserves all 256 byte values losslessly, and `string_value_to_bytes`
    // (which truncates each char to u8) recovers the original bytes exactly.
    let content: String = raw_bytes.iter().map(|&b| b as char).collect();
    let mut start = 0;

    // Skip bytes until we find the first line that doesn't start with ';' or
    // is not a special header byte. The magic is ";ELC" + 4 version bytes.
    let bytes = content.as_bytes();

    // First, skip the 8-byte magic header if present
    if bytes.starts_with(b";ELC") && bytes.len() >= 8 {
        start = 8;
        // Skip any additional non-printable/non-newline header bytes
        while start < bytes.len() && bytes[start] != b'\n' && bytes[start] != b';' {
            start += 1;
        }
    }

    // Now skip comment lines
    while start < bytes.len() {
        if bytes[start] == b'\n' {
            start += 1;
            continue;
        }
        if bytes[start] == b';' {
            // Skip to end of line
            while start < bytes.len() && bytes[start] != b'\n' {
                start += 1;
            }
            continue;
        }
        break;
    }

    content[start..].to_string()
}

/// Check if an .elc file has lexical-binding enabled in its header.
fn elc_has_lexical_binding(raw_bytes: &[u8]) -> bool {
    // Look for "lexical-binding: t" in the first few lines (header area)
    let preview = std::str::from_utf8(&raw_bytes[..raw_bytes.len().min(1024)]).unwrap_or("");
    preview.contains("lexical-binding: t")
}

fn record_load_history(eval: &mut super::eval::Context, path_lisp: &LispString) {
    let path_str = load_runtime_string(path_lisp);
    tracing::debug!("record_load_history: {}", path_str);
    eval.with_gc_scope(|eval| {
        // GNU protects the same post-load temporaries with GCPRO/specpdl roots
        // in lread.c. Exact GC needs explicit rooting here as well.
        let path_value = eval.root(Value::heap_string(path_lisp.clone()));
        let entry = eval.root(Value::cons(path_value, Value::NIL));
        let history = eval
            .obarray()
            .symbol_value("load-history")
            .cloned()
            .unwrap_or(Value::NIL);
        let filtered_history = eval.root(Value::list(
            list_to_vec(&history)
                .unwrap_or_default()
                .into_iter()
                .filter(|existing| {
                    if existing.is_cons() {
                        existing
                            .cons_car()
                            .as_lisp_string()
                            .is_none_or(|loaded| loaded != path_lisp)
                    } else {
                        true
                    }
                })
                .collect(),
        ));
        let updated_history = eval.root(Value::cons(entry, filtered_history));
        eval.set_variable("load-history", updated_history);

        // GNU Emacs lread.c:1540-1541: after loading a file, call
        // (do-after-load-evaluation FILENAME) to run eval-after-load hooks.
        let dale_id = super::intern::intern("do-after-load-evaluation");
        let is_fboundp = eval
            .obarray()
            .symbol_function_id(dale_id)
            .is_some_and(|f| !f.is_nil());
        if is_fboundp {
            let abs_path = eval.root(Value::heap_string(path_lisp.clone()));
            if let Err(e) = eval.apply(Value::symbol(dale_id), vec![abs_path]) {
                let err_msg = match &e {
                    super::error::Flow::Signal(sig) => {
                        let sym = super::intern::resolve_sym(sig.symbol);
                        let data: Vec<String> =
                            sig.data.iter().map(|v| format_value_for_error(v)).collect();
                        format!("({} {})", sym, data.join(" "))
                    }
                    other => format!("{other:?}"),
                };
                tracing::warn!(
                    "do-after-load-evaluation error for {}: {}",
                    path_str,
                    err_msg
                );
            }
        }
    });
}

/// Register bootstrap variables owned by the file-loading subsystem.
pub fn register_bootstrap_vars(obarray: &mut super::symbol::Obarray) {
    obarray.set_symbol_value("after-load-alist", Value::NIL);
    obarray.make_special("after-load-alist");
    obarray.set_symbol_value("macroexp--dynvars", Value::NIL);
    obarray.make_special("macroexp--dynvars");
}

/// Create an Context with the full Emacs bootstrap loaded (like GNU
/// Emacs's dumped state).  Mirrors the loadup.el boot sequence.
fn normalized_bootstrap_features(extra_features: &[&str]) -> Vec<String> {
    let mut features = extra_features
        .iter()
        .map(|feature| (*feature).to_string())
        .filter(|feature| !feature.is_empty())
        .collect::<Vec<_>>();
    features.sort_unstable();
    features.dedup();
    features
}

// Bump when bootstrap image semantics change in ways an older dump cannot
// represent correctly. V16 invalidates older caches because category-table
// ownership moved from a parallel manager into dumped Lisp objects.
const BOOTSTRAP_IMAGE_SCHEMA_VERSION: u32 = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadupDumpMode {
    Pbootstrap,
    Pdump,
}

impl LoadupDumpMode {
    pub const fn as_gnu_string(self) -> &'static str {
        match self {
            Self::Pbootstrap => "pbootstrap",
            Self::Pdump => "pdump",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadupStartupSurface {
    pub command_line_args: Vec<String>,
    pub noninteractive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeImageRole {
    Bootstrap,
    Final,
}

impl RuntimeImageRole {
    pub const fn canonical_image_stem(self) -> &'static str {
        match self {
            Self::Bootstrap => "bootstrap-neomacs",
            Self::Final => "neomacs",
        }
    }

    pub const fn image_file_name(self) -> &'static str {
        match self {
            Self::Bootstrap => "bootstrap-neomacs.pdump",
            Self::Final => "neomacs.pdump",
        }
    }

    pub fn fingerprinted_image_file_name(self) -> String {
        format!(
            "{}-{}.pdump",
            self.canonical_image_stem(),
            super::pdump::fingerprint_hex()
        )
    }
}
const RUNTIME_ROOT_ENV: &str = "NEOMACS_RUNTIME_ROOT";
const BOOTSTRAP_CACHE_DIR_ENV: &str = "NEOVM_BOOTSTRAP_CACHE_DIR";

fn compile_time_project_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().expect("project root").to_path_buf()
}

fn is_runtime_root(path: &Path) -> bool {
    path.join("lisp").is_dir() && path.join("etc").is_dir()
}

fn runtime_project_root() -> PathBuf {
    if let Ok(root) = std::env::var(RUNTIME_ROOT_ENV) {
        let path = PathBuf::from(root);
        if is_runtime_root(&path) {
            return path;
        }
        tracing::warn!(
            "{RUNTIME_ROOT_ENV}={} does not contain lisp/ and etc/; falling back",
            path.display()
        );
    }

    let compile_root = compile_time_project_root();
    if is_runtime_root(&compile_root) {
        return compile_root;
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(prefix) = exe.parent().and_then(Path::parent)
    {
        for candidate in [
            prefix.join("share/neomacs"),
            prefix.join("Resources/neomacs"),
        ] {
            if is_runtime_root(&candidate) {
                return candidate;
            }
        }
    }

    panic!(
        "Neomacs runtime root not found. Set {RUNTIME_ROOT_ENV} to a directory containing lisp/ and etc/."
    );
}

fn bootstrap_cache_dir(runtime_root: &Path) -> PathBuf {
    if let Ok(dir) = std::env::var(BOOTSTRAP_CACHE_DIR_ENV)
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }

    let compile_root = compile_time_project_root();
    if runtime_root == compile_root {
        return compile_root.join("target");
    }

    if let Ok(dir) = std::env::var("XDG_CACHE_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("neomacs");
    }

    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(".cache/neomacs");
    }

    std::env::temp_dir().join("neomacs")
}

fn should_hash_bootstrap_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("el") | Some("elc")
    )
}

fn collect_bootstrap_source_files(path: &Path, out: &mut Vec<PathBuf>) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };

    if metadata.is_file() {
        if should_hash_bootstrap_source_file(path) {
            out.push(path.to_path_buf());
        }
        return;
    }

    let Ok(entries) = fs::read_dir(path) else {
        return;
    };

    let mut children = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect::<Vec<_>>();
    children.sort();
    for child in children {
        collect_bootstrap_source_files(&child, out);
    }
}

fn bootstrap_source_fingerprint(runtime_root: &Path) -> String {
    let mut files = Vec::new();
    collect_bootstrap_source_files(&runtime_root.join("lisp"), &mut files);
    files.sort();

    let mut hasher = Sha256::new();
    hasher.update(b"neomacs-bootstrap-source-fingerprint-v1\0");
    for path in files {
        let rel = path.strip_prefix(runtime_root).unwrap_or(&path);
        hasher.update(rel.as_os_str().as_encoded_bytes());
        hasher.update([0]);
        match fs::read(&path) {
            Ok(bytes) => {
                hasher.update([1]);
                hasher.update(bytes);
            }
            Err(err) => {
                hasher.update([0]);
                hasher.update(err.to_string().as_bytes());
            }
        }
        hasher.update([0xff]);
    }

    let digest = hasher.finalize();
    format!("{:x}", digest)[..16].to_string()
}

fn bootstrap_dump_path(runtime_root: &Path, extra_features: &[&str]) -> PathBuf {
    let features = normalized_bootstrap_features(extra_features);
    let suffix = if features.is_empty() {
        String::new()
    } else {
        format!("-{}", features.join("-"))
    };
    let source_fingerprint = bootstrap_source_fingerprint(runtime_root);
    bootstrap_cache_dir(runtime_root).join(format!(
        "neovm-bootstrap-v{BOOTSTRAP_IMAGE_SCHEMA_VERSION}-{source_fingerprint}{suffix}.pdump"
    ))
}

fn runtime_image_stem_for_executable(executable: &Path, role: RuntimeImageRole) -> String {
    let file_name = executable
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(role.canonical_image_stem());
    file_name
        .strip_suffix(".exe")
        .unwrap_or(file_name)
        .to_string()
}

pub fn runtime_image_path_for_executable(executable: &Path, role: RuntimeImageRole) -> PathBuf {
    executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            "{}.pdump",
            runtime_image_stem_for_executable(executable, role)
        ))
}

pub fn fingerprinted_runtime_image_path_for_executable(
    executable: &Path,
    role: RuntimeImageRole,
) -> PathBuf {
    executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(role.fingerprinted_image_file_name())
}

pub fn default_runtime_image_path(role: RuntimeImageRole) -> PathBuf {
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok().or(Some(path)))
        .unwrap_or_else(|| PathBuf::from(role.image_file_name()));
    runtime_image_path_for_executable(&executable, role)
}

fn default_fingerprinted_runtime_image_path(role: RuntimeImageRole) -> PathBuf {
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok().or(Some(path)))
        .unwrap_or_else(|| PathBuf::from(role.image_file_name()));
    fingerprinted_runtime_image_path_for_executable(&executable, role)
}

fn runtime_image_candidate_paths_for_executable(
    executable: &Path,
    role: RuntimeImageRole,
) -> Vec<PathBuf> {
    let primary = runtime_image_path_for_executable(executable, role);
    let fingerprinted = fingerprinted_runtime_image_path_for_executable(executable, role);
    if primary == fingerprinted {
        vec![primary]
    } else {
        vec![primary, fingerprinted]
    }
}

fn bootstrap_dump_lock_path(dump_path: &Path) -> PathBuf {
    let file_name = dump_path
        .file_name()
        .expect("bootstrap dump path should have file name");
    let mut lock_name = file_name.to_os_string();
    lock_name.push(".lock");
    dump_path.with_file_name(lock_name)
}

struct BootstrapCacheWriteLock {
    #[cfg(unix)]
    file: std::fs::File,
}

impl BootstrapCacheWriteLock {
    fn acquire(lock_path: &Path) -> Result<Self, String> {
        if let Some(parent) = lock_path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|err| {
                format!(
                    "bootstrap cache lock: failed creating {}: {err}",
                    parent.display()
                )
            })?;
        }

        #[cfg(unix)]
        {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(lock_path)
                .map_err(|err| {
                    format!(
                        "bootstrap cache lock: failed opening {}: {err}",
                        lock_path.display()
                    )
                })?;

            // Serialize cache creation/repair across processes while keeping
            // ordinary pdump reads lock-free.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                if matches!(err.raw_os_error(), Some(libc::EWOULDBLOCK)) {
                    return Err(format!(
                        "bootstrap cache lock busy at {}",
                        lock_path.display()
                    ));
                }
                return Err(format!(
                    "bootstrap cache lock: failed locking {}: {}",
                    lock_path.display(),
                    err
                ));
            }

            Ok(Self { file })
        }

        #[cfg(not(unix))]
        {
            let _ = lock_path;
            Ok(Self {})
        }
    }
}

#[cfg(unix)]
impl Drop for BootstrapCacheWriteLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn ensure_startup_compat_variables(eval: &mut super::eval::Context, project_root: &Path) {
    let etc_dir = format!("{}/", project_root.join("etc").to_string_lossy());
    let source_dir = format!("{}/", project_root.to_string_lossy());
    let temporary_file_directory = std::env::temp_dir().to_string_lossy().to_string();
    let path_separator = if cfg!(windows) { ";" } else { ":" };
    let process_environment = Value::list(
        std::env::vars()
            .map(|(name, value)| Value::string(format!("{name}={value}")))
            .collect::<Vec<_>>(),
    );
    let system_name = super::builtins_extra::builtin_system_name(vec![])
        .unwrap_or_else(|_| Value::string("localhost"));
    let user_full_name = super::builtins_extra::builtin_user_full_name(vec![])
        .unwrap_or_else(|_| Value::string("unknown"));
    let user_login_name = super::builtins_extra::builtin_user_login_name(vec![])
        .unwrap_or_else(|_| Value::string("unknown"));
    let user_real_login_name = super::builtins_extra::builtin_user_real_login_name(vec![])
        .unwrap_or_else(|_| Value::string("unknown"));
    let system_configuration = super::builtins_extra::system_configuration_value();
    let system_configuration_options = super::builtins_extra::system_configuration_options_value();
    let system_configuration_features =
        super::builtins_extra::system_configuration_features_value();
    let operating_system_release = super::builtins_extra::operating_system_release_value();
    let defaults = [
        ("data-directory", Value::string(etc_dir.clone())),
        ("doc-directory", Value::string(etc_dir)),
        ("source-directory", Value::string(source_dir.clone())),
        ("installation-directory", Value::string(source_dir)),
        ("exec-directory", Value::NIL),
        ("configure-info-directory", Value::NIL),
        ("charset-map-path", Value::NIL),
        ("initial-environment", process_environment.clone()),
        ("process-environment", process_environment),
        ("path-separator", Value::string(path_separator)),
        ("file-name-coding-system", Value::NIL),
        ("default-file-name-coding-system", Value::NIL),
        ("set-auto-coding-function", Value::NIL),
        ("after-insert-file-functions", Value::NIL),
        ("write-region-annotate-functions", Value::NIL),
        ("write-region-post-annotation-function", Value::NIL),
        ("write-region-annotations-so-far", Value::NIL),
        ("inhibit-file-name-handlers", Value::NIL),
        ("inhibit-file-name-operation", Value::NIL),
        (
            "temporary-file-directory",
            Value::string(temporary_file_directory),
        ),
        ("create-lockfiles", Value::T),
        ("auto-save-list-file-name", Value::NIL),
        ("auto-save-list-file-prefix", Value::NIL),
        ("auto-save-visited-file-name", Value::NIL),
        ("auto-save-include-big-deletions", Value::NIL),
        ("shared-game-score-directory", Value::NIL),
        ("invocation-name", Value::NIL),
        ("invocation-directory", Value::NIL),
        ("system-messages-locale", Value::NIL),
        ("system-time-locale", Value::NIL),
        ("before-init-time", Value::NIL),
        ("after-init-time", Value::NIL),
        ("system-configuration", system_configuration),
        ("system-configuration-options", system_configuration_options),
        (
            "system-configuration-features",
            system_configuration_features,
        ),
        ("system-name", system_name),
        ("user-full-name", user_full_name),
        ("user-login-name", user_login_name),
        ("user-real-login-name", user_real_login_name),
        ("operating-system-release", operating_system_release),
        ("delayed-warnings-list", Value::NIL),
        ("default-text-properties", Value::NIL),
        ("char-property-alias-alist", Value::NIL),
        ("inhibit-point-motion-hooks", Value::T),
        (
            "text-property-default-nonsticky",
            Value::list(vec![
                Value::cons(Value::symbol("syntax-table"), Value::T),
                Value::cons(Value::symbol("display"), Value::T),
            ]),
        ),
    ];
    for (name, value) in defaults {
        if eval.obarray().symbol_value(name).is_none() {
            eval.set_variable(name, value);
        }
    }
    crate::emacs_core::xfaces::ensure_startup_compat_variables(eval);
}

fn value_symbol_name(value: Value) -> Option<String> {
    if let Some(name) = value.as_symbol_name() {
        return Some(name.to_owned());
    }
    value_quoted_symbol_name(value)
}

fn value_quoted_symbol_name(value: Value) -> Option<String> {
    if let Some(name) = value.as_symbol_name() {
        return Some(name.to_owned());
    }
    // Handle (quote sym) form: a two-element list where the first element is
    // the symbol `quote` and the second is the symbol to extract.
    let items = list_to_vec(&value)?;
    if items.len() == 2 {
        if items[0].is_symbol_named("quote") {
            return items[1].as_symbol_name().map(|s| s.to_owned());
        }
    }
    None
}

fn value_runtime_literal(value: Value) -> Option<Value> {
    // Values from the reader are already runtime values, except (quote X)
    // which evaluates to X (the quoted datum).
    if !value.is_cons() {
        return Some(value);
    }
    // (quote X) -> X
    value_quoted_symbol_name(value).map(|name| Value::symbol(&name))
}

#[derive(Default)]
struct LoaddefsSurfaceState {
    names: std::collections::BTreeSet<String>,
    autoload_args: Vec<Vec<Value>>,
    property_forms: Vec<Value>,
    property_keys: std::collections::BTreeSet<(String, String)>,
}

#[derive(Default)]
struct SourceFileSurfaceState {
    function_names: std::collections::BTreeSet<String>,
    variable_names: std::collections::BTreeSet<String>,
    face_names: std::collections::BTreeSet<String>,
    property_keys: std::collections::BTreeSet<(String, String)>,
    features: std::collections::BTreeSet<String>,
}

fn source_surface_insert_property(
    state: &mut SourceFileSurfaceState,
    name: impl Into<String>,
    prop: impl Into<String>,
) {
    state.property_keys.insert((name.into(), prop.into()));
}

fn collect_source_surface(form: Value, state: &mut SourceFileSurfaceState) {
    let Some(items) = list_to_vec(&form) else {
        return;
    };
    let Some(head) = items.first() else {
        return;
    };
    let Some(head_name) = head.as_symbol_name() else {
        return;
    };

    match head_name {
        "progn" | "eval-and-compile" => {
            for item in items.iter().skip(1) {
                collect_source_surface(*item, state);
            }
        }
        "defun" | "defmacro" | "defsubst" | "define-inline" => {
            if let Some(name) = items.get(1).and_then(|v| value_symbol_name(*v)) {
                state.function_names.insert(name);
            }
        }
        "defalias" => {
            if let Some(name) = items.get(1).and_then(|v| value_quoted_symbol_name(*v)) {
                state.function_names.insert(name);
            }
        }
        "defvar" | "defconst" | "defcustom" => {
            if let Some(name) = items.get(1).and_then(|v| value_symbol_name(*v)) {
                state.variable_names.insert(name);
            }
        }
        "defface" => {
            if let Some(name) = items.get(1).and_then(|v| value_symbol_name(*v)) {
                state.variable_names.insert(name.clone());
                state.face_names.insert(name);
            }
        }
        "put" | "function-put" | "define-symbol-prop" => {
            if let Some(name) = items.get(1).and_then(|v| value_quoted_symbol_name(*v))
                && let Some(prop) = items.get(2).and_then(|v| value_symbol_name(*v))
            {
                source_surface_insert_property(state, name, prop);
            }
        }
        "def-edebug-elem-spec" => {
            if let Some(name) = items.get(1).and_then(|v| value_quoted_symbol_name(*v)) {
                source_surface_insert_property(state, name, "edebug-form-spec");
            }
        }
        "provide" => {
            if let Some(feature) = items.get(1).and_then(|v| value_quoted_symbol_name(*v)) {
                state.features.insert(feature);
            }
        }
        "pcase-defmacro" => {
            if let Some(name) = items.get(1).and_then(|v| value_symbol_name(*v)) {
                let macroexpander = format!("{name}--pcase-macroexpander");
                state.function_names.insert(macroexpander.clone());
                source_surface_insert_property(state, &macroexpander, "edebug-form-spec");
                source_surface_insert_property(state, name, "pcase-macroexpander");
            }
        }
        "define-icon" => {
            if let Some(name) = items.get(1).and_then(|v| value_symbol_name(*v)) {
                source_surface_insert_property(state, name, "icon--properties");
            }
        }
        _ => {}
    }
}

fn collect_source_surface_from_paths(
    paths: &[PathBuf],
    error_context: &str,
) -> Result<SourceFileSurfaceState, EvalError> {
    let mut state = SourceFileSurfaceState::default();

    for path in paths {
        let bytes = fs::read(path).map_err(|err| EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "{error_context}: failed reading {}: {err}",
                path.display()
            ))],
            raw_data: None,
        })?;
        let source = decode_emacs_utf8(&bytes);
        let forms = crate::emacs_core::value_reader::read_all_with_source_multibyte(&source, true)
            .map_err(|err| EvalError::Signal {
                symbol: intern("error"),
                data: vec![Value::string(format!(
                    "{error_context}: failed parsing {}: {err}",
                    path.display()
                ))],
                raw_data: None,
            })?;

        for form in forms {
            collect_source_surface(form, &mut state);
        }
    }

    Ok(state)
}

fn collect_loaddefs_autoload_args(
    expr: Value,
    allowed_files: Option<&std::collections::BTreeSet<String>>,
    allowed_names: Option<&std::collections::BTreeSet<String>>,
    state: &mut LoaddefsSurfaceState,
) {
    let Some(items) = list_to_vec(&expr) else {
        return;
    };
    let Some(head) = items.first() else {
        return;
    };
    if !head.is_symbol_named("autoload") {
        return;
    }

    let Some(name) = items.get(1).and_then(|v| value_quoted_symbol_name(*v)) else {
        return;
    };
    let Some(file_value) = items.get(2).and_then(|v| value_runtime_literal(*v)) else {
        return;
    };
    let ValueKind::String = file_value.kind() else {
        return;
    };
    let file = load_string_text(&file_value).expect("checked string");
    if let Some(files) = allowed_files
        && !files.contains(&file)
    {
        return;
    };
    if let Some(names) = allowed_names
        && !names.contains(&name)
    {
        return;
    }

    state.names.insert(name.clone());
    let mut args = vec![Value::symbol(&name), file_value];
    for item in items.iter().skip(3).take(3) {
        let Some(value) = value_runtime_literal(*item) else {
            return;
        };
        args.push(value);
    }
    state.autoload_args.push(args);
}

fn collect_loaddefs_property_forms(
    expr: Value,
    names: &std::collections::BTreeSet<String>,
    state: &mut LoaddefsSurfaceState,
) {
    let Some(items) = list_to_vec(&expr) else {
        return;
    };
    let Some(head) = items.first() else {
        return;
    };
    let Some(head_name) = head.as_symbol_name() else {
        return;
    };
    if head_name != "function-put" && head_name != "put" {
        return;
    }
    let Some(name) = items.get(1).and_then(|v| value_quoted_symbol_name(*v)) else {
        return;
    };
    if names.contains(&name) {
        state.property_forms.push(expr);
        if let Some(prop) = items.get(2).and_then(|v| value_symbol_name(*v)) {
            state.property_keys.insert((name, prop));
        }
    }
}

fn collect_loaddefs_surface_from_paths(
    paths: &[PathBuf],
    allowed_files: Option<&std::collections::BTreeSet<String>>,
    allowed_names: Option<&std::collections::BTreeSet<String>>,
    error_context: &str,
) -> Result<LoaddefsSurfaceState, EvalError> {
    let mut state = LoaddefsSurfaceState::default();

    for path in paths {
        let bytes = fs::read(path).map_err(|err| EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "{error_context}: failed reading {}: {err}",
                path.display()
            ))],
            raw_data: None,
        })?;
        let source = decode_emacs_utf8(&bytes);
        let forms = crate::emacs_core::value_reader::read_all_with_source_multibyte(&source, true)
            .map_err(|err| EvalError::Signal {
                symbol: intern("error"),
                data: vec![Value::string(format!(
                    "{error_context}: failed parsing {}: {err}",
                    path.display()
                ))],
                raw_data: None,
            })?;

        for form in &forms {
            collect_loaddefs_autoload_args(*form, allowed_files, allowed_names, &mut state);
        }
        let property_names = allowed_names
            .cloned()
            .unwrap_or_else(|| state.names.clone());
        for form in &forms {
            collect_loaddefs_property_forms(*form, &property_names, &mut state);
        }
    }

    Ok(state)
}

fn compile_only_cl_loaddefs_state(project_root: &Path) -> Result<LoaddefsSurfaceState, EvalError> {
    collect_loaddefs_surface_from_paths(
        &[project_root.join("lisp/emacs-lisp/cl-loaddefs.el")],
        None,
        None,
        "bootstrap runtime cleanup",
    )
}

fn runtime_loaddefs_restore_state(project_root: &Path) -> Result<LoaddefsSurfaceState, EvalError> {
    let runtime_files = ["gv", "icons", "pcase"]
        .into_iter()
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    collect_loaddefs_surface_from_paths(
        &[project_root.join("lisp/ldefs-boot.el")],
        Some(&runtime_files),
        None,
        "bootstrap runtime cleanup",
    )
}

fn loaded_source_paths(eval: &mut super::eval::Context) -> Vec<PathBuf> {
    {
        let history = eval
            .obarray()
            .symbol_value("load-history")
            .cloned()
            .unwrap_or(Value::NIL);
        let mut paths = std::collections::BTreeSet::new();

        for entry in list_to_vec(&history).unwrap_or_default() {
            if !entry.is_cons() {
                continue;
            };
            let Some(path) = entry
                .cons_car()
                .is_string()
                .then(|| load_string_text(&entry.cons_car()).expect("checked string"))
            else {
                continue;
            };
            let path = PathBuf::from(path);
            if path.extension().is_some_and(|ext| ext == "el") {
                paths.insert(path);
            }
        }

        paths.into_iter().collect()
    }
}

fn is_compile_only_loaddefs_provider(path: &Path) -> bool {
    matches!(
        path.file_stem().and_then(|stem| stem.to_str()),
        Some(
            "cl-loaddefs"
                | "cl-preloaded"
                | "cl-lib"
                | "cl-macs"
                | "cl-seq"
                | "cl-extra"
                | "gv"
                | "icons"
        )
    )
}

fn runtime_loaded_source_restore_state(
    eval: &mut super::eval::Context,
    project_root: &Path,
    allowed_names: &std::collections::BTreeSet<String>,
) -> Result<LoaddefsSurfaceState, EvalError> {
    let paths = loaded_source_paths(eval)
        .into_iter()
        .filter(|path| path.starts_with(project_root))
        .filter(|path| !is_compile_only_loaddefs_provider(path))
        .collect::<Vec<_>>();
    collect_loaddefs_surface_from_paths(
        &paths,
        None,
        Some(allowed_names),
        "bootstrap runtime cleanup",
    )
}

fn runtime_source_bootstrap_surface_state(
    project_root: &Path,
) -> Result<SourceFileSurfaceState, EvalError> {
    collect_source_surface_from_paths(
        &[
            project_root.join("lisp/emacs-lisp/icons.el"),
            project_root.join("lisp/emacs-lisp/pcase.el"),
        ],
        "bootstrap runtime cleanup",
    )
}

pub(crate) fn apply_ldefs_boot_autoloads_for_names(
    eval: &mut super::eval::Context,
    names: &[&str],
) -> Result<(), EvalError> {
    let project_root = runtime_project_root();
    let ldefs_path = project_root.join("lisp/ldefs-boot.el");
    let source = fs::read_to_string(&ldefs_path).map_err(|err| EvalError::Signal {
        symbol: intern("error"),
        data: vec![Value::string(format!(
            "ldefs-boot autoload restore: failed reading {}: {err}",
            ldefs_path.display()
        ))],
        raw_data: None,
    })?;
    let forms = crate::emacs_core::value_reader::read_all_with_source_multibyte(&source, true)
        .map_err(|err| EvalError::Signal {
            symbol: intern("error"),
            data: vec![Value::string(format!(
                "ldefs-boot autoload restore: failed parsing {}: {err}",
                ldefs_path.display()
            ))],
            raw_data: None,
        })?;

    // Phase: parsed Lisp forms in `forms` (a Vec<Value>) live on
    // the malloc heap and are NOT reachable via conservative stack
    // scanning. The intervening `eval_generated_loaddefs_form`
    // calls below can trigger GC and would reclaim the cons cells
    // out from under us. Root every form for the duration of the
    // dispatch loop.
    let saved_temp_roots = eval.save_temp_roots();
    for form in &forms {
        eval.push_temp_root(*form);
    }

    let wanted = names
        .iter()
        .map(|name| (*name).to_string())
        .collect::<std::collections::BTreeSet<_>>();
    let mut property_forms: Vec<Value> = Vec::new();

    let result: Result<(), EvalError> = (|| {
        for form in &forms {
            let Some(items) = list_to_vec(form) else {
                continue;
            };
            let Some(head) = items.first() else {
                continue;
            };
            if head.is_symbol_named("autoload")
                && let Some(name) = items.get(1).and_then(|v| value_quoted_symbol_name(*v))
                && wanted.contains(&name)
            {
                eval_generated_loaddefs_form(eval, *form)?;
            }
        }

        for form in &forms {
            let Some(items) = list_to_vec(form) else {
                continue;
            };
            let Some(head) = items.first() else {
                continue;
            };
            let Some(head_name) = head.as_symbol_name() else {
                continue;
            };
            if head_name != "function-put" && head_name != "put" {
                continue;
            }
            let Some(name) = items.get(1).and_then(|v| value_quoted_symbol_name(*v)) else {
                continue;
            };
            if wanted.contains(&name) {
                property_forms.push(*form);
            }
        }

        for form in &property_forms {
            eval_generated_loaddefs_form(eval, *form)?;
        }
        Ok(())
    })();
    eval.restore_temp_roots(saved_temp_roots);
    result?;

    Ok(())
}

fn normalize_bootstrap_runtime_surface(
    eval: &mut super::eval::Context,
    project_root: &Path,
) -> Result<(), EvalError> {
    let compile_only_state = compile_only_cl_loaddefs_state(project_root).map_err(|e| {
        tracing::error!("compile_only_cl_loaddefs_state failed: {e:?}");
        e
    })?;
    let runtime_loaddefs_state = runtime_loaddefs_restore_state(project_root).map_err(|e| {
        tracing::error!("runtime_loaddefs_restore_state failed: {e:?}");
        e
    })?;
    let runtime_source_state =
        runtime_source_bootstrap_surface_state(project_root).map_err(|e| {
            tracing::error!("runtime_source_bootstrap_surface_state failed: {e:?}");
            e
        })?;
    let runtime_loaded_state =
        runtime_loaded_source_restore_state(eval, project_root, &compile_only_state.names)
            .map_err(|e| {
                tracing::error!("runtime_loaded_source_restore_state failed: {e:?}");
                e
            })?;
    let mut strip_names = compile_only_state.names.clone();
    strip_names.extend(runtime_loaddefs_state.names.iter().cloned());
    strip_names.extend(runtime_loaded_state.names.iter().cloned());

    let mut stripped_features = TRANSIENT_RUNTIME_FEATURES
        .iter()
        .map(|name| (*name).to_string())
        .collect::<std::collections::BTreeSet<_>>();
    stripped_features.extend(runtime_source_state.features.iter().cloned());
    for feature in &stripped_features {
        eval.remove_feature(feature);
    }
    // Keep the transient helper list authoritative even if the parsed source
    // surface misses a provide edge.
    for feature in TRANSIENT_RUNTIME_FEATURES {
        eval.remove_feature(feature);
    }
    // GNU's dumped runtime starts `gensym-counter` at 0.  Source bootstrap
    // expands many macros while loading core Lisp, so NeoVM must explicitly
    // drop that transient expansion count from the runtime surface.
    eval.set_variable("gensym-counter", Value::fixnum(0));

    for (name, prop) in compile_only_state
        .property_keys
        .iter()
        .chain(runtime_loaddefs_state.property_keys.iter())
        .chain(runtime_loaded_state.property_keys.iter())
        .chain(runtime_source_state.property_keys.iter())
    {
        let _ = super::builtins::builtin_put(
            eval,
            vec![Value::symbol(name), Value::symbol(prop), Value::NIL],
        );
    }

    for name in &strip_names {
        eval.obarray_mut().fmakunbound(&name);
        eval.autoloads.remove(name);
        let _ = super::builtins::builtin_put(
            eval,
            vec![
                Value::symbol(name),
                Value::symbol("autoload-macro"),
                Value::NIL,
            ],
        );
    }
    for name in &runtime_source_state.function_names {
        eval.obarray_mut().fmakunbound(name);
        eval.autoloads.remove(name);
        let _ = super::builtins::builtin_put(
            eval,
            vec![
                Value::symbol(name),
                Value::symbol("autoload-macro"),
                Value::NIL,
            ],
        );
    }
    for name in &runtime_source_state.variable_names {
        eval.obarray_mut().makunbound(name);
    }
    for name in &runtime_source_state.face_names {
        super::font::clear_created_lisp_face(name);
    }

    let autoload_entries = eval.autoloads.entries_snapshot();
    for (name, _) in &autoload_entries {
        if strip_names.contains(name) {
            eval.autoloads.remove(name);
            let _ = super::builtins::builtin_put(
                eval,
                vec![
                    Value::symbol(name),
                    Value::symbol("autoload-macro"),
                    Value::NIL,
                ],
            );
        }
    }

    // Phase: protect parsed-form Values across the autoload/eval
    // calls below. The Values stored in `runtime_loaded_state` and
    // `runtime_loaddefs_state` come from `value_reader::read_all`
    // which allocates Lisp cells on the tagged heap. Conservative
    // stack scanning only reaches stack-resident pointers, NOT
    // pointers stored inside Vec<Value> heap allocations, so
    // intervening GCs (triggered by builtin_autoload, builtin_put,
    // etc.) would reclaim the cons cells and leave the Values
    // dangling. Push them all into temp_roots for the duration of
    // the call.
    let saved_temp_roots = eval.save_temp_roots();
    for args in runtime_loaded_state
        .autoload_args
        .iter()
        .chain(runtime_loaddefs_state.autoload_args.iter())
    {
        for v in args {
            eval.push_temp_root(*v);
        }
    }
    for form in runtime_loaded_state
        .property_forms
        .iter()
        .chain(runtime_loaddefs_state.property_forms.iter())
    {
        eval.push_temp_root(*form);
    }

    let result: Result<(), EvalError> = (|| {
        for args in runtime_loaded_state
            .autoload_args
            .iter()
            .chain(runtime_loaddefs_state.autoload_args.iter())
        {
            super::autoload::builtin_autoload(eval, args.clone()).map_err(map_flow)?;
        }
        for form in runtime_loaded_state
            .property_forms
            .iter()
            .chain(runtime_loaddefs_state.property_forms.iter())
        {
            eval_runtime_form(eval, *form)?;
        }
        Ok(())
    })();
    eval.restore_temp_roots(saved_temp_roots);
    result?;

    Ok(())
}

fn bootstrap_runtime_window_system_symbol(eval: &mut super::eval::Context) -> Option<Value> {
    if eval.feature_present("neomacs")
        || eval.feature_present(super::display::gui_window_system_symbol())
    {
        Some(Value::symbol(super::display::gui_window_system_symbol()))
    } else if eval.feature_present("x") {
        Some(Value::symbol("x"))
    } else {
        None
    }
}

fn restore_cached_runtime_window_system_surface(eval: &mut super::eval::Context) {
    let Some(window_system) = bootstrap_runtime_window_system_symbol(eval) else {
        return;
    };

    let frame_id = if let Some(frame_id) = eval.frames.selected_frame().map(|frame| frame.id) {
        Some(frame_id)
    } else if let Some(frame_id) = eval.frames.frame_list().into_iter().next() {
        let _ = eval.frames.select_frame(frame_id);
        eval.sync_keyboard_terminal_owner();
        Some(frame_id)
    } else {
        None
    };

    if let Some(frame_id) = frame_id
        && let Some(frame) = eval.frames.get_mut(frame_id)
    {
        frame.set_window_system(Some(window_system));
        if frame.parameter("display-type").is_none() {
            frame.set_parameter(Value::symbol("display-type"), Value::symbol("color"));
        }
        if frame.parameter("background-mode").is_none() {
            frame.set_parameter(Value::symbol("background-mode"), Value::symbol("light"));
        }
    }

    eval.set_variable("window-system", window_system);
    eval.set_variable("initial-window-system", window_system);
}

fn clear_runtime_loader_state(eval: &mut super::eval::Context) {
    // These stacks only describe in-flight bootstrap loads/requires.
    // Letting them leak into the runtime surface makes later `require`
    // calls falsely look recursive/already-active.
    eval.require_stack.clear();
    eval.loads_in_progress.clear();
}

fn finalize_cached_bootstrap_eval(
    eval: &mut super::eval::Context,
    project_root: &Path,
) -> Result<(), EvalError> {
    // Register all builtins — pdump doesn't preserve live Rust entry-point
    // pointers on heap subr objects, so the callable surface must be rebuilt.
    // GNU Emacs loads the pdump as-is with no cleanup/normalization.
    // We only need to:
    // 1. Re-register builtins (pdump can't preserve Rust function pointers)
    // 2. Re-install BUFFER_OBJFWD forwarders (pdump load leaves the
    //    redirect as Plainval; mirror Context::new_inner here so
    //    default-directory etc. are Forwarded again).
    // 3. Reset thread-local caches
    // 4. Set path variables for the current runtime location
    super::builtins::init_builtins(eval);

    // Re-install BUFFER_OBJFWD forwarders to restore the Forwarded
    // redirect tag on per-buffer variables. `pdump::convert.rs`
    // leaves Forwarded symbols at Plainval/NIL (documented Phase 8
    // gap), so writes via set_variable would otherwise bypass the
    // per-buffer slot entirely. Mirrors the loop in
    // `Context::new_inner`.
    {
        use crate::buffer::buffer::BUFFER_SLOT_INFO;
        use crate::emacs_core::forward::alloc_buffer_objfwd;
        use crate::emacs_core::intern::intern;
        let obarray = eval.obarray_mut();
        for info in BUFFER_SLOT_INFO {
            // Phase 10D holdouts 3/4/5: skip internal-only slots
            // (syntax-table / category-table / case-table) — they
            // live in the BVAR slot block but have no Lisp variable
            // exposure, matching GNU.
            if !info.install_as_forwarder {
                continue;
            }
            let id = intern(info.name);
            let predicate = if info.predicate.is_empty() {
                intern("null")
            } else {
                intern(info.predicate)
            };
            let fwd = alloc_buffer_objfwd(
                info.offset as u16,
                info.local_flags_idx,
                predicate,
                info.default.to_value(),
            );
            obarray.install_buffer_objfwd(id, fwd);
        }
    }
    super::font::restore_created_faces_from_table(&eval.face_table.face_list());
    clear_runtime_loader_state(eval);
    ensure_startup_compat_variables(eval, project_root);
    restore_cached_runtime_window_system_surface(eval);

    let lisp_dir = project_root.join("lisp");
    eval.set_variable(
        "load-path",
        Value::list(bootstrap_load_path_entries(&lisp_dir)),
    );

    let etc_dir = project_root.join("etc");
    eval.set_variable(
        "data-directory",
        Value::string(format!("{}/", etc_dir.to_string_lossy())),
    );
    eval.set_variable(
        "source-directory",
        Value::string(format!("{}/", project_root.to_string_lossy())),
    );
    eval.set_variable(
        "installation-directory",
        Value::string(format!("{}/", project_root.to_string_lossy())),
    );

    // Mirror GNU `init_buffer` (`src/buffer.c:4923`): after loading
    // the dumped image, switch to `*scratch*` and reset its
    // `default-directory` to the runtime cwd captured at startup
    // (GNU `emacs_wd` / our `std::env::current_dir()`). GNU only
    // touches the scratch buffer and the (shared) minibuffer here —
    // every other buffer inherits on creation. Mirror that by
    // setting just the current buffer's slot via `set_variable`,
    // which routes through the FORWARDED dispatch.
    if let Ok(cwd) = std::env::current_dir() {
        let mut cwd_string = cwd.to_string_lossy().into_owned();
        if !cwd_string.ends_with('/') {
            cwd_string.push('/');
        }
        eval.set_variable("default-directory", Value::string(cwd_string));
    }

    eval.clear_top_level_eval_state();

    Ok(())
}

pub(crate) fn bootstrap_load_path_entries(lisp_dir: &Path) -> Vec<Value> {
    let mut load_path_entries = Vec::new();
    for sub in BOOTSTRAP_LOAD_PATH_SUBDIRS {
        let dir = if sub.is_empty() {
            lisp_dir.to_path_buf()
        } else {
            lisp_dir.join(sub)
        };
        if dir.is_dir() {
            load_path_entries.push(Value::string(dir.to_string_lossy().to_string()));
        }
    }
    load_path_entries
}

pub(crate) fn runtime_bootstrap_load_path() -> Vec<String> {
    let lisp_dir = runtime_project_root().join("lisp");
    bootstrap_load_path_entries(&lisp_dir)
        .into_iter()
        .filter_map(|value| {
            value
                .is_string()
                .then(|| load_string_text(&value).expect("checked string"))
        })
        .collect()
}

fn eval_startup_forms(eval: &mut super::eval::Context, forms_src: &str) -> Result<(), EvalError> {
    eval.eval_str(forms_src)?;
    Ok(())
}

/// Apply the runtime startup state that GNU Emacs has after the dumped image
/// is loaded and `normal-top-level` begins to run.
///
/// The dumped bootstrap image intentionally stops before normal interactive
/// startup.  Runtime callers that compare against `emacs --batch -Q` still
/// need the early startup buffer initialization that `startup.el` performs for
/// the `*scratch*` buffer.
fn sync_runtime_interpreted_closure_filter(eval: &mut super::eval::Context) {
    let closure_filter_sym = super::intern::intern("internal-make-interpreted-closure-function");
    let cconv_sym = super::intern::intern("cconv-make-interpreted-closure");
    let filter_fn = eval
        .obarray()
        .symbol_value_id(closure_filter_sym)
        .cloned()
        .and_then(|value| {
            if value.as_symbol_id() == Some(cconv_sym) {
                eval.obarray().symbol_function_id(cconv_sym).cloned()
            } else {
                None
            }
        });
    eval.set_interpreted_closure_filter_fn(filter_fn);
}

pub fn apply_runtime_startup_state(eval: &mut super::eval::Context) -> Result<(), EvalError> {
    let project_root = runtime_project_root();
    eval_startup_forms(
        eval,
        // Note: the closing paren count must balance the opens.
        // GNU loadup.el invokes `initial-major-mode` on `*scratch*`
        // when it's still in `fundamental-mode`; we replicate that
        // post-loadup hook here.
        r#"
          (if (get-buffer "*scratch*")
              (with-current-buffer "*scratch*"
                (if (eq major-mode 'fundamental-mode)
                    (funcall initial-major-mode))))
        "#,
    )?;

    // GNU's startup path reaches its post-startup surface through compiled
    // early Lisp. NeoVM executes the same files from source, which can
    // transiently reload compile-time helpers such as `gv`. Normalize the
    // runtime-visible autoload/feature surface again after those forms run.
    normalize_bootstrap_runtime_surface(eval, &project_root)?;

    sync_runtime_interpreted_closure_filter(eval);
    for feature in TRANSIENT_RUNTIME_FEATURES {
        eval.remove_feature(feature);
    }
    eval.clear_top_level_eval_state();

    Ok(())
}

fn install_bootstrap_x_window_system_vars(
    eval: &mut super::eval::Context,
) -> Result<(), EvalError> {
    let keysym_table = builtin_make_hash_table(vec![
        Value::keyword(":test"),
        Value::symbol("eql"),
        Value::keyword(":size"),
        Value::fixnum(900),
    ])
    .map_err(map_flow)?;
    eval.set_variable("x-keysym-table", keysym_table);
    eval.set_variable("x-selection-timeout", Value::fixnum(0));
    eval.set_variable("x-session-id", Value::NIL);
    eval.set_variable("x-session-previous-id", Value::NIL);
    for name in [
        "x-ctrl-keysym",
        "x-alt-keysym",
        "x-hyper-keysym",
        "x-meta-keysym",
        "x-super-keysym",
    ] {
        eval.set_variable(name, Value::NIL);
    }
    Ok(())
}

fn maybe_trace_bootstrap_step(message: impl AsRef<str>) {
    if std::env::var_os("NEOVM_TRACE_BOOTSTRAP_STEPS").is_some() {
        eprintln!("bootstrap-step: {}", message.as_ref());
    }
}

fn maybe_trace_bootstrap_macro_perf(eval: &super::eval::Context) {
    if let Some(summary) = eval.macro_perf_summary() {
        let gc_elapsed = eval
            .obarray()
            .symbol_value("gc-elapsed")
            .and_then(|value| value.as_number_f64())
            .unwrap_or(0.0);
        eprintln!(
            "bootstrap-macro-perf: {summary} | gc=gcs-done:{} elapsed:{:.3}s",
            eval.gc_count, gc_elapsed
        );
    }
}

pub fn create_bootstrap_evaluator() -> Result<super::eval::Context, EvalError> {
    create_bootstrap_evaluator_with_features(&[])
}

fn set_loadup_dump_mode(eval: &mut super::eval::Context, dump_mode: Option<LoadupDumpMode>) {
    match dump_mode {
        Some(mode) => eval.set_variable("dump-mode", Value::string(mode.as_gnu_string())),
        None => eval.set_variable("dump-mode", Value::NIL),
    }
}

fn apply_loadup_startup_surface(
    eval: &mut super::eval::Context,
    startup_surface: &LoadupStartupSurface,
) {
    let argv = startup_surface
        .command_line_args
        .iter()
        .cloned()
        .map(Value::string)
        .collect::<Vec<_>>();
    eval.set_variable("command-line-args", Value::list(argv));
    eval.set_variable("command-line-args-left", Value::NIL);
    eval.set_variable("command-line-processed", Value::NIL);
    eval.set_variable(
        "noninteractive",
        if startup_surface.noninteractive {
            Value::T
        } else {
            Value::NIL
        },
    );
}

pub fn create_bootstrap_evaluator_with_features(
    extra_features: &[&str],
) -> Result<super::eval::Context, EvalError> {
    create_bootstrap_evaluator_with_dump_mode(extra_features, None)
}

pub fn create_bootstrap_evaluator_with_dump_mode(
    extra_features: &[&str],
    dump_mode: Option<LoadupDumpMode>,
) -> Result<super::eval::Context, EvalError> {
    create_bootstrap_evaluator_with_startup_surface(extra_features, dump_mode, None)
}

pub fn create_bootstrap_evaluator_with_startup_surface(
    extra_features: &[&str],
    dump_mode: Option<LoadupDumpMode>,
    startup_surface: Option<&LoadupStartupSurface>,
) -> Result<super::eval::Context, EvalError> {
    // Discover the runtime root (contains lisp/ and etc/).
    let project_root = runtime_project_root();
    let lisp_dir = project_root.join("lisp");
    assert!(
        lisp_dir.is_dir(),
        "lisp/ directory not found at {}",
        lisp_dir.display()
    );
    stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
        maybe_trace_bootstrap_step("create_bootstrap_evaluator_with_features: enter");
        let mut eval = super::eval::Context::new();
        maybe_trace_bootstrap_step("create_bootstrap_evaluator_with_features: evaluator-new");
        let bootstrap_features = normalized_bootstrap_features(extra_features);
        for feature in &bootstrap_features {
            let _ = eval.provide_value(Value::symbol(&feature), None);
        }
        maybe_trace_bootstrap_step(format!(
            "create_bootstrap_evaluator_with_features: provided-features={bootstrap_features:?}"
        ));
        if bootstrap_features
            .iter()
            .any(|feature| feature == "x" || feature == "neomacs")
        {
            install_bootstrap_x_window_system_vars(&mut eval)?;
            maybe_trace_bootstrap_step(
                "create_bootstrap_evaluator_with_features: installed-x-window-system-vars",
            );
        }

        // Set up load-path with lisp/ and its subdirectories.
        eval.set_variable(
            "load-path",
            Value::list(bootstrap_load_path_entries(&lisp_dir)),
        );
        let bootstrap_frame_id = super::window_cmds::seed_batch_startup_frame_in_state(
            &mut eval.frames,
            &mut eval.buffers,
        );
        maybe_trace_bootstrap_step(format!(
            "create_bootstrap_evaluator_with_features: seeded-batch-bootstrap-frame={bootstrap_frame_id:?}"
        ));
        if let Some(startup_surface) = startup_surface {
            apply_loadup_startup_surface(&mut eval, startup_surface);
            maybe_trace_bootstrap_step(
                "create_bootstrap_evaluator_with_features: applied-loadup-startup-surface",
            );
        }
        // GNU loadup.el uses a string-valued dump-mode (`pdump` /
        // `pbootstrap`) to decide whether Lisp should call
        // `dump-emacs-portable`. Keep ordinary cached bootstrap on nil, but
        // let explicit temacs-style flows seed the real GNU value here.
        set_loadup_dump_mode(&mut eval, dump_mode);
        eval.set_variable("purify-flag", Value::NIL);
        // NeoVM counts depth more aggressively than GNU (see eval.rs comment).
        eval.set_variable("max-lisp-eval-depth", Value::fixnum(2400));
        eval.set_variable("inhibit-load-charset-map", Value::T);
        // data-directory: directory of machine-independent data files (etc/)
        let etc_dir = project_root.join("etc");
        eval.set_variable(
            "data-directory",
            Value::string(format!("{}/", etc_dir.to_string_lossy())),
        );
        // source-directory: top-level source tree
        eval.set_variable(
            "source-directory",
            Value::string(format!("{}/", project_root.to_string_lossy())),
        );
        eval.set_variable(
            "installation-directory",
            Value::string(format!("{}/", project_root.to_string_lossy())),
        );

        // exec-path: list of dirs from PATH env var (C: callproc.c init_callproc_1)
        let path_dirs: Vec<Value> = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .filter(|s| !s.is_empty())
            .map(|s| Value::string(s.to_string()))
            .collect();
        eval.set_variable("exec-path", Value::list(path_dirs));
        eval.set_variable("exec-suffixes", Value::NIL);
        eval.set_variable("exec-directory", Value::NIL);

        // shell-file-name: GNU callproc.c:2041 — $SHELL or /bin/sh
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        eval.set_variable("shell-file-name", Value::string(shell));
        // shell-command-switch: GNU simple.el — defaults to "-c"
        eval.set_variable("shell-command-switch", Value::string("-c"));

        // menu-bar-final-items: list of menu-bar items to put at end (C: xmenu.c)
        eval.set_variable(
            "menu-bar-final-items",
            Value::list(vec![Value::symbol("help-menu")]),
        );

        // glyphless-char-display: char-table for glyphless character display
        // (C: xdisp.c syms_of_xdisp). First register extra slots, then create.
        {
            let stubs = [
                "(put 'glyphless-char-display 'char-table-extra-slots 1)",
                "(setq glyphless-char-display (make-char-table 'glyphless-char-display nil))",
                "(set-char-table-extra-slot glyphless-char-display 0 'empty-box)",
            ];
            for stub in &stubs {
                let _ = eval.eval_str(stub);
            }
        }

        // Load loadup.el — this does everything GNU's loadup.el does:
        // loads all core .el/.elc files, handles platform conditionals,
        // manages eager expansion, etc.
        let loadup_path = lisp_dir.join("loadup.el");
        tracing::info!("Loading loadup.el from {}", loadup_path.display());
        let _bootstrap_ldefs_boot_preference = BootstrapLdefsBootPreferenceGuard::enable();
        match load_file(&mut eval, &loadup_path) {
            Ok(_) => tracing::info!("loadup.el completed successfully"),
            Err(e) => {
                let rendered = format_eval_error_in_state(&eval, &e);
                tracing::error!("loadup.el failed: {rendered}");
                maybe_trace_bootstrap_step(format!(
                    "create_bootstrap_evaluator_with_features: loadup-failed={rendered}"
                ));
                match &e {
                    EvalError::Signal { symbol, .. } if resolve_sym(*symbol) == "kill-emacs" => {
                        tracing::info!("loadup.el completed (kill-emacs after dump)");
                    }
                    _ => {
                        return Err(e);
                    }
                }
            }
        }
        maybe_trace_bootstrap_macro_perf(&eval);

        if dump_mode.is_some() && eval.shutdown_request.is_some() {
            return Ok(eval);
        }

        // If loadup.el set a shutdown request (via kill-emacs at the end
        // of the dump flow), clear it so the caller gets a usable evaluator.
        eval.shutdown_request = None;

        // For neomacs builds, load term/neo-win after loadup.el completes.
        // loadup.el handles `(featurep 'x)` which loads term/x-win, but
        // NeoVM's neomacs feature needs term/neo-win instead/additionally.
        if bootstrap_features.iter().any(|f| f == "neomacs") && !eval.feature_present("x") {
            let load_path = get_load_path(&eval.obarray());
            for neo_file in &["term/common-win", "term/neo-win"] {
                if let Some(path) = find_file_in_load_path(neo_file, &load_path) {
                    tracing::info!("LOADING (neomacs): {neo_file} ...");
                    if let Err(e) = load_file(&mut eval, &path) {
                        tracing::error!("FAIL (neomacs): {neo_file} => {e:?}");
                        return Err(e);
                    }
                }
            }
        }

        tracing::info!("\n=== LOADUP BOOTSTRAP COMPLETE ===");

        // Modern Emacs (27+) defaults to lexical-binding: t for *scratch*
        // and interactive evaluation. Match this for oracle test parity.
        eval.set_lexical_binding(true);
        eval.clear_top_level_eval_state();
        let _ = eval.frames.delete_frame(bootstrap_frame_id);
        clear_runtime_loader_state(&mut eval);

        Ok(eval)
    })
}

/// Create a bootstrap evaluator, using a pdump cache file if available.
///
/// On first call, performs the full bootstrap and saves the result to a
/// `.pdump` file next to the `lisp/` directory. On subsequent calls,
/// loads from the dump file (~10-50ms vs 3-5s bootstrap).
///
/// The dump file is invalidated by the bootstrap image schema version and
/// by a fingerprint of the runtime root's Lisp sources. Set
/// `NEOVM_DISABLE_PDUMP=1` to force fresh bootstrap.
pub fn create_bootstrap_evaluator_cached() -> Result<super::eval::Context, EvalError> {
    create_bootstrap_evaluator_cached_with_features(&[])
}

pub fn create_runtime_startup_evaluator() -> Result<super::eval::Context, EvalError> {
    create_runtime_startup_evaluator_with_features(&[])
}

pub(crate) fn create_runtime_startup_evaluator_at_path(
    extra_features: &[&str],
    dump_path: &Path,
) -> Result<super::eval::Context, EvalError> {
    let mut eval = create_bootstrap_evaluator_cached_at_path(extra_features, dump_path)?;
    apply_runtime_startup_state(&mut eval)?;
    maybe_run_after_pdump_load_hook(&mut eval);

    Ok(eval)
}

pub fn create_runtime_startup_evaluator_with_features(
    extra_features: &[&str],
) -> Result<super::eval::Context, EvalError> {
    let project_root = runtime_project_root();
    let dump_path = bootstrap_dump_path(&project_root, extra_features);
    create_runtime_startup_evaluator_at_path(extra_features, &dump_path)
}

pub fn create_runtime_startup_evaluator_cached() -> Result<super::eval::Context, EvalError> {
    create_runtime_startup_evaluator()
}

pub(crate) fn create_runtime_startup_evaluator_cached_at_path(
    extra_features: &[&str],
    dump_path: &Path,
) -> Result<super::eval::Context, EvalError> {
    create_runtime_startup_evaluator_at_path(extra_features, dump_path)
}

pub fn create_runtime_startup_evaluator_cached_with_features(
    extra_features: &[&str],
) -> Result<super::eval::Context, EvalError> {
    create_runtime_startup_evaluator_with_features(extra_features)
}

pub fn load_runtime_image_with_features(
    role: RuntimeImageRole,
    extra_features: &[&str],
    dump_path: Option<&Path>,
) -> Result<super::eval::Context, EvalError> {
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok().or(Some(path)))
        .unwrap_or_else(|| {
            if dump_path.is_some() {
                PathBuf::from(role.image_file_name())
            } else {
                default_runtime_image_path(role)
            }
        });
    load_runtime_image_with_features_for_executable(role, extra_features, dump_path, &executable)
}

pub(crate) fn load_runtime_image_with_features_for_executable(
    role: RuntimeImageRole,
    extra_features: &[&str],
    dump_path: Option<&Path>,
    executable: &Path,
) -> Result<super::eval::Context, EvalError> {
    use super::pdump;

    let project_root = runtime_project_root();
    let candidates = dump_path
        .map(|path| vec![path.to_path_buf()])
        .unwrap_or_else(|| runtime_image_candidate_paths_for_executable(executable, role));
    let mut eval = {
        let mut last_error = None;
        let mut loaded = None;
        for (index, candidate) in candidates.iter().enumerate() {
            match pdump::load_from_dump(candidate) {
                Ok(eval) => {
                    loaded = Some(eval);
                    break;
                }
                Err(pdump::DumpError::Io(err))
                    if err.kind() == std::io::ErrorKind::NotFound
                        && index + 1 < candidates.len() =>
                {
                    tracing::info!(
                        "pdump: runtime image {} not found, trying next candidate",
                        candidate.display()
                    );
                }
                Err(err) => {
                    last_error = Some(runtime_image_load_error(role, candidate, err));
                    break;
                }
            }
        }
        match (loaded, last_error) {
            (Some(eval), _) => eval,
            (None, Some(err)) => return Err(err),
            (None, None) => {
                return Err(runtime_image_load_error(
                    role,
                    &default_fingerprinted_runtime_image_path(role),
                    pdump::DumpError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "runtime image not found",
                    )),
                ));
            }
        }
    };

    if !extra_features.is_empty() {
        let bootstrap_features = normalized_bootstrap_features(extra_features);
        for feature in &bootstrap_features {
            let _ = eval.provide_value(Value::symbol(feature), None);
        }
    }

    finalize_cached_bootstrap_eval(&mut eval, &project_root).map_err(|e| {
        tracing::error!("finalize_cached_bootstrap_eval failed: {e:?}");
        e
    })?;

    Ok(eval)
}

fn runtime_image_load_error(
    role: RuntimeImageRole,
    dump_path: &Path,
    err: super::pdump::DumpError,
) -> EvalError {
    let image_kind = match role {
        RuntimeImageRole::Bootstrap => "bootstrap",
        RuntimeImageRole::Final => "final",
    };
    let message = format!(
        "failed to load {image_kind} image {}: {err}",
        dump_path.display()
    );
    tracing::error!("{message}");
    let payload = Value::symbol(intern(&message));
    EvalError::Signal {
        symbol: intern("error"),
        data: vec![payload],
        raw_data: Some(payload),
    }
}

pub fn maybe_run_after_pdump_load_hook(eval: &mut super::eval::Context) -> bool {
    if super::pdump::take_after_pdump_load_hook_pending(eval) {
        super::pdump::runtime::run_after_pdump_load_hook(eval);
        return true;
    }
    false
}

pub fn create_bootstrap_evaluator_cached_with_features(
    extra_features: &[&str],
) -> Result<super::eval::Context, EvalError> {
    let project_root = runtime_project_root();
    let dump_path = bootstrap_dump_path(&project_root, extra_features);
    create_bootstrap_evaluator_cached_at_path(extra_features, &dump_path)
}

pub(crate) fn create_bootstrap_evaluator_cached_at_path(
    extra_features: &[&str],
    dump_path: &Path,
) -> Result<super::eval::Context, EvalError> {
    use super::pdump;

    fn finalize_or_log(
        eval: &mut super::eval::Context,
        project_root: &Path,
        context: &str,
    ) -> Result<(), EvalError> {
        match finalize_cached_bootstrap_eval(eval, project_root) {
            Ok(()) => Ok(()),
            Err(err) => {
                let rendered = format_eval_error_in_state(eval, &err);
                tracing::error!("{context}: {rendered}");
                Err(err)
            }
        }
    }

    let project_root = runtime_project_root();
    tracing::info!("pdump: bootstrap cache candidate {}", dump_path.display());

    // Allow disabling pdump via env var
    if std::env::var("NEOVM_DISABLE_PDUMP").unwrap_or_default() == "1" {
        let mut eval = create_bootstrap_evaluator_with_features(extra_features)?;
        finalize_or_log(&mut eval, &project_root, "pdump disabled finalize failed")?;
        return Ok(eval);
    }

    // Try loading from dump first
    if dump_path.exists() {
        let start = std::time::Instant::now();
        match pdump::load_from_dump(dump_path) {
            Ok(mut eval) => {
                tracing::info!(
                    "pdump: loaded bootstrap state from {} ({:.2?})",
                    dump_path.display(),
                    start.elapsed()
                );
                finalize_or_log(&mut eval, &project_root, "pdump finalize failed")?;

                return Ok(eval);
            }
            Err(e) => {
                tracing::warn!("pdump: load failed ({e}), falling back to full bootstrap");
            }
        }
    } else {
        tracing::info!("pdump: bootstrap cache miss at {}", dump_path.display());
    }

    let _write_lock = match BootstrapCacheWriteLock::acquire(&bootstrap_dump_lock_path(dump_path)) {
        Ok(lock) => Some(lock),
        Err(err) => {
            tracing::warn!("pdump: cache lock unavailable ({err}), bootstrapping without cache");
            None
        }
    };

    if _write_lock.is_none() {
        let mut eval = create_bootstrap_evaluator_with_features(extra_features)?;
        ensure_startup_compat_variables(&mut eval, &project_root);
        finalize_or_log(
            &mut eval,
            &project_root,
            "pdump lockless fallback finalize failed",
        )?;
        return Ok(eval);
    }

    if dump_path.exists() {
        let start = std::time::Instant::now();
        match pdump::load_from_dump(dump_path) {
            Ok(mut eval) => {
                tracing::info!(
                    "pdump: loaded bootstrap state from {} after lock ({:.2?})",
                    dump_path.display(),
                    start.elapsed()
                );
                finalize_or_log(&mut eval, &project_root, "pdump finalize after lock failed")?;
                return Ok(eval);
            }
            Err(e) => {
                tracing::warn!("pdump: load after lock failed ({e}), rebuilding bootstrap cache");
            }
        }
    }

    // Full bootstrap
    let start = std::time::Instant::now();
    let mut eval = create_bootstrap_evaluator_with_features(extra_features)?;
    ensure_startup_compat_variables(&mut eval, &project_root);
    let bootstrap_time = start.elapsed();

    // Save dump for next time.
    if let Some(parent) = dump_path.parent()
        && !parent.exists()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    let dump_start = std::time::Instant::now();
    match pdump::dump_to_file(&eval, dump_path) {
        Ok(()) => {
            tracing::info!(
                "pdump: saved bootstrap state to {} ({:.2?}, bootstrap took {:.2?})",
                dump_path.display(),
                dump_start.elapsed(),
                bootstrap_time,
            );
            let reload_start = std::time::Instant::now();
            match pdump::load_from_dump(dump_path) {
                Ok(mut loaded) => {
                    finalize_or_log(
                        &mut loaded,
                        &project_root,
                        "pdump fresh reload finalize failed",
                    )?;
                    tracing::info!(
                        "pdump: reloaded freshly written bootstrap state from {} ({:.2?})",
                        dump_path.display(),
                        reload_start.elapsed()
                    );
                    return Ok(loaded);
                }
                Err(e) => {
                    tracing::warn!(
                        "pdump: failed to reload freshly written bootstrap image ({e}), using in-memory bootstrap"
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!("pdump: failed to save ({e}), will bootstrap again next time");
        }
    }

    finalize_or_log(
        &mut eval,
        &project_root,
        "pdump in-memory fallback finalize failed",
    )?;
    Ok(eval)
}

/// Expand `~/` prefix to the HOME directory, matching GNU Emacs's
/// `Fsubstitute_in_file_name` (lread.c:1155).
pub(crate) fn expand_tilde(path: &str) -> String {
    if path.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}{}", home.to_string_lossy(), &path[1..]);
        }
    }
    path.to_string()
}

#[cfg(test)]
#[path = "load_test.rs"]
mod tests;
